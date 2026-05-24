// kernel/src/elf.rs
//
// Native ELF parsing + segment mapping (B3 Step 4). The mechanism behind the
// `ElfLoader` Frame system: `ElfLoader` owns the *phase sequence* (read →
// validate → map → stack → done, with a `$Failed` cleanup funnel); this module
// owns the *bytes* — parsing the ELF64 header + program headers and mapping
// PT_LOAD segments into the target address space. Same "Frame owns lifecycle,
// native owns mechanism" split as serial / vm.
//
// Single load at a time (one process at B3): the input + parse results + the
// list of mapped pages live in one global, set by `prepare()` before the
// `ElfLoader` is constructed. `kernel-tests` supplies a host double of this
// module so the loader's phase logic is testable without real paging.
//
// ELF64 layout we read (little-endian, the only encoding we accept):
//   header:  e_type@16(u16) e_machine@18(u16) e_entry@24(u64)
//            e_phoff@32(u64) e_phentsize@54(u16) e_phnum@56(u16)
//   phdr:    p_type@0(u32) p_flags@4(u32) p_offset@8(u64) p_vaddr@16(u64)
//            p_filesz@32(u64) p_memsz@40(u64)

use crate::{frames, paging};

const PAGE: u64 = 4096;
const USER_STACK_VA: u64 = 0x0000_0000_2000_0000; // proven-free user VA
                                                  // User stack size in pages. One page (4 KiB) sufficed through B10, but the
                                                  // on-device C compiler (tcc, B11-3) is a recursive-descent parser with large
                                                  // local buffers and easily blows a 4 KiB stack. 32 pages (128 KiB) gives it
                                                  // generous headroom; the stack region [VA, VA + 32*PAGE) sits far below the
                                                  // brk heap (0x3000_0000) and above the program image (0x1000_0000), so it
                                                  // can't collide with either. Uniform for every program — small programs simply
                                                  // don't touch the extra pages (they're mapped lazily-zeroed at load).
const USER_STACK_PAGES: u64 = 32;
// Pages we can roll back on a *failed* load (the $Failed funnel calls
// `cleanup`). Sized to cover the largest program we load (tcc: ~104 PT_LOAD
// pages) plus its stack, so a partial load is fully reclaimed. The *success*
// path doesn't depend on this — `paging::free_address_space` walks the page
// tables on exit/reap and frees everything regardless.
const MAX_TRACKED: usize = 512;

/// The parsed ELF header fields that flow down the `ElfLoader` phase pipeline
/// as an enter parameter (`$ReadingHeader → $ValidatingHeader → $MappingSegments`).
/// Frame threads this descriptor; the *bytes*, target `pml4`, and the mapped-
/// pages rollback list stay in the `ELF` global (accumulating native payload).
#[derive(Clone, Copy, Default)]
pub struct ElfHeader {
    pub phoff: u64,
    pub phentsize: u16,
    pub phnum: u16,
}

struct ElfState {
    bytes: &'static [u8],
    pml4: u64,
    entry: u64, // kept for the entry_va() query, read after the load settles
    stack_top: u64,
    mapped: [(u64, u64); MAX_TRACKED], // (virt, frame_phys)
    mapped_n: usize,
}

static mut ELF: ElfState = ElfState {
    bytes: &[],
    pml4: 0,
    entry: 0,
    stack_top: 0,
    mapped: [(0, 0); MAX_TRACKED],
    mapped_n: 0,
};

fn elf() -> &'static mut ElfState {
    // Bind the raw pointer first, then deref: avoids both `static_mut_refs`
    // (no `&mut ELF`) and `clippy::deref_addrof` (no `*(&raw mut ELF)`).
    let p = &raw mut ELF;
    unsafe { &mut *p }
}

// --- little-endian readers (bounds-checked) --------------------------------

fn rd_u16(b: &[u8], off: usize) -> Option<u16> {
    let s = b.get(off..off + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
    let s = b.get(off..off + 8)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}

// --- public API (called from the ElfLoader handlers) -----------------------

/// Stash the ELF image + target page table for the load that follows.
pub fn prepare(bytes: &'static [u8], pml4: u64) {
    let e = elf();
    e.bytes = bytes;
    e.pml4 = pml4;
    e.entry = 0;
    e.stack_top = 0;
    e.mapped_n = 0;
}

/// Phase 1: read the ELF header fields, returning them as the descriptor that
/// flows down the pipeline. None if the image is too short. (`entry` is also
/// stashed in the global for the post-load `entry_va()` query.)
pub fn read_header() -> Option<ElfHeader> {
    let e = elf();
    let b = e.bytes;
    let (Some(entry), Some(phoff), Some(phentsize), Some(phnum)) =
        (rd_u64(b, 24), rd_u64(b, 32), rd_u16(b, 54), rd_u16(b, 56))
    else {
        return None;
    };
    e.entry = entry; // stashed for the post-load entry_va() query
    Some(ElfHeader {
        phoff,
        phentsize,
        phnum,
    })
}

/// Phase 2: validate magic + that this is a 64-bit little-endian x86-64
/// executable, and that the program-header table (from the threaded header) is
/// in bounds.
pub fn validate_header(hdr: ElfHeader) -> bool {
    let e = elf();
    let b = e.bytes;
    if b.len() < 64 || &b[0..4] != b"\x7fELF" {
        return false;
    }
    if b[4] != 2 || b[5] != 1 {
        return false; // EI_CLASS=ELFCLASS64, EI_DATA=ELFDATA2LSB
    }
    match (rd_u16(b, 16), rd_u16(b, 18)) {
        (Some(2), Some(0x3E)) => {} // ET_EXEC, EM_X86_64
        _ => return false,
    }
    let ph_end = hdr.phoff + (hdr.phnum as u64) * (hdr.phentsize as u64);
    (ph_end as usize) <= b.len() && hdr.phentsize >= 56
}

/// Phase 3: map every PT_LOAD segment at its p_vaddr, walking the program
/// headers named by the threaded descriptor. False on allocation failure (the
/// loader then transitions to $Failed, which calls `cleanup`).
pub fn map_segments(hdr: ElfHeader) -> bool {
    let e = elf();
    for i in 0..hdr.phnum as u64 {
        let off = (hdr.phoff + i * hdr.phentsize as u64) as usize;
        let b = e.bytes;
        let (
            Some(p_type),
            Some(p_flags),
            Some(p_offset),
            Some(p_vaddr),
            Some(p_filesz),
            Some(p_memsz),
        ) = (
            rd_u32(b, off),
            rd_u32(b, off + 4),
            rd_u64(b, off + 8),
            rd_u64(b, off + 16),
            rd_u64(b, off + 32),
            rd_u64(b, off + 40),
        )
        else {
            return false;
        };
        if p_type != 1 {
            continue; // not PT_LOAD
        }
        let mut flags = paging::USER;
        if p_flags & 2 != 0 {
            flags |= paging::WRITABLE; // PF_W
        }
        if !map_one_segment(p_offset, p_vaddr, p_filesz, p_memsz, flags) {
            return false;
        }
    }
    true
}

fn map_one_segment(p_offset: u64, p_vaddr: u64, p_filesz: u64, p_memsz: u64, flags: u64) -> bool {
    let seg_end = p_vaddr + p_memsz;
    // Align the *page* down, but keep the segment's sub-page offset: ELF only
    // guarantees `p_offset ≡ p_vaddr (mod PAGE)`, NOT that p_vaddr is page
    // aligned. (Our own linker emits aligned segments, but tcc's output does
    // not — its data segment starts mid-page, e.g. 0x…bc8.) For each page we
    // zero it (covering .bss / the bytes outside file content) and copy only
    // the file bytes that overlap this page, placed at their correct in-page
    // offset. Getting this wrong shifts .data/.got by the sub-page delta —
    // every absolute pointer in the image then points into garbage.
    let file_end = p_vaddr + p_filesz; // VA one past the last file-backed byte
    let mut va = p_vaddr & !(PAGE - 1); // page containing the segment start
    while va < seg_end {
        let Some(frame) = frames::alloc_frame() else {
            return false;
        };
        unsafe {
            let dst = frames::phys_to_virt(frame);
            core::ptr::write_bytes(dst, 0, PAGE as usize); // zero (covers .bss)
                                                           // Overlap of this page [va, va+PAGE) with file content [p_vaddr, file_end).
            let content_start = core::cmp::max(va, p_vaddr);
            let content_end = core::cmp::min(va + PAGE, file_end);
            if content_start < content_end {
                let dst_in_page = (content_start - va) as usize;
                let src_off = (p_offset + (content_start - p_vaddr)) as usize;
                let n = (content_end - content_start) as usize;
                let e = elf();
                if let Some(src) = e.bytes.get(src_off..src_off + n) {
                    core::ptr::copy_nonoverlapping(src.as_ptr(), dst.add(dst_in_page), n);
                } else {
                    frames::free_frame(frame);
                    return false; // file offsets out of range — corrupt
                }
            }
            paging::map_in(elf().pml4, va, frame, flags);
        }
        track(va, frame);
        va += PAGE;
    }
    true
}

/// Phase 4: allocate + map the user stack (`USER_STACK_PAGES` pages). Returns
/// false on OOM (after rolling back via the tracked-page list on the $Failed
/// path). The stack occupies `[USER_STACK_VA, USER_STACK_VA + N*PAGE)` and grows
/// down from the top; every page is mapped up front (no demand paging here).
pub fn build_stack() -> bool {
    let e = elf();
    for i in 0..USER_STACK_PAGES {
        let va = USER_STACK_VA + i * PAGE;
        let Some(frame) = frames::alloc_frame() else {
            return false;
        };
        unsafe {
            core::ptr::write_bytes(frames::phys_to_virt(frame), 0, PAGE as usize);
            paging::map_in(e.pml4, va, frame, paging::USER | paging::WRITABLE);
        }
        track(va, frame);
    }
    e.stack_top = USER_STACK_VA + USER_STACK_PAGES * PAGE - 16; // 16-aligned-ish top
    true
}

/// The program entry VA (valid once `read_header` succeeded).
pub fn entry_va() -> u64 {
    elf().entry
}

/// The top of the mapped user stack (valid once `build_stack` succeeded).
pub fn stack_top() -> u64 {
    elf().stack_top
}

/// Roll back every mapped page: unmap + free. Used by `$Failed` cleanup and by
/// the demo's teardown after the process exits.
pub fn cleanup() {
    let e = elf();
    for i in 0..e.mapped_n {
        let (va, frame) = e.mapped[i];
        unsafe {
            paging::unmap(va);
        }
        frames::free_frame(frame);
    }
    e.mapped_n = 0;
}

fn track(va: u64, frame: u64) {
    let e = elf();
    if e.mapped_n < MAX_TRACKED {
        e.mapped[e.mapped_n] = (va, frame);
        e.mapped_n += 1;
    }
}

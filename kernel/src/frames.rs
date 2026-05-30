// kernel/src/frames.rs
//
// Physical frame allocator (B2 Step 1; arch-agnostic data layer as of
// B-HAL.4.1). Pure native — bookkeeping over raw physical memory, the
// territory where a state machine adds ceremony, not clarity.
//
// A bitmap over 4 KiB physical frames: bit f set ⇒ frame f is in use. We
// mark every frame used, then free the frames inside the *usable* regions
// the caller hands us. Each arch's boot path supplies its own memory map:
//   - x86_64 boot: Limine's memory-map response + HHDM offset (see `init`)
//   - aarch64 boot: the FDT `/memory` node, minus the kernel image + DTB
//                   (see `arch/aarch64/boot.rs`); HHDM offset = 0 because
//                   the kernel runs through an identity map (B-HAL.3.4)
// The bookkeeping itself (`init_from_regions`, alloc/free/free_count,
// phys_to_virt) is the same code on both ISAs.
//
// Single-threaded use at B2 (init + the page-fault path, which runs with
// interrupts off). A lock is added when a preemptible context allocates
// (B3).

#[cfg(target_arch = "x86_64")]
use limine::memory_map::EntryType;
#[cfg(target_arch = "x86_64")]
use limine::request::{HhdmRequest, MemoryMapRequest};

#[cfg(target_arch = "x86_64")]
#[used]
#[link_section = ".requests"]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[cfg(target_arch = "x86_64")]
#[used]
#[link_section = ".requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

/// Physical frame size.
pub const FRAME_SIZE: u64 = 4096;

/// Frames the bitmap can track. 1 Mi frames = 4 GiB of physical address
/// space — comfortable for QEMU; frames above this are ignored.
const MAX_FRAMES: usize = 1 << 20;
const BITMAP_BYTES: usize = MAX_FRAMES / 8;

// 1 = used. Start all-used; init() frees the USABLE frames.
static mut BITMAP: [u8; BITMAP_BYTES] = [0xFF; BITMAP_BYTES];
static mut FREE_COUNT: usize = 0;
static mut HHDM_OFFSET: u64 = 0;

fn bitmap() -> *mut u8 {
    (&raw mut BITMAP) as *mut u8
}

fn is_used(frame: usize) -> bool {
    unsafe { *bitmap().add(frame / 8) & (1 << (frame % 8)) != 0 }
}

fn mark_used(frame: usize) {
    unsafe {
        let byte = bitmap().add(frame / 8);
        *byte |= 1 << (frame % 8);
    }
}

fn mark_free(frame: usize) {
    unsafe {
        let byte = bitmap().add(frame / 8);
        *byte &= !(1 << (frame % 8));
    }
}

fn adjust_free(delta: isize) {
    unsafe {
        let p = &raw mut FREE_COUNT;
        let v = p.read() as isize + delta;
        p.write(v as usize);
    }
}

/// Initialize the allocator from a list of usable `(base, length)` regions and
/// the HHDM offset. Each region is freed frame-by-frame into the bitmap; the
/// null frame is never handed out. Arch-agnostic — both the Limine path (x86,
/// see [`init`]) and the FDT path (aarch64, in `arch/aarch64/boot.rs`) reach
/// the allocator through here. Must run once before any alloc.
pub fn init_from_regions(usable: &[(u64, u64)], hhdm: u64) {
    unsafe {
        (&raw mut HHDM_OFFSET).write(hhdm);
    }

    for &(base, length) in usable {
        let start = (base / FRAME_SIZE) as usize;
        let end = ((base + length) / FRAME_SIZE) as usize;
        for frame in start..end {
            if frame < MAX_FRAMES && is_used(frame) {
                mark_free(frame);
                adjust_free(1);
            }
        }
    }

    // Never hand out the null frame.
    if !is_used(0) {
        mark_used(0);
        adjust_free(-1);
    }
}

/// x86 init: read Limine's memory map + HHDM and feed them into
/// [`init_from_regions`]. The x86 boot HSM's `$InitMemory` phase calls this.
#[cfg(target_arch = "x86_64")]
pub fn init() {
    let hhdm = HHDM_REQUEST
        .get_response()
        .expect("limine HHDM response missing")
        .offset();
    let mmap = MEMORY_MAP_REQUEST
        .get_response()
        .expect("limine memory map response missing");

    // Collect usable entries into a small stack buffer. The Limine map has
    // far fewer than 64 entries on any board the kernel boots on (QEMU virt:
    // ~10); cap defensively and skip extras.
    let mut regions: [(u64, u64); 64] = [(0, 0); 64];
    let mut n = 0usize;
    for entry in mmap.entries() {
        if entry.entry_type != EntryType::USABLE {
            continue;
        }
        if n < regions.len() {
            regions[n] = (entry.base, entry.length);
            n += 1;
        }
    }
    init_from_regions(&regions[..n], hhdm);
}

/// Allocate one physical frame. Returns its physical base address (4 KiB
/// aligned), or None if out of memory.
pub fn alloc_frame() -> Option<u64> {
    for frame in 1..MAX_FRAMES {
        if !is_used(frame) {
            mark_used(frame);
            adjust_free(-1);
            return Some(frame as u64 * FRAME_SIZE);
        }
    }
    None
}

/// Allocate `n` *physically contiguous* frames. Returns the physical base of
/// the run (4 KiB aligned), or None. Needed for DMA regions like the virtio
/// virtqueue, which the device addresses as one contiguous physical block.
pub fn alloc_contiguous(n: usize) -> Option<u64> {
    if n == 0 {
        return None;
    }
    let mut start = 1usize;
    while start + n <= MAX_FRAMES {
        let mut ok = true;
        for k in 0..n {
            if is_used(start + k) {
                ok = false;
                start += k + 1; // skip past the used frame
                break;
            }
        }
        if ok {
            for k in 0..n {
                mark_used(start + k);
                adjust_free(-1);
            }
            return Some(start as u64 * FRAME_SIZE);
        }
    }
    None
}

/// Free a frame previously returned by `alloc_frame`.
pub fn free_frame(phys: u64) {
    let frame = (phys / FRAME_SIZE) as usize;
    if frame < MAX_FRAMES && is_used(frame) {
        mark_free(frame);
        adjust_free(1);
    }
}

/// Number of free frames.
pub fn free_count() -> usize {
    unsafe { (&raw const FREE_COUNT).read() }
}

/// The HHDM offset: physical address `p` is readable at virtual `p + offset`.
/// (Used by the page-table walker at B2 Step 2.)
#[allow(dead_code)]
pub fn hhdm_offset() -> u64 {
    unsafe { (&raw const HHDM_OFFSET).read() }
}

/// Virtual address (in the HHDM) of a physical address.
/// (Used by the page-table walker at B2 Step 2.)
#[allow(dead_code)]
pub fn phys_to_virt(phys: u64) -> *mut u8 {
    (phys + hhdm_offset()) as *mut u8
}

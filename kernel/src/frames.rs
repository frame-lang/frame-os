// kernel/src/frames.rs
//
// Physical frame allocator (B2 Step 1). Pure native — bookkeeping over raw
// physical memory, the territory where a state machine adds ceremony, not
// clarity.
//
// A bitmap over 4 KiB physical frames: bit f set ⇒ frame f is in use. Limine
// hands us a memory map (which regions are USABLE) and an HHDM offset (a
// direct map of all physical memory at a fixed virtual offset, so we can
// touch any physical frame as `phys + HHDM`). We mark every frame used,
// then free the frames inside USABLE regions. The kernel image, Limine's
// own structures, and this bitmap all live in non-USABLE regions, so they
// are never handed out.
//
// Single-threaded use at B2 (init + the page-fault path, which runs with
// interrupts off). A lock is added when a preemptible context allocates
// (B3).

use limine::memory_map::EntryType;
use limine::request::{HhdmRequest, MemoryMapRequest};

#[used]
#[link_section = ".requests"]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

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

/// Initialize the allocator from Limine's memory map + HHDM. Must run once
/// before any alloc.
pub fn init() {
    let hhdm = HHDM_REQUEST
        .get_response()
        .expect("limine HHDM response missing")
        .offset();
    let mmap = MEMORY_MAP_REQUEST
        .get_response()
        .expect("limine memory map response missing");

    unsafe {
        (&raw mut HHDM_OFFSET).write(hhdm);
    }

    for entry in mmap.entries() {
        if entry.entry_type != EntryType::USABLE {
            continue;
        }
        let start = (entry.base / FRAME_SIZE) as usize;
        let end = ((entry.base + entry.length) / FRAME_SIZE) as usize;
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

// kernel/src/allocator.rs
//
// Kernel heap. The Frame-generated `Kernel` code allocates: it builds
// `String`s for event payloads, `Vec`s for the compartment / context
// stacks, and `Rc`s for frame events. None of that works without a
// global allocator, so we install one before `Kernel::__create()` runs.
//
// At B0 the heap is a fixed 256 KiB static buffer managed by
// `linked_list_allocator`. This is plenty for the boot HSM (which
// allocates a handful of small Strings/Vecs) and avoids needing a real
// physical-frame allocator + paging, which lands at B1. When B1's memory
// manager exists, `init()` will hand it a region of mapped physical
// memory instead of a static array.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};
use linked_list_allocator::LockedHeap;

/// The global allocator, wrapped to **count allocations**. Frame's runtime
/// allocates on every event dispatch (an `Rc<FrameEvent>` + a context map), so a
/// running count lets us *measure* that per-event cost — the number the
/// `frame_assessment.md` flagged but never quantified (see R2 / `tcp::scale_stress`).
/// The counter is a `Relaxed` atomic bump per `alloc`; negligible overhead.
struct CountingHeap {
    inner: LockedHeap,
    allocs: AtomicU64,
}
impl CountingHeap {
    const fn empty() -> Self {
        Self {
            inner: LockedHeap::empty(),
            allocs: AtomicU64::new(0),
        }
    }
}
unsafe impl GlobalAlloc for CountingHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.allocs.fetch_add(1, Ordering::Relaxed);
        self.inner.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.inner.dealloc(ptr, layout);
    }
}

#[global_allocator]
static ALLOCATOR: CountingHeap = CountingHeap::empty();

/// Total heap allocations since boot (for the per-event-allocation measurement).
pub fn alloc_count() -> u64 {
    ALLOCATOR.allocs.load(Ordering::Relaxed)
}

/// Heap size. 256 KiB sufficed through B10, but `exec` now reads each program's
/// ELF image into a per-exec heap buffer (`usermode::read_exec_elf`), and the
/// on-device C compiler `/bin/tcc` is ~1.2 MiB — far past 256 KiB, so its exec
/// `try_reserve` failed and the shell reported "command not found". 8 MiB gives
/// the largest program's image plus steady-state kernel allocations (Frame
/// runtime events, process table, buffer cache) generous headroom. It's a
/// zero-initialized BSS static, so it costs nothing in the on-disk image — the
/// loader carves it from RAM (QEMU's default is well over 100 MiB).
const HEAP_SIZE: usize = 8 * 1024 * 1024;

/// The heap backing store. A zero-initialized static lives in BSS, so it
/// costs nothing in the kernel image on disk — it's carved out of RAM by
/// the loader.
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

/// Initialize the global allocator. Must be called exactly once, before
/// any allocation happens (i.e. before `Kernel::__create()`).
///
/// # Panics
///
/// `LockedHeap::init` does not panic; it simply records the region. If
/// called twice the second call would corrupt the allocator's free list,
/// hence the "exactly once" contract.
pub fn init() {
    // `addr_of_mut!` avoids creating a reference to the mutable static
    // (which the `static_mut_refs` lint forbids); we hand the allocator
    // a raw pointer to the start of the buffer.
    let heap_start = core::ptr::addr_of_mut!(HEAP) as *mut u8;
    unsafe {
        ALLOCATOR.inner.lock().init(heap_start, HEAP_SIZE);
    }
}

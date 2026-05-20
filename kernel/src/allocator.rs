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

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Heap size. 256 KiB is comfortable headroom for B0's allocation needs
/// (boot HSM event/compartment plumbing) without bloating the kernel
/// image's BSS unreasonably.
const HEAP_SIZE: usize = 256 * 1024;

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
        ALLOCATOR.lock().init(heap_start, HEAP_SIZE);
    }
}

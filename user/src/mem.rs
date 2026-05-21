// user/src/mem.rs
//
// A ring-3 heap for the Frame-generated `Parser` code, which allocates
// (`String`/`Vec`/`Rc`/`BTreeMap` for event + compartment plumbing). Mirrors
// the kernel's allocator (`kernel/src/allocator.rs`): a fixed static buffer
// managed by `linked_list_allocator`. Only `frameshell` includes this module,
// so the other (allocator-free) user programs are unaffected.
//
// The user program has no MMU of its own — the buffer is a plain BSS static the
// kernel's ElfLoader maps as part of the program image. 64 KiB (16 pages) is
// ample for tokenizing a few command lines and keeps the program's mapped page
// count modest (the loader tracks up to 64 pages for rollback).

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

const HEAP_SIZE: usize = 64 * 1024;
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

/// Initialize the heap. Call exactly once, before any allocation.
pub fn init() {
    let heap_start = &raw mut HEAP as *mut u8;
    unsafe {
        ALLOCATOR.lock().init(heap_start, HEAP_SIZE);
    }
}

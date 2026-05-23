//! frame-libc heap: C `malloc`/`free`/`calloc`/`realloc` over `brk` (B10-2).
//!
//! The free-list bookkeeping is `linked_list_allocator`; this module adds the
//! two things the C ABI needs that a Rust `GlobalAlloc` doesn't provide:
//!
//!  1. **A size-tracking header.** C `free(ptr)` carries no size, but the
//!     underlying allocator's `deallocate` needs the original `Layout`. So
//!     every allocation is `HEADER + size` bytes; we store `size` in the header
//!     and hand back the pointer just past it, reconstructing the `Layout` on
//!     `free`/`realloc`.
//!  2. **Growth via `brk`.** The heap starts empty and grows on demand: when
//!     first-fit fails, we push the program break up (syscall #10) and `extend`
//!     the heap over the freshly mapped region. The libc is `brk`'s only user
//!     in a process, so the region above `USER_HEAP_BASE` is contiguous and
//!     owned entirely by this allocator.
//!
//! Single-threaded: user processes have no threads, and preemption switches to
//! a *different* process with its own address space + heap, so there is no
//! concurrent access to this one — a plain `static mut` is safe here.

use core::alloc::Layout;
use core::ptr::{self, NonNull};

use linked_list_allocator::Heap;

use crate::sys_brk;

/// Payload alignment (C's `max_align_t` is 16 on x86-64). The header is one
/// alignment unit so the returned pointer stays 16-aligned.
const ALIGN: usize = 16;
const HEADER: usize = 16;
/// Grow the break in 64 KiB steps (a whole number of 4 KiB pages).
const GROW_CHUNK: usize = 64 * 1024;

static mut HEAP: Heap = Heap::empty();
static mut INITED: bool = false;

fn heap() -> &'static mut Heap {
    unsafe { &mut *(&raw mut HEAP) }
}

/// Push the program break up by at least `by` bytes; return the number of bytes
/// actually added (the kernel rounds up to a page, so this may exceed `by`).
fn grow(by: u64) -> usize {
    let cur = sys_brk(0);
    let new = sys_brk(cur + by);
    (new - cur) as usize
}

/// Lazily create the heap on first use: anchor it at the current break and seed
/// it with one chunk.
fn ensure_init() {
    unsafe {
        if INITED {
            return;
        }
        let start = sys_brk(0) as *mut u8;
        let got = grow(GROW_CHUNK as u64);
        heap().init(start, got);
        INITED = true;
    }
}

/// `malloc(size)` — allocate `size` bytes, 16-aligned, or NULL on failure /
/// `size == 0`.
#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    if size == 0 {
        return ptr::null_mut();
    }
    ensure_init();
    let Some(total) = size.checked_add(HEADER) else {
        return ptr::null_mut();
    };
    let layout = match Layout::from_size_align(total, ALIGN) {
        Ok(l) => l,
        Err(_) => return ptr::null_mut(),
    };
    let block = match heap().allocate_first_fit(layout) {
        Ok(p) => p.as_ptr(),
        Err(_) => {
            // Out of free space: grow the break and extend the heap, then retry.
            let got = grow(total.max(GROW_CHUNK) as u64);
            if got == 0 {
                return ptr::null_mut();
            }
            heap().extend(got);
            match heap().allocate_first_fit(layout) {
                Ok(p) => p.as_ptr(),
                Err(_) => return ptr::null_mut(),
            }
        }
    };
    (block as *mut usize).write(size); // header: the payload size
    block.add(HEADER)
}

/// `free(ptr)` — release a block from `malloc`/`calloc`/`realloc`. NULL is a
/// no-op (POSIX).
#[no_mangle]
pub unsafe extern "C" fn free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let block = ptr.sub(HEADER);
    let size = (block as *const usize).read();
    let layout = Layout::from_size_align_unchecked(size + HEADER, ALIGN);
    heap().deallocate(NonNull::new_unchecked(block), layout);
}

/// `calloc(nmemb, size)` — allocate a zeroed array, or NULL on overflow / OOM.
#[no_mangle]
pub unsafe extern "C" fn calloc(nmemb: usize, size: usize) -> *mut u8 {
    let Some(total) = nmemb.checked_mul(size) else {
        return ptr::null_mut();
    };
    let p = malloc(total);
    if !p.is_null() {
        ptr::write_bytes(p, 0, total);
    }
    p
}

/// `realloc(ptr, new_size)` — resize a block, preserving its contents up to the
/// smaller of the old/new sizes. `ptr == NULL` behaves as `malloc`; `new_size
/// == 0` frees and returns NULL. Allocate-copy-free (not in-place).
#[no_mangle]
pub unsafe extern "C" fn realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    if ptr.is_null() {
        return malloc(new_size);
    }
    if new_size == 0 {
        free(ptr);
        return ptr::null_mut();
    }
    let old_size = (ptr.sub(HEADER) as *const usize).read();
    let new = malloc(new_size);
    if new.is_null() {
        return ptr::null_mut(); // original block left intact, per C
    }
    ptr::copy_nonoverlapping(ptr, new, old_size.min(new_size));
    free(ptr);
    new
}

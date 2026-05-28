// kernel/src/vm.rs
//
// Demand-paging policy + the page-fault entry point (B2 Step 3). Native:
// the registered lazy regions and the alloc+map mechanics. The *decision*
// (lazy vs fatal) and the response dispatch live in the Frame
// `PageFaultHandler` HSM, which this module owns one instance of and drives
// from the #PF handler.
//
// The #PF stub (interrupts.rs `isr_page_fault`) reads CR2 + the error code
// and calls `page_fault_handler(addr, error_code)`. The #PF gate clears IF,
// so this runs with no preemption and no concurrent fault — one global
// `PageFaultHandler` driven synchronously is safe (no lock, no queue).

use crate::frame_systems::PageFaultHandler;
use crate::hal::{mmu, MapFlags, Mmu};
use crate::{frames, serial};

const MAX_LAZY: usize = 8;

#[derive(Clone, Copy)]
struct LazyRegion {
    start: u64,
    len: u64,
}

static mut LAZY: [LazyRegion; MAX_LAZY] = [LazyRegion { start: 0, len: 0 }; MAX_LAZY];
static mut LAZY_N: usize = 0;
static mut PFH: Option<PageFaultHandler> = None;

/// Create the global PageFaultHandler. Call once, before installing the
/// #PF vector / taking any fault.
pub fn init() {
    unsafe {
        let p = &raw mut PFH;
        *p = Some(PageFaultHandler::__create());
    }
}

/// Register `[start, start+len)` as a demand-paged (lazy) region: a fault
/// in it is satisfied by allocating + mapping a frame, rather than fatal.
pub fn register_lazy_region(start: u64, len: u64) {
    unsafe {
        let n = (&raw const LAZY_N).read();
        if n >= MAX_LAZY {
            return;
        }
        let arr = (&raw mut LAZY) as *mut LazyRegion;
        (*arr.add(n)).start = start;
        (*arr.add(n)).len = len;
        (&raw mut LAZY_N).write(n + 1);
    }
}

/// True if `addr` lies in a registered lazy region. Called by the
/// PageFaultHandler's `$Classifying` action.
pub fn is_lazy_region(addr: u64) -> bool {
    unsafe {
        let n = (&raw const LAZY_N).read();
        let arr = (&raw const LAZY) as *const LazyRegion;
        for i in 0..n {
            let r = *arr.add(i);
            if addr >= r.start && addr < r.start + r.len {
                return true;
            }
        }
    }
    false
}

/// Satisfy a demand fault at `addr`: allocate a frame and map the
/// containing page writable. Returns false if out of frames. Called by the
/// PageFaultHandler's `$LazyFault` action.
pub fn lazy_map(addr: u64) -> bool {
    let page = addr & !0xFFF;
    match frames::alloc_frame() {
        Some(frame) => {
            unsafe {
                mmu().map(page, frame, MapFlags::WRITABLE);
            }
            true
        }
        None => false,
    }
}

/// The #PF Rust entry, invoked by the `isr_page_fault` stub with the
/// faulting address (CR2) and the CPU's error code. Drives the
/// PageFaultHandler HSM, then acts on its disposition:
///   - `$Killing` (ring-3 fault): tear down the offending process and longjmp
///     back to the kernel — the kernel survives (B3 Step 4b). Never returns.
///   - `$Fatal` (kernel fault): a kernel bug; halt. Never returns.
///   - otherwise recoverable (e.g. demand fault mapped): return so the stub
///     `iretq`s and retries the faulting instruction.
#[no_mangle]
extern "C" fn page_fault_handler(addr: u64, error_code: u64, rip: u64) {
    let pfh = unsafe {
        let p = &raw mut PFH;
        (*p).as_mut().expect("PageFaultHandler initialized")
    };
    // A prior surviving disposition ($Killing) leaves the handler parked in
    // that sink; reset it so this new fault re-classifies from $Classifying.
    if pfh.is_killing() {
        pfh.recover();
    }
    pfh.fault(addr, error_code);
    if pfh.is_killing() {
        // The HSM already printed the "user fault → killing process" line.
        // Also print the faulting RIP — for diagnosing a crash in a user
        // program (e.g. tcc) against its unstripped symbol table (B11-3d).
        serial::write_str("[#PF] faulting RIP=");
        serial::write_hex_u64(rip);
        serial::writeln("");
        crate::usermode::kill_current_user_process(); // never returns
    }
    if pfh.is_fatal() {
        serial::write_str("[#PF] halting. RIP=");
        serial::write_hex_u64(rip);
        serial::writeln("");
        crate::halt_forever();
    }
    // Recoverable (e.g. demand fault mapped): return → iretq → retry.
}

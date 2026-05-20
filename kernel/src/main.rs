// kernel/src/main.rs
//
// Frame OS — bare-metal kernel entry point (B0 Step 2).
//
// Step 2 introduces the Kernel HSM (first hierarchical state machine in
// the project). The boot sequence is now:
//
//   1. Limine hands off → kmain runs
//   2. allocator::init() — set up the heap (framec generated code needs alloc)
//   3. Kernel::__create() — drives the boot chain via $InitMemory →
//      $InitIDT → $InitTimer → $InitConsole → $LaunchInit → $Running.
//      Each phase's $> handler prints its phase to serial.
//   4. After __create returns the kernel is in $Running (or earlier if
//      something panicked and we landed in $Halted). kmain calls
//      halt_forever() to park the CPU — there's no scheduler yet, so
//      $Running is effectively a rest state at B0.
//
// No real init work happens in the phases yet — they print and transition.
// Real init (paging, GDT/IDT, timer) lands at B1+. Step 2 demonstrates
// the HSM scaffold; Step 3 introduces SerialDriver to replace the inline
// `serial::*` calls in Kernel's actions.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;

mod allocator;
mod context;
mod frame_systems;
mod interrupts;
mod io;
mod pic;
mod pit;
mod sched;
mod sched_demo;
mod serial;

use frame_systems::Kernel;

// ---------------------------------------------------------------------------
// Limine boot protocol declarations
// ---------------------------------------------------------------------------

// Base revision: tells Limine which version of its boot protocol we
// support. Revision 3 is the current protocol as of Limine v9+.
#[used]
#[link_section = ".requests"]
static BASE_REVISION: limine::BaseRevision = limine::BaseRevision::with_revision(3);

// Markers that delimit the .requests section. Limine looks between these
// to find our protocol-info structs. Placing them in dedicated sections
// keeps the linker from reordering or eliminating them.
#[used]
#[link_section = ".requests_start_marker"]
static REQUESTS_START_MARKER: limine::request::RequestsStartMarker =
    limine::request::RequestsStartMarker::new();

#[used]
#[link_section = ".requests_end_marker"]
static REQUESTS_END_MARKER: limine::request::RequestsEndMarker =
    limine::request::RequestsEndMarker::new();

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Kernel entry. Called by Limine after long mode is set up.
///
/// # Safety
///
/// Called once at kernel startup; never re-entered. The boot environment
/// (page tables, stack, GDT) is set up by Limine before this runs.
#[no_mangle]
unsafe extern "C" fn kmain() -> ! {
    // Verify Limine actually understood our base revision.
    if !BASE_REVISION.is_supported() {
        halt_forever();
    }

    serial::writeln("Frame OS kernel — B0 Step 2");
    serial::writeln("entering boot HSM...");

    // Heap must be live before any allocating code. framec's generated
    // Kernel constructor allocates Vec/String/Rc for compartment + event
    // plumbing, so init the allocator first.
    allocator::init();

    // Create the Kernel system. Construction synchronously drives the
    // boot chain: $InitMemory.$> fires first, transitions to $InitIDT,
    // etc., until $Running ($LaunchInit.$> finishes). The returned
    // instance is unused at B0 — its only purpose was running the chain.
    // (When B1 adds a scheduler, kmain will hold the Kernel and pump
    // tick() events into it instead of halting here.)
    let _kernel = Kernel::__create();

    // B1 Step 3a: install the IDT and prove the interrupt path works by
    // firing a software breakpoint. The handler prints "[int3 ok]" and
    // `iretq`s; the "[idt] survived int3" line proves we returned.
    interrupts::init();
    serial::write_str("[idt] firing int3: ");
    interrupts::test_breakpoint();
    serial::writeln("\n[idt] survived int3");

    // B1 Step 3b: remap the PIC, start the PIT at 100 Hz, enable
    // interrupts, and wait for ~20 timer ticks. Reaching the "elapsed"
    // line proves IRQ0 is firing (otherwise the hlt loop blocks forever
    // and the smoke test times out). Disable again before the cooperative
    // demo so the two don't interleave.
    pic::remap();
    pit::init(100);
    interrupts::enable();
    serial::writeln("[timer] waiting for ticks...");
    let target = interrupts::ticks() + 20;
    while interrupts::ticks() < target {
        interrupts::wait_for_interrupt();
    }
    serial::writeln("[timer] 20 ticks elapsed");
    interrupts::disable();

    // B1 Step 2: demonstrate the native cooperative context switch — two
    // kernel threads ping-pong on independent stacks and hand control back.
    // Transitional; superseded by the preemptive scheduler below.
    sched_demo::run();

    // B1 Step 3c: real preemption. Two threads busy-loop and print without
    // ever yielding; the timer ISR preempts them round-robin. Both digits
    // appearing proves preemption works.
    sched::run();

    halt_forever();
}

// ---------------------------------------------------------------------------
// Halt loop
// ---------------------------------------------------------------------------

fn halt_forever() -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

// ---------------------------------------------------------------------------
// Panic handler
//
// On panic: write the location to serial then halt. We use the safe
// `serial::*` API. We avoid `format!`-ing the panic message because the
// allocator may itself be the thing that panicked; emitting the static
// location is always safe.
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial::write_str("\nKERNEL PANIC: ");
    if let Some(loc) = info.location() {
        serial::write_str(loc.file());
        serial::write_byte(b':');
        serial::write_u32_decimal(loc.line());
    }
    serial::writeln("\nhalted.");
    halt_forever();
}

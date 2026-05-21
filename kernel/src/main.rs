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
mod frames;
mod gdt;
mod interrupts;
mod io;
mod paging;
mod pic;
mod pit;
mod sched;
mod sched_demo;
mod serial;
mod vm;

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

    // B3 Step 1a: install our own GDT + TSS (required for ring 3 +
    // syscall/sysret). Reaching the next line proves the GDT load + segment
    // reload + ltr didn't fault.
    gdt::init();
    serial::writeln("[gdt] loaded our GDT + TSS");

    // B2 Step 1: physical frame allocator. As of Step 5 the allocator is
    // initialized by the boot HSM's $InitMemory phase (during __create
    // above), so kmain only runs the self-test: two distinct page-aligned
    // frames, free restores the count, realloc after free works.
    serial::write_str("[frames] usable frames: ");
    serial::write_u32_decimal(frames::free_count() as u32);
    serial::writeln("");
    {
        let before = frames::free_count();
        let f1 = frames::alloc_frame().expect("frame alloc");
        let f2 = frames::alloc_frame().expect("frame alloc");
        if f1 != f2 && f1 % 4096 == 0 && f2 % 4096 == 0 && frames::free_count() == before - 2 {
            serial::writeln("[frames] alloc two distinct frames: ok");
        }
        frames::free_frame(f1);
        frames::free_frame(f2);
        if frames::free_count() == before {
            serial::writeln("[frames] free restores count: ok");
        }
        let f3 = frames::alloc_frame().expect("frame alloc");
        frames::free_frame(f3);
        serial::writeln("[frames] realloc after free: ok");
    }

    // B2 Step 2: paging. Map a fresh frame at an unmapped test VA, write a
    // pattern through the mapping, confirm it lands in the right physical
    // frame (cross-checked via the HHDM), then translate and unmap.
    {
        const TEST_VA: u64 = 0x0000_4000_0000_0000; // 64 TiB, unmapped lower-half
        const PATTERN: u64 = 0xDEAD_BEEF_CAFE_F00D;
        let frame = frames::alloc_frame().expect("frame alloc");
        unsafe {
            paging::map(TEST_VA, frame, paging::WRITABLE);
            let p = TEST_VA as *mut u64;
            p.write_volatile(PATTERN);
            let via_va = p.read_volatile();
            let via_hhdm = (frames::phys_to_virt(frame) as *const u64).read_volatile();
            if via_va == PATTERN && via_hhdm == PATTERN {
                serial::writeln("[paging] map + write + read-back: ok");
            }
        }
        if paging::translate(TEST_VA) == Some(frame) {
            serial::writeln("[paging] translate matches frame: ok");
        }
        unsafe {
            paging::unmap(TEST_VA);
        }
        if paging::translate(TEST_VA).is_none() {
            serial::writeln("[paging] unmap clears mapping: ok");
        }
        frames::free_frame(frame);
    }

    // B2 Step 4: per-process address spaces (the primitive B3 needs). Build
    // a fresh PML4 (kernel higher-half mirrored), map a page in it that is
    // NOT mapped in the current space, switch to it, read the page back
    // (proving the new space's mapping is live AND the kernel survived the
    // CR3 load), switch back, and confirm the mapping was isolated to the
    // new space.
    {
        const AS_VA: u64 = 0x0000_3000_0000_0000;
        const AS_PATTERN: u64 = 0x0bad_c0de_1337_d00d;
        let saved = paging::current_pml4();
        let frame = frames::alloc_frame().expect("frame alloc");
        unsafe {
            // Seed the frame via the HHDM (address-space independent).
            (frames::phys_to_virt(frame) as *mut u64).write_volatile(AS_PATTERN);
            let new_as = paging::new_address_space();
            paging::map_in(new_as, AS_VA, frame, paging::WRITABLE);
            paging::switch(new_as);
            let got = (AS_VA as *const u64).read_volatile();
            paging::switch(saved); // back to the original space
            if got == AS_PATTERN {
                serial::writeln("[vm] address-space switch sees its mapping: ok");
            }
        }
        // AS_VA was mapped only in the new space; the original has no such
        // mapping → per-address-space isolation.
        if paging::translate(AS_VA).is_none() {
            serial::writeln("[vm] mapping isolated to its address space: ok");
        }
        frames::free_frame(frame);
    }

    // B1 Step 3a: install the IDT and prove the interrupt path works by
    // firing a software breakpoint. The handler prints "[int3 ok]" and
    // `iretq`s; the "[idt] survived int3" line proves we returned.
    interrupts::init();
    serial::write_str("[idt] firing int3: ");
    interrupts::test_breakpoint();
    serial::writeln("\n[idt] survived int3");

    // B2 Step 3: demand paging via the PageFaultHandler HSM. Register a
    // lazy region, then touch it: the access faults (#PF), the HSM
    // classifies it $LazyFault, maps a fresh frame, and the instruction
    // retries successfully — all driven from inside the exception handler.
    vm::init();
    {
        const LAZY_VA: u64 = 0x0000_5000_0000_0000;
        const PATTERN: u64 = 0x1234_5678_9abc_def0;
        vm::register_lazy_region(LAZY_VA, 4096);
        unsafe {
            let p = LAZY_VA as *mut u64;
            p.write_volatile(PATTERN); // first touch → #PF → demand-mapped → retry
            if p.read_volatile() == PATTERN {
                serial::writeln("[#PF] demand fault recovered: ok");
            }
        }
    }

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

    // B2 Step 3 (fatal path): deliberately fault on an unmapped, non-lazy
    // address. The PageFaultHandler classifies it $Fatal, reports it, and
    // halts — a clean fatal, not a silent triple-fault. This is the last
    // thing kmain does.
    serial::writeln("[#PF] triggering a deliberate fatal fault...");
    unsafe {
        let bad = 0x0000_6000_0000_0000 as *const u64;
        let _ = bad.read_volatile(); // → #PF → $Fatal → halt (never returns)
    }

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

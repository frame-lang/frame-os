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
mod elf;
mod frame_systems;
mod frames;
mod fs;
mod gdt;
mod interrupts;
mod io;
mod ip_reasm;
mod net;
mod paging;
mod pci;
mod percpu;
mod pic;
mod pit;
mod sched;
mod sched_demo;
mod serial;
mod spin;
mod tcp;
mod usermode;
mod vfs;
mod virtio_blk;
mod virtio_net;
mod vm;
mod xhci;

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

// B7 Step 1 (SMP): ask Limine to start the application processors. Without this
// request the non-bootstrap cores are left parked by the bootloader. Each AP is
// launched at `ap_entry` (below) once we write its `goto_address`.
#[used]
#[link_section = ".requests"]
static MP_REQUEST: limine::request::MpRequest = limine::request::MpRequest::new();

/// Count of application processors that have reached `ap_entry` and set up their
/// per-CPU state. The BSP waits on this during SMP bringup.
static AP_ONLINE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// B7 Step 2: a cross-core, lock-protected shared counter. Every core (BSP + APs)
// hammers it concurrently; if the `SpinLock` is correct, the final total is
// exactly cores × `HAMMER_ITERS` with no lost updates. `AP_HAMMERED` lets the
// BSP wait for the APs to finish their share.
static SHARED_COUNTER: spin::SpinLock<u64> = spin::SpinLock::new(0);
static AP_HAMMERED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
const HAMMER_ITERS: u64 = 50_000;

/// Increment the shared counter `HAMMER_ITERS` times, taking the lock each time —
/// the cross-core critical-section stress. Run concurrently by every core.
fn hammer_counter() {
    for _ in 0..HAMMER_ITERS {
        *SHARED_COUNTER.lock() += 1;
    }
}

/// Application-processor entry point. Limine jumps here (in our address space,
/// on a Limine-provided stack) once the BSP writes the CPU's `goto_address`. The
/// CPU's index was stashed in `extra` by the BSP. B7 Step 1: set up per-CPU state
/// (GS base), report online, and park — later steps will run the scheduler here.
unsafe extern "C" fn ap_entry(cpu: &limine::mp::Cpu) -> ! {
    use core::sync::atomic::Ordering;
    let index = cpu.extra.load(Ordering::SeqCst) as usize;
    percpu::init_this_cpu(index, cpu.lapic_id);
    AP_ONLINE.fetch_add(1, Ordering::SeqCst);
    // B7 Step 2: hammer the shared counter concurrently with the other cores
    // (the SpinLock cross-core stress), then signal done.
    hammer_counter();
    AP_HAMMERED.fetch_add(1, Ordering::SeqCst);
    // Park: interrupts off (the timer/PIC route to the BSP; APs run the
    // scheduler from a later B7 step), low-power halt.
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
    }
}

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

    // Heap must be live before the Kernel HSM constructor (framec's
    // generated code allocates Vec/String/Rc for event + compartment
    // plumbing).
    allocator::init();

    // B3 Step 1a: our own GDT + TSS, installed BEFORE the boot HSM so that
    // $InitIDT's interrupts::init() builds its gate descriptors with our
    // kernel CS (0x08). Reaching the marker proves the lgdt + segment reload
    // + ltr didn't fault.
    gdt::init();
    serial::writeln("[gdt] loaded our GDT + TSS");

    serial::writeln("entering boot HSM...");

    // Drive the boot chain. As of B2/B3 the init phases do real work:
    // $InitMemory (frame allocator) → $InitIDT (IDT) → $InitTimer (PIC+PIT)
    // → $InitConsole (SerialDriver) → $LaunchInit → $Running. The returned
    // instance is unused here; its purpose was running the chain.
    let _kernel = Kernel::__create();

    // B7 Step 1 (SMP): bring up the application processors. Set up the BSP's own
    // per-CPU block, then launch each AP (stashing its index in `extra`) and wait
    // for it to report online. The APs park; the rest of kmain's demos run on the
    // BSP. (Per-CPU scheduling across cores lands in B7 Step 2.)
    {
        use core::sync::atomic::Ordering;
        if let Some(mp) = MP_REQUEST.get_response() {
            let bsp = mp.bsp_lapic_id();
            percpu::init_this_cpu(0, bsp); // BSP is per-CPU index 0
            let mut next_index = 1usize;
            let mut ap_count = 0usize;
            for cpu in mp.cpus() {
                if cpu.lapic_id == bsp || next_index >= percpu::MAX_CPUS {
                    continue;
                }
                cpu.extra.store(next_index as u64, Ordering::SeqCst);
                cpu.goto_address.write(ap_entry); // launches the AP at ap_entry
                next_index += 1;
                ap_count += 1;
            }
            // Bounded wait for the APs to report in.
            let mut spins = 0u64;
            while AP_ONLINE.load(Ordering::SeqCst) < ap_count && spins < 200_000_000 {
                spins += 1;
                core::hint::spin_loop();
            }
            let online = AP_ONLINE.load(Ordering::SeqCst) + 1; // + BSP
            serial::write_str("[smp] cores online: ");
            serial::write_u32_decimal(online as u32);
            serial::write_str(" of ");
            serial::write_u32_decimal((ap_count + 1) as u32);
            serial::write_str(" (BSP lapic ");
            serial::write_u32_decimal(bsp);
            serial::write_str(", this cpu ");
            serial::write_u32_decimal(percpu::this_cpu_index());
            serial::writeln(")");

            // B7 Step 2: the BSP joins the APs in hammering the shared counter
            // (they started as soon as they came online), then waits for all APs
            // to finish and checks the total — exactly cores × HAMMER_ITERS iff
            // the SpinLock serialized every increment with no lost update.
            hammer_counter();
            let mut spins = 0u64;
            while AP_HAMMERED.load(Ordering::SeqCst) < ap_count && spins < 1_000_000_000 {
                spins += 1;
                core::hint::spin_loop();
            }
            let total = *SHARED_COUNTER.lock();
            let expected = (ap_count as u64 + 1) * HAMMER_ITERS;
            serial::write_str("[smp] shared counter: ");
            serial::write_u32_decimal(total as u32);
            serial::write_str(" (expected ");
            serial::write_u32_decimal(expected as u32);
            serial::writeln(")");
            if total == expected {
                serial::writeln("[smp] cross-core lock: ok (no lost updates)");
            } else {
                serial::writeln("[smp] cross-core lock: FAILED (lost updates)");
            }
        } else {
            serial::writeln("[smp] no MP response (single core)");
        }
    }

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

    // B1 Step 3a: prove the interrupt path with a software breakpoint. The
    // IDT was installed by the boot HSM's $InitIDT phase; the handler prints
    // "[int3 ok]" and `iretq`s, and "[idt] survived int3" proves we returned.
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

    // B1 Step 3b: the PIC was remapped + the PIT started by the boot HSM's
    // $InitTimer phase. Enable interrupts and wait for ~20 ticks (reaching
    // "elapsed" proves IRQ0 fires; otherwise the hlt loop blocks forever and
    // the smoke test times out), then disable before the cooperative demo.
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

    // B4 Step 1: init virtio-blk and round-trip a sector (write → IRQ → post →
    // drain → BlockRequest), exercising the deferred-event path.
    virtio_blk::run_demo();

    // B4 Step 2: mount the FS, read a baked file, and create/write/read/delete
    // round-trip — over the buffer cache + the Mount HSM.
    fs::run_demo();

    // B4 Step 3: open files by path through the VFS (incl. a nested directory)
    // and exercise the OpenFile lifecycle.
    vfs::run_demo();

    // B3 Step 1b: the user/kernel boundary. Enter ring 3 running a tiny
    // hand-crafted program that writes "AB" via syscalls and exits(42); the
    // exit syscall longjmps back to the kernel.
    usermode::run();

    // B5 Step 1/2a: bring up virtio-net (NIC init + TX + RX + post/drain), then
    // resolve the slirp gateway's MAC through the `ArpResolver` Frame system —
    // the first networking Frame system + the retransmit-timer-via-enter-handler
    // pattern.
    net::run_demo();

    // B6 Step 1: bring up the xHCI USB host controller (PCI discovery + MMIO +
    // reset + DCBAA/command-ring/event-ring setup + Run), then report any device
    // connected on a port.
    if xhci::init() {
        // B6 Step 2: drive the connected port through the HubPort Frame system —
        // connect → reset (a timed transition) → enabled, readying the device
        // for enumeration.
        xhci::run_port_lifecycle();
        // B6 Step 3: enumerate the device through the UsbEnumeration Frame system
        // — Enable Slot → Address Device → GET_DESCRIPTOR → SET_CONFIGURATION.
        xhci::run_enumeration();
        // B6 Step 4: configure the interrupt endpoint and complete one transfer
        // (a HID key report) through the UsbTransfer Frame system.
        xhci::run_transfer();
    }

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
    // Signal a clean stop to the host. QEMU's `isa-debug-exit` device
    // (wired up by the smoke harness at iobase 0xf4) turns a port write
    // into a process exit with code `(value << 1) | 1` — so writing 0x10
    // yields exit code 33, which the harness recognizes as "the kernel
    // finished and parked." On real hardware (and under a QEMU without the
    // device) the write goes to an unclaimed I/O port and is harmless; we
    // fall through to the `hlt` loop and park the CPU as before.
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") 0xf4u16,
            in("al") 0x10u8,
            options(nomem, nostack, preserves_flags),
        );
    }
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

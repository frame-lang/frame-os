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
// The `interactive` build boots straight to a shell and deliberately skips the
// B0–B7 self-test demos (see `kmain`), so the scaffolding those demos use — the
// SMP stress statics/helpers, the cross-core post machinery, FS write-path and
// allocator-introspection helpers, etc. — is unused *in that build only*. None
// of it is dead in the default (smoke-tested) build; rather than pepper dozens of
// `#[cfg]` gates across modules, scope a single dead-code allowance to the feature.
#![cfg_attr(feature = "interactive", allow(dead_code))]

extern crate alloc;

use core::panic::PanicInfo;

mod allocator;
mod arch;
mod console;
mod context;
mod crosscore;
mod elf;
mod fpu;
mod frame_systems;
mod frames;
mod fs;
mod gdt;
mod hal;
mod interrupts;
mod io;
mod ip_reasm;
mod ksched;
mod lapic;
mod lockorder;
mod net;
mod pci;
mod pcsched;
mod percpu;
mod pic;
mod pipe;
mod pit;
// RAM-backed block device for the interactive build (#110 mitigation): serves
// the fs from a baked-in image in RAM, bypassing QEMU's flaky emulated disk.
#[cfg(feature = "interactive")]
mod ramdisk;
mod reactor;
mod rtc;
mod sched;
mod sched_demo;
mod serial;
mod spin;
mod tcp;
mod usermode;
mod vfs;
// The interactive build serves its fs from the RAM disk, so virtio-blk's
// read/write data path is unused there (its `on_irq` is still wired into the
// IDT); allow the resulting dead code only in that build. The default + smoke
// builds use the driver fully and keep full dead-code warnings.
#[cfg_attr(feature = "interactive", allow(dead_code))]
mod virtio_blk;
mod virtio_net;
mod vm;
mod xhci;

use frame_systems::Kernel;
// The MMU is exercised only by the default build's paging/vm/TLB smoke blocks;
// the interactive build replaces that boot tail with the shell.
#[cfg(not(feature = "interactive"))]
use hal::{mmu, MapFlags, Mmu};

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
// R5b: count of APs that verified they loaded *their own* per-CPU TSS selector.
static AP_TSS_OK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// B7 Step 2: a cross-core, lock-protected shared counter. Every core (BSP + APs)
// hammers it concurrently; if the `SpinLock` is correct, the final total is
// exactly cores × `HAMMER_ITERS` with no lost updates. `AP_HAMMERED` lets the
// BSP wait for the APs to finish their share.
static SHARED_COUNTER: spin::SpinLock<u64> = spin::SpinLock::new(0);
static AP_HAMMERED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
const HAMMER_ITERS: u64 = 50_000;

// B7 Step 4: per-CPU preemption. Each AP runs a busy loop preempted by its own
// LAPIC timer until the timer has fired `TARGET_TICKS` times, then signals done.
static AP_PREEMPTED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
const TARGET_TICKS: u64 = 5;

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
    // Load our GDT first (it reloads gs, zeroing the GS base), THEN set up the
    // per-CPU block — so the GS-based per-CPU state survives. `load_on_ap` also
    // `ltr`s this core's own TSS (R5b), so a #DF here lands on this core's IST
    // stack rather than triple-faulting.
    gdt::load_on_ap(index);
    percpu::init_this_cpu(index, cpu.lapic_id);
    fpu::init_this_cpu(); // enable SSE/x87 + fninit on this AP (B11-3a)
                          // R5b: confirm this core loaded its own per-CPU TSS (so #DF uses its IST stack).
    if gdt::current_tr() == gdt::tss_selector(index) {
        AP_TSS_OK.fetch_add(1, Ordering::SeqCst);
    }
    AP_ONLINE.fetch_add(1, Ordering::SeqCst);
    // B7 Step 2: hammer the shared counter concurrently with the other cores
    // (the SpinLock cross-core stress), then signal done.
    hammer_counter();
    AP_HAMMERED.fetch_add(1, Ordering::SeqCst);
    // B7 cross-core post: contribute this AP's events into the MPSC queue that
    // the BSP drains into its EventCounter Frame system instance.
    crosscore::ap_post_phase();

    // B7 Step 4: per-CPU preemptive execution. Load the IDT, start this core's
    // LAPIC timer, enable interrupts, and run a busy loop that the timer
    // preempts — proving the core runs a real, time-sliced thread (not just a
    // one-shot). Run until the timer has fired TARGET_TICKS times.
    interrupts::load_idt_on_ap();
    lapic::init_this_cpu();
    interrupts::enable();
    let mut work = 0u64;
    while percpu::this_cpu_ticks() < TARGET_TICKS {
        work += 1;
        core::hint::spin_loop();
    }
    percpu::set_this_cpu_work(work);
    AP_PREEMPTED.fetch_add(1, Ordering::SeqCst);

    // R1a: drive this core's own Scheduler Frame instance from the cross-core
    // posts the BSP staged for it (admit/retire tasks). The instance is pinned
    // to this core; only Send event data crossed from the BSP.
    ksched::ap_run(index);

    // R1b: per-core context-switched execution. This core builds a run queue of
    // kernel threads and time-slices them under its own LAPIC timer, driving its
    // own Scheduler Frame instance through $Active→$Idle as they spawn and exit.
    // Real preemptive multitasking per core; the BSP reads back the results.
    pcsched::ap_run(index);

    // R5a: nested-lock deadlock-avoidance stress. This core acquires two ranked
    // locks (A→B) in the documented global order, many times, concurrently with
    // every other core — exercising nested locking beyond the B7 leaf-lock stage.
    lockorder::stress();

    // B7 Step 5: stay *interrupt-enabled* and idle, so this core can service the
    // BSP's TLB-shootdown IPI (and its own timer). `hlt` with IF=1 wakes on each
    // interrupt and loops back — the AP's resting state for the rest of the boot.
    loop {
        interrupts::wait_for_interrupt();
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
    //
    // Skipped in the `interactive` build: the SMP stress demos run billion-iteration
    // spin-bounded waits that take minutes under TCG, and the shell runs single-core
    // on the BSP — an interactive build boots straight to a prompt (see below).
    #[cfg(not(feature = "interactive"))]
    {
        use core::sync::atomic::Ordering;
        if let Some(mp) = MP_REQUEST.get_response() {
            let bsp = mp.bsp_lapic_id();
            percpu::init_this_cpu(0, bsp); // BSP is per-CPU index 0
            fpu::init_this_cpu(); // enable SSE/x87 + fninit on the BSP (B11-3a)
            lapic::map(); // map the LAPIC register page before the APs use it
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

            // R5b: per-CPU TSS + IST. Each core (BSP in gdt::init, APs in
            // load_on_ap) loaded *its own* TSS, whose ist[0] points at that core's
            // #DF stack — so a double fault on any core lands on a known-good stack
            // (IDT vector 8 → IST1) instead of triple-faulting. Verify: the BSP's
            // TR is core 0's selector, and every AP confirmed its own.
            let bsp_tss_ok = gdt::current_tr() == gdt::tss_selector(0);
            let ap_tss_ok = AP_TSS_OK.load(Ordering::SeqCst);
            serial::write_str("[smp] per-CPU TSS+IST: ");
            serial::write_u32_decimal((ap_tss_ok + bsp_tss_ok as usize) as u32);
            serial::write_str(" of ");
            serial::write_u32_decimal((ap_count + 1) as u32);
            serial::writeln(" cores armed (#DF -> IST1)");
            if bsp_tss_ok && ap_tss_ok == ap_count {
                serial::writeln("[smp] per-CPU TSS+IST: ok");
            }

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

            // B7 cross-core post: drive a Frame system (EventCounter) from the
            // other cores. The APs posted tick events into the MPSC queue (in
            // their post phase); the BSP owns the instance and drains the queue
            // into it — the instance never leaves this core, so framec's
            // non-Send codegen is fine.
            crosscore::run_drain_demo(ap_count);

            // B7 Step 4: per-CPU preemption. Wait for each AP to be preempted
            // TARGET_TICKS times by its own LAPIC timer (proving it ran a real,
            // time-sliced thread), then report each core's tick + work counts.
            let mut spins = 0u64;
            while AP_PREEMPTED.load(Ordering::SeqCst) < ap_count && spins < 2_000_000_000 {
                spins += 1;
                core::hint::spin_loop();
            }
            let mut all_preempted = true;
            for i in 1..=ap_count {
                let t = percpu::cpu_ticks(i);
                let w = percpu::cpu_work(i);
                serial::write_str("[smp] core ");
                serial::write_u32_decimal(i as u32);
                serial::write_str(": ");
                serial::write_u32_decimal(t as u32);
                serial::write_str(" timer ticks, ");
                serial::write_u32_decimal(w as u32);
                serial::writeln(" work units");
                if t < TARGET_TICKS {
                    all_preempted = false;
                }
            }
            if all_preempted && ap_count > 0 {
                serial::writeln("[smp] per-core preemption: ok (each AP timer-sliced)");
            }

            // B7 Step 5: TLB shootdown. Map a test page, then unmap it on the BSP
            // (flushing the BSP's own TLB via `invlpg`) and IPI the other cores to
            // flush theirs. Wait for every core to ack the flush — the barrier
            // that lets the page be safely reused/freed. (The APs are idling
            // interrupt-enabled, so they service the shootdown IPI.)
            if ap_count > 0 {
                const SHOOT_VA: u64 = 0x0000_7000_0000_0000;
                if let Some(frame) = frames::alloc_frame() {
                    unsafe {
                        mmu().map(SHOOT_VA, frame, MapFlags::WRITABLE);
                        (SHOOT_VA as *mut u64).write_volatile(0xDEAD_BEEF);
                        mmu().unmap(SHOOT_VA); // flushes the BSP's own TLB entry
                    }
                    interrupts::shootdown(SHOOT_VA); // IPI the other cores to flush theirs
                    let mut spins = 0u64;
                    while interrupts::shootdown_acks() < ap_count && spins < 1_000_000_000 {
                        spins += 1;
                        core::hint::spin_loop();
                    }
                    let acks = interrupts::shootdown_acks();
                    serial::write_str("[smp] TLB shootdown: ");
                    serial::write_u32_decimal(acks as u32);
                    serial::write_str(" of ");
                    serial::write_u32_decimal(ap_count as u32);
                    serial::writeln(" cores flushed");
                    if acks == ap_count {
                        serial::writeln("[smp] TLB shootdown ack barrier: ok (safe to reuse page)");
                    }
                    frames::free_frame(frame); // safe now — every core has flushed
                }
            }

            // R1a: per-core Frame schedulers driven by cross-core posts. Stage
            // each AP's admit/retire sequence (the BSP scheduling work onto remote
            // cores), then read back each core's Scheduler trajectory. Each AP
            // owns its Scheduler instance; only the SchedPost data crossed cores.
            if ap_count > 0 {
                const TASKS_PER_CORE: u32 = 3;
                for cpu in 1..=ap_count {
                    for _ in 0..TASKS_PER_CORE {
                        ksched::post_ready(cpu);
                    }
                    for _ in 0..TASKS_PER_CORE {
                        ksched::post_unready(cpu);
                    }
                    ksched::set_expected(cpu, (TASKS_PER_CORE as usize) * 2);
                }
                let mut spins = 0u64;
                while ksched::done_count() < ap_count && spins < 2_000_000_000 {
                    spins += 1;
                    core::hint::spin_loop();
                }
                let mut all_ok = true;
                for cpu in 1..=ap_count {
                    let pk = ksched::peak(cpu);
                    let idle = ksched::ended_idle(cpu);
                    serial::write_str("[smp] core ");
                    serial::write_u32_decimal(cpu as u32);
                    serial::write_str(" Frame scheduler: peak ");
                    serial::write_u32_decimal(pk);
                    serial::write_str(" runnable, ended idle=");
                    serial::writeln(if idle { "true" } else { "false" });
                    if pk != TASKS_PER_CORE || !idle {
                        all_ok = false;
                    }
                }
                if all_ok {
                    serial::writeln("[smp] per-core Frame schedulers driven cross-core: ok");
                }
            }

            // R1b: per-core context-switched execution. Each AP is now running a
            // real run queue of kernel threads, time-slicing them under its own
            // LAPIC timer and driving its own Scheduler Frame instance. Snapshot
            // the heap-alloc counter, wait for every core to drain its queue
            // ($Idle), then read back per-core results — and the alloc delta,
            // which is the per-event allocation cost paid by N cores scheduling
            // concurrently against the one shared, spin-locked heap (the load
            // case R1a could not exercise; ties back to R2a finding #3).
            if ap_count > 0 {
                // Snapshot the heap-alloc counter, then release the APs — so the
                // delta below covers every per-core dispatch in the phase.
                let allocs_before = allocator::alloc_count();
                pcsched::release();
                let mut spins = 0u64;
                while pcsched::done_count() < ap_count && spins < 4_000_000_000 {
                    spins += 1;
                    core::hint::spin_loop();
                }
                let alloc_delta = allocator::alloc_count().saturating_sub(allocs_before);
                let want = pcsched::workers_per_core();
                let mut all_ok = pcsched::done_count() == ap_count;
                for cpu in 1..=ap_count {
                    let sw = pcsched::switches(cpu);
                    let ran = pcsched::threads_run(cpu);
                    let idle = pcsched::ended_idle(cpu);
                    serial::write_str("[r1b] core ");
                    serial::write_u32_decimal(cpu as u32);
                    serial::write_str(": sliced ");
                    serial::write_u32_decimal(ran);
                    serial::write_str(" threads, ");
                    serial::write_u32_decimal(sw);
                    serial::write_str(" switches, ended idle=");
                    serial::writeln(if idle { "true" } else { "false" });
                    if ran != want || !idle || sw < want {
                        all_ok = false;
                    }
                }
                serial::write_str("[r1b] heap allocs during per-core scheduling: ");
                serial::write_u32_decimal(alloc_delta as u32);
                serial::write_str(" (");
                serial::write_u32_decimal(ap_count as u32);
                serial::writeln(" cores dispatching concurrently)");
                if all_ok {
                    serial::writeln("[r1b] per-core context-switched execution: ok");
                }
            }

            // R5a: nested-lock deadlock-avoidance stress. The BSP joins the APs
            // (which started in ap_entry) in hammering two ranked locks in the
            // documented A→B order, then waits for all cores and checks the
            // counters: exactly cores × ITERS on each iff every nested increment
            // serialized with no lost update and no deadlock. The SpinLock rank
            // checker would have panicked had any core reversed the order.
            lockorder::stress();
            let cores = ap_count + 1; // + BSP
            let mut spins = 0u64;
            while lockorder::done_count() < cores && spins < 4_000_000_000 {
                spins += 1;
                core::hint::spin_loop();
            }
            let (ta, tb) = lockorder::totals();
            let want = lockorder::expected(cores as u64);
            serial::write_str("[smp] nested-lock stress: A=");
            serial::write_u32_decimal(ta as u32);
            serial::write_str(" B=");
            serial::write_u32_decimal(tb as u32);
            serial::write_str(" (expected ");
            serial::write_u32_decimal(want as u32);
            serial::writeln(")");
            if ta == want && tb == want && lockorder::done_count() == cores {
                serial::writeln("[smp] nested-lock ordering: ok (no deadlock, no lost updates)");
            }
        } else {
            serial::writeln("[smp] no MP response (single core)");
        }
    }

    // The B0–B6 self-test demos. Skipped in the `interactive` build, which boots
    // straight to a shell (its minimal init is the `#[cfg(feature=…)]` block below).
    #[cfg(not(feature = "interactive"))]
    {
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
                mmu().map(TEST_VA, frame, MapFlags::WRITABLE);
                let p = TEST_VA as *mut u64;
                p.write_volatile(PATTERN);
                let via_va = p.read_volatile();
                let via_hhdm = (frames::phys_to_virt(frame) as *const u64).read_volatile();
                if via_va == PATTERN && via_hhdm == PATTERN {
                    serial::writeln("[paging] map + write + read-back: ok");
                }
            }
            if mmu().translate(TEST_VA) == Some(frame) {
                serial::writeln("[paging] translate matches frame: ok");
            }
            unsafe {
                mmu().unmap(TEST_VA);
            }
            if mmu().translate(TEST_VA).is_none() {
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
            let saved = mmu().current_address_space();
            let frame = frames::alloc_frame().expect("frame alloc");
            unsafe {
                // Seed the frame via the HHDM (address-space independent).
                (frames::phys_to_virt(frame) as *mut u64).write_volatile(AS_PATTERN);
                let new_as = mmu().new_address_space();
                mmu().map_in(new_as, AS_VA, frame, MapFlags::WRITABLE);
                mmu().switch_address_space(new_as);
                let got = (AS_VA as *const u64).read_volatile();
                mmu().switch_address_space(saved); // back to the original space
                if got == AS_PATTERN {
                    serial::writeln("[vm] address-space switch sees its mapping: ok");
                }
            }
            // AS_VA was mapped only in the new space; the original has no such
            // mapping → per-address-space isolation.
            if mmu().translate(AS_VA).is_none() {
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

        // R2a: measure Frame's per-event allocation at scale — spin up 16
        // TcpConnection FSM instances on the real kernel heap, drive each through a
        // full lifecycle, and report allocations per dispatch.
        tcp::scale_stress();

        // B6 Step 1: bring up the xHCI USB host controller (PCI discovery + MMIO +
        // reset + DCBAA/command-ring/event-ring setup + Run), then report any device
        // connected on a port.
        if xhci::init() {
            // B6 Step 2: drive the connected port through the HubPort Frame system —
            // connect → reset (a timed transition) → enabled, readying the device
            // for enumeration.
            xhci::run_port_lifecycle();
            // B6 Step 3 / R3a: enumerate every attached device concurrently through
            // the UsbEnumeration Frame system — Enable Slot → Address Device →
            // GET_DESCRIPTOR → SET_CONFIGURATION, one instance per device.
            xhci::run_enumeration();
            // R3b: read each device's configuration descriptor and classify it (HID
            // keyboard/mouse, mass storage), so the class-specific drivers below
            // route by class rather than table index.
            xhci::classify_devices();
            // B6 Step 4: configure the keyboard's interrupt endpoint and complete one
            // transfer (a HID key report) through the UsbTransfer Frame system.
            xhci::run_transfer();
            // R3b: drive the mass-storage device's SCSI commands over Bulk-Only
            // Transport through the UsbMsd Frame system.
            xhci::run_msd();
        }
    } // end of the B0–B6 self-test demos (default build only)

    // B2 Step 3 (fatal path): deliberately fault on an unmapped, non-lazy
    // address. The PageFaultHandler classifies it $Fatal, reports it, and
    // halts — a clean fatal, not a silent triple-fault. This is the last
    // thing kmain does in the default build.
    #[cfg(not(feature = "interactive"))]
    {
        serial::writeln("[#PF] triggering a deliberate fatal fault...");
        unsafe {
            let bad = 0x0000_6000_0000_0000 as *const u64;
            let _ = bad.read_volatile(); // → #PF → $Fatal → halt (never returns)
        }
    }

    // B8 (interactive build): boot straight to the interactive shell. We run only
    // the minimal init the shell needs — the page-fault handler (so a user fault
    // is caught, not fatal), the virtio-blk disk, and the mounted FS (the shell
    // loads `/bin/<cmd>` from disk). `ish` then reads live console input and
    // fork+exec+waits the programs you type, returning here only when you type
    // `exit`. Gated by the `interactive` feature so the default boot + smoke suite
    // is unaffected; the heavy B0–B7 self-test demos above are skipped.
    #[cfg(feature = "interactive")]
    {
        // The BSP's per-CPU block (GS base) MUST be initialized before any user
        // process runs: the scheduler calls `gdt::set_rsp0` on every switch into a
        // ring-3 process, and that reads this core's index via `gs:[0]`. `gdt::init`
        // zeroed the GS base (its `mov gs, kdata`), so without this the read hits
        // virtual address 0 → #PF. The default build does this inside the SMP
        // bring-up block (skipped here), so the interactive build must do it itself.
        let bsp = MP_REQUEST.get_response().map_or(0, |mp| mp.bsp_lapic_id());
        percpu::init_this_cpu(0, bsp);
        fpu::init_this_cpu(); // enable SSE/x87 + fninit on the BSP (B11-3a)

        vm::init(); // PageFaultHandler: a user fault kills the process, not the kernel

        // #110 mitigation: load the baked-in RAM disk. The fs still goes through
        // virtio_blk's Frame-system wrapper (IoScheduler + BlockRequest) — only
        // the transfer backend is the RAM disk instead of the emulated virtqueue,
        // so the heavy interactive I/O never hits the host's flaky disk-completion
        // path while the Frame systems stay on the critical path. (virtio_blk's
        // device init is skipped here; its virtqueue path is the smoke suite's.)
        ramdisk::init();
        if !fs::mount() {
            serial::writeln("[ish] WARNING: FS mount failed — /bin programs unavailable");
        } else {
            serial::writeln("[fs] mounted");
        }
        usermode::run_interactive_shell();
        serial::writeln("[ish] shell exited; halting");
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
        interrupts::wait_for_interrupt();
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

// kernel/src/sched.rs
//
// B1 Step 3c + completion: preemptive round-robin over kernel threads,
// with the Frame `Scheduler` made load-bearing for the run/halt decision.
//
// Split of responsibilities (the honest native/Frame line):
//   - NATIVE (this module): the register/stack mechanics. A fixed TCB array,
//     a per-TCB run state, and `schedule()` — called from the timer ISR —
//     which saves the outgoing thread's rsp and picks the next *runnable*
//     worker round-robin (or the boot context if none). The ISR path never
//     touches a Frame system (Frame dispatch is non-reentrant).
//   - FRAME (`Scheduler`, $Idle/$Active): the run/halt *mode*. Spawning a
//     worker fires `task_ready` ($Idle→$Active); a worker exiting fires
//     `task_unready` (→$Idle at zero). The boot context reads `is_idle()`
//     to decide when to stop. Because the Scheduler is shared across
//     preemptible threads and is non-reentrant, every dispatch runs inside
//     `interrupts::without_interrupts` — single-core mutual exclusion.
//
// The demo: two threads busy-loop printing '1'/'2' (no yield) for a few
// rounds, then each EXITS. Preemption interleaves them (B1-4/B1-5); once
// both have exited the Frame Scheduler is $Idle and the kernel halts
// (B1-6).

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::frame_systems::Scheduler;
use crate::{gdt, interrupts, paging, serial};

const MAX_THREADS: usize = 8;
const STACK_SIZE: usize = 16 * 1024;
const ROUNDS_PER_WORKER: u32 = 4;

#[derive(Clone, Copy, PartialEq)]
enum RunState {
    Free,
    Runnable,
    Blocked, // alive but not runnable (e.g. a parent in wait()) — skipped by the round-robin, still counted "alive"
    Dead,
}

#[derive(Clone, Copy)]
struct Tcb {
    rsp: u64,
    state: RunState,
    // User-process fields (B3 Step 5a). For kernel threads / the boot context
    // these are 0 and the scheduler keeps the kernel address space.
    pml4: u64,       // 0 ⇒ kernel address space; else this process's PML4 phys
    kstack_top: u64, // ring-0 stack top for TSS.RSP0 + the syscall path
    pid: u32,        // the owning Process's pid (0 if none)
    parent_pid: u32, // the pid that forked this one (0 if none) — for wait()
    heap_brk: u64,   // program break: end of this process's brk heap (B9-1)
}

const TCB_INIT: Tcb = Tcb {
    rsp: 0,
    state: RunState::Free,
    pml4: 0,
    kstack_top: 0,
    pid: 0,
    parent_pid: 0,
    heap_brk: 0,
};

/// Base VA of a user process's `brk` heap (B9-1). Sits well above the program
/// image (0x1000_0000) and the user stack (0x2000_0000), so growing the heap
/// upward never collides with either. Within PML4 index 0 (the user half), so a
/// `fork` copies the heap along with the rest of the user address space.
pub const USER_HEAP_BASE: u64 = 0x0000_0000_3000_0000;

static mut TCBS: [Tcb; MAX_THREADS] = [TCB_INIT; MAX_THREADS];
static mut N: usize = 0; // total TCBs incl. boot (slot 0)
static mut CURRENT: usize = 0;
static ACTIVE: AtomicBool = AtomicBool::new(false);

// Address-space + per-process-kernel-stack tracking (B3 Step 5a).
static mut KERNEL_PML4: u64 = 0; // the boot/kernel address space
static mut LAST_PML4: u64 = 0; // CR3 currently loaded (avoid redundant reloads)

// Current user process's ring-0 stack top. #[no_mangle] so the syscall entry
// stub (usermode.rs global_asm) can load it RIP-relative — each syscall runs
// on its own process's kernel stack. Updated by `schedule()` on every switch.
#[no_mangle]
static mut CURRENT_KSTACK: u64 = 0;

static mut STACK1: [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut STACK2: [u8; STACK_SIZE] = [0; STACK_SIZE];

// Per-process ring-0 kernel stacks, indexed by TCB slot (B3 Step 5a). A user
// process traps (timer/syscall) onto its own kernel stack so concurrent
// processes never share ring-0 stack state.
static mut KSTACKS: [[u8; STACK_SIZE]; MAX_THREADS] = [[0; STACK_SIZE]; MAX_THREADS];

// The Frame Scheduler — guarded by interrupts-off critical sections (it is
// non-reentrant and shared across preemptible threads).
static mut SCHED: Option<Scheduler> = None;

fn tcbs() -> *mut Tcb {
    (&raw mut TCBS) as *mut Tcb
}

/// Run `f` with the Frame Scheduler, in a critical section.
fn with_sched<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    interrupts::without_interrupts(|| unsafe {
        let p = &raw mut SCHED;
        let s = (*p).as_mut().expect("scheduler initialized");
        f(s)
    })
}

// ---------------------------------------------------------------------------
// The scheduler callback (invoked from the timer ISR — native only)
// ---------------------------------------------------------------------------

/// Called by `isr_timer` with the interrupted thread's stack pointer.
/// Returns the stack pointer to resume. Pure native: it must not touch the
/// Frame Scheduler (that would re-enter a non-reentrant system from
/// interrupt context).
#[no_mangle]
extern "C" fn schedule(current_rsp: u64) -> u64 {
    interrupts::record_tick();

    if !ACTIVE.load(Ordering::Relaxed) {
        return current_rsp;
    }

    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        let cur = (&raw const CURRENT).read();
        (*t.add(cur)).rsp = current_rsp;

        // Round-robin over worker slots 1..n (skip boot slot 0); fall back
        // to boot (the idle context) when no worker is runnable.
        let mut next = 0usize;
        let mut step = 1usize;
        while step <= n {
            let cand = (cur + step) % n;
            step += 1;
            if cand == 0 {
                continue;
            }
            if (*t.add(cand)).state == RunState::Runnable {
                next = cand;
                break;
            }
        }

        (&raw mut CURRENT).write(next);

        // Address-space + kernel-stack switch (B3 Step 5a). A user process
        // runs in its own PML4 and traps onto its own ring-0 stack; a kernel
        // thread / the boot context runs in the kernel address space. The
        // kernel higher-half is mirrored into every PML4, so the `mov cr3`
        // here (executed on a kernel stack) keeps this code + stack mapped.
        let np = (*t.add(next)).pml4;
        let kernel_pml4 = (&raw const KERNEL_PML4).read();
        let target = if np != 0 { np } else { kernel_pml4 };
        if target != 0 && target != (&raw const LAST_PML4).read() {
            paging::switch(target);
            (&raw mut LAST_PML4).write(target);
        }
        if np != 0 {
            let kstack = (*t.add(next)).kstack_top;
            gdt::set_rsp0(kstack);
            (&raw mut CURRENT_KSTACK).write(kstack);
        }

        (*t.add(next)).rsp
    }
}

/// The pid of the currently-running process (0 if none / a kernel thread).
pub fn current_pid() -> u32 {
    unsafe {
        let cur = (&raw const CURRENT).read();
        (*tcbs().add(cur)).pid
    }
}

// ---------------------------------------------------------------------------
// Thread setup (native)
// ---------------------------------------------------------------------------

fn read_cs() -> u16 {
    let v: u16;
    unsafe {
        asm!("mov {0:x}, cs", out(reg) v, options(nomem, nostack, preserves_flags));
    }
    v
}

fn read_ss() -> u16 {
    let v: u16;
    unsafe {
        asm!("mov {0:x}, ss", out(reg) v, options(nomem, nostack, preserves_flags));
    }
    v
}

/// Craft a fresh thread's stack as a synthetic preemption frame so the
/// first switch `iretq`s `entry` to life with interrupts enabled. Layout at
/// the returned rsp (low → high): [15 zeroed GPRs][RIP][CS][RFLAGS][RSP][SS].
///
/// # Safety
/// `stack_top` must point one past a writable stack of at least 256 bytes.
unsafe fn init_thread(stack_top: *mut u8, entry: extern "C" fn() -> !, cs: u16, ss: u16) -> u64 {
    let top = (stack_top as u64) & !0xF;
    let saved_rsp = top - 160;
    let s = saved_rsp as *mut u64;
    let mut i = 0;
    while i < 15 {
        s.add(i).write(0);
        i += 1;
    }
    s.add(15).write(entry as *const () as usize as u64); // RIP
    s.add(16).write(cs as u64); // CS
    s.add(17).write(0x202); // RFLAGS: IF=1, reserved bit1=1
    s.add(18).write(top - 8); // RSP (≡ 8 mod 16)
    s.add(19).write(ss as u64); // SS
    saved_rsp
}

/// Reserve TCB[0] for the boot context (the idle fallback), and capture the
/// kernel address space so the scheduler can restore it when switching away
/// from a user process.
fn init_boot() {
    unsafe {
        // Reset the table (a fresh scheduler run reuses the global array).
        for i in 0..MAX_THREADS {
            (*tcbs().add(i)) = TCB_INIT;
        }
        (&raw mut N).write(1);
        (&raw mut CURRENT).write(0);
        (*tcbs().add(0)).state = RunState::Runnable; // boot is always available
        let kpml4 = paging::current_pml4();
        (&raw mut KERNEL_PML4).write(kpml4);
        (&raw mut LAST_PML4).write(kpml4);
    }
}

/// Add a worker thread; fires the Frame Scheduler's `task_ready`.
unsafe fn spawn(stack_top: *mut u8, entry: extern "C" fn() -> !) {
    let cs = read_cs();
    let ss = read_ss();
    let rsp = init_thread(stack_top, entry, cs, ss);
    let n = (&raw const N).read();
    let t = tcbs();
    (*t.add(n)).rsp = rsp;
    (*t.add(n)).state = RunState::Runnable;
    (&raw mut N).write(n + 1);
    with_sched(|s| s.task_ready());
}

// ---------------------------------------------------------------------------
// User-process scheduling (B3 Step 5a)
// ---------------------------------------------------------------------------

/// Craft a fresh *user* process's kernel stack as a synthetic preemption frame
/// whose `iretq` drops to ring 3 at `entry` with interrupts enabled. Identical
/// layout to `init_thread` but with ring-3 selectors and the user RSP:
/// [15 zeroed GPRs][RIP=entry][CS=0x23][RFLAGS=0x202][RSP=user_rsp][SS=0x1b].
///
/// # Safety
/// `kstack_top` must point one past a writable kernel stack ≥ 256 bytes.
unsafe fn init_user_frame(kstack_top: u64, entry: u64, user_rsp: u64) -> u64 {
    let top = kstack_top & !0xF;
    let saved_rsp = top - 160;
    let s = saved_rsp as *mut u64;
    let mut i = 0;
    while i < 15 {
        s.add(i).write(0);
        i += 1;
    }
    s.add(15).write(entry); // RIP
    s.add(16).write((gdt::USER_CODE | 3) as u64); // CS (ring 3)
    s.add(17).write(0x202); // RFLAGS: IF=1, reserved bit1=1
    s.add(18).write(user_rsp); // user RSP
    s.add(19).write((gdt::USER_DATA | 3) as u64); // SS (ring 3)
    saved_rsp
}

/// Admit a user process to the scheduler: it runs in address space `pml4`,
/// first enters ring 3 at `entry` with stack `user_rsp`, and is linked to
/// `Process` `pid`. Fires the Frame Scheduler's `task_ready`.
///
/// # Safety
/// `pml4` must be a valid address space with `entry`/`user_rsp` mapped USER.
pub unsafe fn spawn_user(pml4: u64, entry: u64, user_rsp: u64, pid: u32) {
    let n = (&raw const N).read();
    // Top of this slot's kernel stack: base + (n+1)*STACK_SIZE, 16-aligned.
    let base = (&raw mut KSTACKS) as *mut u8;
    let kstack_top = (base.add((n + 1) * STACK_SIZE) as u64) & !0xF;
    let rsp = init_user_frame(kstack_top, entry, user_rsp);
    let t = tcbs();
    (*t.add(n)).rsp = rsp;
    (*t.add(n)).state = RunState::Runnable;
    (*t.add(n)).pml4 = pml4;
    (*t.add(n)).kstack_top = kstack_top;
    (*t.add(n)).pid = pid;
    (*t.add(n)).heap_brk = USER_HEAP_BASE; // fresh process: empty brk heap
    (&raw mut N).write(n + 1);
    with_sched(|s| s.task_ready());
}

/// Admit a `fork`ed child: it runs in (copied) address space `pml4`, resuming
/// from `frame` — the parent's full trap frame with `rax` already set to 0.
/// The frame is copied to the top of the child's kernel stack so the next
/// switch restores it + `iretq`s the child to the fork-return point in ring 3.
///
/// # Safety
/// `pml4` must be a valid (forked) address space matching `frame`'s user RSP.
pub unsafe fn spawn_user_from_frame(
    pml4: u64,
    frame: &crate::usermode::TrapFrame,
    pid: u32,
    parent_pid: u32,
) {
    let n = (&raw const N).read();
    let base = (&raw mut KSTACKS) as *mut u8;
    let kstack_top = (base.add((n + 1) * STACK_SIZE) as u64) & !0xF;
    let saved_rsp = kstack_top - 160; // sizeof(TrapFrame)
    (saved_rsp as *mut crate::usermode::TrapFrame).write(*frame);
    let t = tcbs();
    (*t.add(n)).rsp = saved_rsp;
    (*t.add(n)).state = RunState::Runnable;
    (*t.add(n)).pml4 = pml4;
    (*t.add(n)).kstack_top = kstack_top;
    (*t.add(n)).pid = pid;
    (*t.add(n)).parent_pid = parent_pid;
    // The child inherits the parent's program break — fork_address_space copied
    // the parent's heap pages (PML4 index 0), so the heap contents carry over.
    (*t.add(n)).heap_brk = brk_of_pid(parent_pid).unwrap_or(USER_HEAP_BASE);
    (&raw mut N).write(n + 1);
    with_sched(|s| s.task_ready());
}

/// Block the current process (B3 Step 5d `wait`): mark it `Blocked` and yield
/// to the scheduler, returning only once another context wakes it (`wake`) and
/// it is rescheduled. A Blocked task is skipped by the round-robin but stays
/// "alive" in the Frame Scheduler's count (no `task_unready`), so the boot
/// context won't declare `$Idle` while a parent waits. Restores IF=0 on return
/// (the caller is a syscall handler that must stay non-preemptible).
pub fn block_current() {
    unsafe {
        let cur = (&raw const CURRENT).read();
        interrupts::without_interrupts(|| {
            (*tcbs().add(cur)).state = RunState::Blocked;
        });
        // Yield: enable interrupts so the timer reschedules us away. We resume
        // here (post-hlt) only once `wake` has set us Runnable again.
        while (*tcbs().add(cur)).state == RunState::Blocked {
            interrupts::wait_for_interrupt_enabled();
        }
        // Back to non-preemptible for the rest of the syscall.
        interrupts::disable();
    }
}

/// Wake the (Blocked) task whose process pid is `pid`, marking it Runnable so the
/// scheduler will pick it again. Single-writer-ish (called from the device IRQ to
/// wake a process blocked on I/O, or from a waker); a no-op if no such Blocked
/// task exists. Pure native — safe to call from an interrupt handler.
pub fn wake_pid(pid: u32) {
    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        for i in 0..n {
            if (*t.add(i)).pid == pid && (*t.add(i)).state == RunState::Blocked {
                (*t.add(i)).state = RunState::Runnable;
                return;
            }
        }
    }
}

/// Whether preemptive scheduling is active (a user process is running under the
/// scheduler). Drivers use this to choose blocking I/O (yield + wake) over a
/// busy-wait: during early boot (before `run_until_idle`) it's false, so the
/// busy-wait path is taken; once processes run, true → block-and-wake.
pub fn is_preemption_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Reap one *dead* (exited) child of `parent_pid`: free its scheduler slot and
/// return its pid + PML4 so the caller can tear down the address space + the
/// `Process`. Returns `None` if the parent has no exited-unreaped child.
pub fn reap_dead_child(parent_pid: u32) -> Option<(u32, u64)> {
    unsafe {
        interrupts::without_interrupts(|| {
            let t = tcbs();
            let n = (&raw const N).read();
            for i in 1..n {
                if (*t.add(i)).parent_pid == parent_pid && (*t.add(i)).state == RunState::Dead {
                    let pid = (*t.add(i)).pid;
                    let pml4 = (*t.add(i)).pml4;
                    (*t.add(i)) = TCB_INIT; // free the slot
                    return Some((pid, pml4));
                }
            }
            None
        })
    }
}

/// Whether `parent_pid` has any child still tracked by the scheduler (alive,
/// blocked, or exited-unreaped). False ⇒ `wait` should return "no children".
pub fn has_children(parent_pid: u32) -> bool {
    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        for i in 1..n {
            if (*t.add(i)).parent_pid == parent_pid && (*t.add(i)).state != RunState::Free {
                return true;
            }
        }
        false
    }
}

/// Point the *current* process at a new address space (B3 Step 5c `exec`): the
/// process keeps its pid + kernel stack, but its user space is replaced. Updates
/// the TCB's PML4 and switches CR3 now (so the syscall return `iretq`s into the
/// new program). The old address space is abandoned (teardown is Step 5d).
///
/// # Safety
/// `new_pml4` must root a valid address space with the new program + stack
/// mapped USER and the kernel higher-half mirrored.
pub unsafe fn exec_into(new_pml4: u64) {
    let cur = (&raw const CURRENT).read();
    (*tcbs().add(cur)).pml4 = new_pml4;
    (*tcbs().add(cur)).heap_brk = USER_HEAP_BASE; // new image ⇒ fresh, empty brk heap
    paging::switch(new_pml4);
    (&raw mut LAST_PML4).write(new_pml4);
}

/// The current process's program break (B9-1). The `brk` syscall reads this to
/// answer a query and to know where to start growing.
pub fn current_heap_brk() -> u64 {
    unsafe {
        let cur = (&raw const CURRENT).read();
        (*tcbs().add(cur)).heap_brk
    }
}

/// Set the current process's program break (B9-1), after the `brk` syscall has
/// mapped/unmapped the heap pages between the old and new break.
pub fn set_current_heap_brk(brk: u64) {
    unsafe {
        let cur = (&raw const CURRENT).read();
        (*tcbs().add(cur)).heap_brk = brk;
    }
}

/// The program break of the process with pid `pid` (used to let a `fork`ed child
/// inherit its parent's break). `None` if no live TCB has that pid.
fn brk_of_pid(pid: u32) -> Option<u64> {
    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        for i in 0..n {
            if (*t.add(i)).pid == pid && (*t.add(i)).state != RunState::Free {
                return Some((*t.add(i)).heap_brk);
            }
        }
        None
    }
}

/// Initialize the scheduler: create the Frame `Scheduler`, reserve the boot
/// context, and capture the kernel address space. Call once before spawning.
pub fn init() {
    unsafe {
        let p = &raw mut SCHED;
        *p = Some(Scheduler::__create());
    }
    init_boot();
}

/// Enable preemption and idle the boot context until every spawned task has
/// exited (the Frame Scheduler reports `$Idle`), then disable preemption.
pub fn run_until_idle() {
    ACTIVE.store(true, Ordering::Relaxed);
    interrupts::enable();
    while !with_sched(|s| s.is_idle()) {
        interrupts::wait_for_interrupt();
    }
    interrupts::disable();
    ACTIVE.store(false, Ordering::Relaxed);
}

/// Exit the calling worker: mark it dead (so the ISR stops scheduling it),
/// fire the Frame Scheduler's `task_unready`, then park. Never returns — the
/// next tick switches away and this thread is never resumed.
pub fn exit_current() -> ! {
    // Mark Dead *and* fire `task_unready` in a single critical section. If
    // these were separate sections a timer tick could land in between:
    // `schedule()` would see this thread is Dead and switch away, and since
    // a Dead thread is never picked again, `task_unready` would never run —
    // the Frame Scheduler would never reach $Idle and the boot loop would
    // hang. (`with_sched`/`without_interrupts` nest safely: the inner cli
    // sees IF already clear and leaves it that way.)
    interrupts::without_interrupts(|| unsafe {
        let cur = (&raw const CURRENT).read();
        (*tcbs().add(cur)).state = RunState::Dead;
        with_sched(|s| s.task_unready());
        // SIGCHLD: wake the parent if it's blocked in wait() — now that this
        // child is Dead, the parent's reap will find it. (No-op if the parent
        // isn't waiting, or for parentless kernel threads.)
        let parent = (*tcbs().add(cur)).parent_pid;
        if parent != 0 {
            let t = tcbs();
            let n = (&raw const N).read();
            for i in 0..n {
                if (*t.add(i)).pid == parent && (*t.add(i)).state == RunState::Blocked {
                    (*t.add(i)).state = RunState::Runnable;
                    break;
                }
            }
        }
    });
    // A dead task MUST park with interrupts enabled so the next timer tick
    // switches away. Kernel-thread callers already run with IF=1, but a user
    // process exits via the syscall path (IF=0, cleared by FMASK), so enable
    // explicitly — `wait_for_interrupt` is a bare `hlt` and would hang at IF=0.
    interrupts::enable();
    loop {
        interrupts::wait_for_interrupt();
    }
}

// ---------------------------------------------------------------------------
// Demo threads
// ---------------------------------------------------------------------------

fn pace() {
    // Small spin: under TCG the 100 Hz timer still preempts mid-spin (TCG
    // runs far slower than host wall-clock), so the threads interleave;
    // keeping it small keeps total boot time well inside the smoke timeout.
    for _ in 0..8_000u64 {
        core::hint::spin_loop();
    }
}

extern "C" fn worker1() -> ! {
    for _ in 0..ROUNDS_PER_WORKER {
        pace();
        serial::write_str("1");
    }
    exit_current();
}

extern "C" fn worker2() -> ! {
    for _ in 0..ROUNDS_PER_WORKER {
        pace();
        serial::write_str("2");
    }
    exit_current();
}

/// Run the preemptive demo to completion: start two non-yielding threads,
/// let preemption interleave them, and once both have exited (the Frame
/// Scheduler reports `$Idle`) halt the demo.
pub fn run() {
    serial::writeln("[preempt] starting two non-yielding threads");
    unsafe {
        let p = &raw mut SCHED;
        *p = Some(Scheduler::__create());
        init_boot();
        let s1 = (&raw mut STACK1).add(1) as *mut u8;
        let s2 = (&raw mut STACK2).add(1) as *mut u8;
        spawn(s1, worker1);
        spawn(s2, worker2);
    }

    ACTIVE.store(true, Ordering::Relaxed);
    interrupts::enable();

    // Idle when both workers have exited — the Frame Scheduler's $Idle
    // state, read here from normal context, drives the halt.
    while !with_sched(|s| s.is_idle()) {
        interrupts::wait_for_interrupt();
    }

    interrupts::disable();
    ACTIVE.store(false, Ordering::Relaxed);

    serial::writeln("\n[preempt] both threads exited; scheduler is $Idle — done");
}

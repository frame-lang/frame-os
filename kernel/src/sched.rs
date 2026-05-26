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

use crate::frame_systems::{IoScheduler, Scheduler};
use crate::{fpu, gdt, interrupts, paging, serial};

const MAX_THREADS: usize = 8;
const STACK_SIZE: usize = 16 * 1024;
const ROUNDS_PER_WORKER: u32 = 4;
/// Max length of a process's stored current-working-directory path (B11-3
/// follow-up). Canonical absolute paths only; 256 matches the syscall path cap.
const CWD_MAX: usize = 256;

#[derive(Clone, Copy, PartialEq)]
enum RunState {
    Free,
    Runnable,
    Blocked, // alive but not runnable (e.g. a parent in wait()) — skipped by the round-robin, still counted "alive"
    Stopped, // job-control suspend (SIGTSTP/SIGSTOP) — skipped by the round-robin until SIGCONT, like Blocked but reported 'T'
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
    // Current working directory: a canonical absolute path (B11-3 follow-up).
    // `fork` copies it; `exec` keeps it (same pid/slot); fresh processes start
    // at "/". Path syscalls resolve relative paths against it.
    cwd: [u8; CWD_MAX],
    cwd_len: u16,
    // POSIX signal state (S10). These are native plumbing (bitmasks + a handler
    // table), *not* a state machine: the signal *lifecycle* a process moves
    // through (Running/Stopped/Terminated) lives in the Process Frame FSM; this
    // is just the per-process pending/blocked sets the delivery path consults.
    // `sig_pending`/`sig_blocked` are bit `sig` (1..=31) of a u32; `sig_handlers`
    // holds the user handler VA per signal (0 = SIG_DFL default action,
    // 1 = SIG_IGN ignore). fork() copies all three; exec() resets handlers to
    // default (a fresh image can't keep the old image's handler addresses) but
    // keeps the blocked mask, per POSIX.
    sig_pending: u32,
    sig_blocked: u32,
    sig_handlers: [u64; NSIG],
    // The user-space "restorer" trampoline VA: a tiny stub (`mov rax, sigreturn;
    // syscall`) the runtime registers via sigaction. The delivery path pushes it
    // as the return address under a handler call, so when the handler returns it
    // `ret`s into the restorer, which invokes sigreturn to restore the
    // interrupted frame. One per process (every signal shares it). 0 ⇒ no
    // restorer registered, so handlers can't be safely invoked (delivery falls
    // back to the default action).
    sig_restorer: u64,
}

const TCB_INIT: Tcb = Tcb {
    rsp: 0,
    state: RunState::Free,
    pml4: 0,
    kstack_top: 0,
    pid: 0,
    parent_pid: 0,
    heap_brk: 0,
    cwd: [0; CWD_MAX],
    cwd_len: 0,
    sig_pending: 0,
    sig_blocked: 0,
    sig_handlers: [0; NSIG],
    sig_restorer: 0,
};

/// Number of signals tracked (0 unused; 1..=31 valid), sized for a u32 mask.
pub const NSIG: usize = 32;
/// SIG_IGN sentinel stored in `sig_handlers` (distinct from 0 = SIG_DFL).
pub const SIG_IGN: u64 = 1;

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

// Per-thread x87/SSE save area, indexed by TCB slot (B11-3a). `schedule()`
// FXSAVEs the outgoing thread's FPU here and FXRSTORs the incoming thread's —
// so the FPU register file is per-thread, exactly like the GPRs. New threads
// are seeded with the clean (post-`fninit`) template (see the spawn paths).
static mut FPU_AREAS: [fpu::FpuState; MAX_THREADS] = [fpu::FpuState::zeroed(); MAX_THREADS];

/// Raw pointer to slot `n`'s FPU save area (for FXSAVE/FXRSTOR).
fn fpu_area(n: usize) -> *mut fpu::FpuState {
    unsafe { ((&raw mut FPU_AREAS) as *mut fpu::FpuState).add(n) }
}

// The Frame Scheduler — guarded by interrupts-off critical sections (it is
// non-reentrant and shared across preemptible threads).
static mut SCHED: Option<Scheduler> = None;

// The IoScheduler supervisor (S6 follow-up): sequences blocking I/O — currently
// the single-flight disk engine's access (who holds it, who's queued, who's
// next). Like SCHED it's a shared non-reentrant Frame system, so every touch is
// inside `without_interrupts` and only ever from syscall/drained context (never
// an ISR).
static mut IO_SCHED: Option<IoScheduler> = None;

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

/// Run `f` with the IoScheduler supervisor, in a critical section.
fn with_io_sched<R>(f: impl FnOnce(&mut IoScheduler) -> R) -> R {
    interrupts::without_interrupts(|| unsafe {
        let p = &raw mut IO_SCHED;
        let s = (*p).as_mut().expect("io scheduler initialized");
        f(s)
    })
}

/// Acquire the single-flight disk engine, blocking (yielding) until this process
/// is its owner. The supervisor decides grant-vs-queue; we then block until it
/// names us owner. The check-and-block in `block_current_until` is atomic, and on
/// any wake we re-ask the supervisor — so a hand-off that races the block is
/// never lost. (S6: replaces the ad-hoc native disk lock.)
pub fn acquire_disk() {
    let pid = current_pid();
    // Boot / non-process context (early boot fs::mount, the idle slot): there's
    // no concurrency and the supervisor may not exist yet — skip it. (pid 0 is
    // also the "no owner" sentinel, so it must never enter the queue.)
    if !is_preemption_active() || pid == 0 {
        return;
    }
    with_io_sched(|s| s.acquire_disk(pid));
    block_current_until(|| with_io_sched(|s| s.disk_owner(pid)));
}

/// Release the disk engine and wake the next queued owner (if any) so it can
/// claim it. Called by the holder after its transaction completes.
pub fn release_disk() {
    if !is_preemption_active() || current_pid() == 0 {
        return; // paired with the bypass in `acquire_disk`
    }
    let next = with_io_sched(|s| s.release_disk());
    if next != 0 {
        wake_pid(next);
    }
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

        // S10 2d: deliver a pending signal to the process being PREEMPTED, if it
        // was running in ring 3 (so it has a deliverable signal but never reaches
        // a syscall boundary on its own — a CPU-bound loop). This runs while CR3
        // is still `cur`'s, so a handler-frame write / RIP rewrite lands in its
        // address space. NATIVE ONLY (no Frame dispatch — the ISR invariant): a
        // terminate redirects RIP to the exit trampoline (the process exits
        // itself at its next syscall), a stop marks it Stopped (handled below by
        // the round-robin skipping non-Runnable), a handler rewrites its frame.
        // Guarded so the common no-pending-signal case is a cheap early-out.
        if (*t.add(cur)).pid != 0
            && (*t.add(cur)).pml4 != 0
            && (*t.add(cur)).sig_pending & !(*t.add(cur)).sig_blocked != 0
        {
            let frame = current_rsp as *mut crate::usermode::TrapFrame;
            if crate::usermode::frame_is_user(frame) {
                crate::usermode::deliver_on_preempt(frame);
            }
        }

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

        // FPU/SSE state is per-thread (B11-3a): save the outgoing thread's
        // x87/SSE registers and restore the incoming thread's, so two
        // preemptively-interleaved FPU users don't clobber each other. Skip the
        // no-op self-switch. (The ISR prologue + the scheduler code above touch
        // no FPU, so the outgoing thread's live FPU is still intact here.)
        if next != cur {
            fpu::save(fpu_area(cur));
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

        if next != cur {
            fpu::restore(fpu_area(next));
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
        fpu_area(0).write(fpu::clean()); // boot context starts with a clean FPU
        let kpml4 = paging::current_pml4();
        (&raw mut KERNEL_PML4).write(kpml4);
        (&raw mut LAST_PML4).write(kpml4);
    }
}

/// Add a worker thread; fires the Frame Scheduler's `task_ready`.
/// Allocate a scheduler slot for a new thread/process: **reuse** the lowest
/// freed worker slot (`reap_dead_child` resets exited slots to `TCB_INIT` ⇒
/// `Free`), or append a fresh one. Returns the slot index; `N` (the high-water
/// mark the round-robin iterates and the per-slot kernel-stack index derive
/// from) is bumped only when appending.
///
/// Reuse is the fix for a slot *leak*: every `spawn_user_from_frame` (fork) used
/// to append (`N += 1`) and reap never shrank `N`, so a shell running enough
/// programs in sequence drove `N` past `MAX_THREADS` and wrote `TCBS[8]` /
/// `KSTACKS[9]` out of bounds — corrupting kernel memory (it crashed the kernel
/// after ~7 sequential programs, e.g. the tcc compile run). With reuse, `N`
/// tracks the *concurrent* thread count (a small number), never the cumulative.
unsafe fn alloc_slot() -> usize {
    let t = tcbs();
    let n = (&raw const N).read();
    for i in 1..n {
        if (*t.add(i)).state == RunState::Free {
            return i; // reuse a reaped slot
        }
    }
    // Append. Slot `n`'s kernel stack is `KSTACKS[n + 1]`, so keep `n + 1`
    // within the fixed array (`n + 1 < MAX_THREADS`) as well as `n`.
    assert!(n + 1 < MAX_THREADS, "scheduler: out of TCB/kstack slots");
    (&raw mut N).write(n + 1);
    n
}

unsafe fn spawn(stack_top: *mut u8, entry: extern "C" fn() -> !) {
    let cs = read_cs();
    let ss = read_ss();
    let rsp = init_thread(stack_top, entry, cs, ss);
    let n = alloc_slot();
    let t = tcbs();
    (*t.add(n)).rsp = rsp;
    (*t.add(n)).state = RunState::Runnable;
    fpu_area(n).write(fpu::clean()); // fresh thread → clean FPU
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
    let n = alloc_slot();
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
    set_slot_cwd(n, b"/"); // fresh process starts at the root directory
    crate::vfs::init_console_fds(n); // fd 0/1/2 = console (stdin/stdout/stderr)
    fpu_area(n).write(fpu::clean()); // fresh process → clean FPU
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
    let n = alloc_slot();
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
    // The child inherits the parent's cwd (POSIX fork semantics).
    let mut pcwd = [0u8; CWD_MAX];
    let pl = cwd_of_pid(parent_pid, &mut pcwd);
    set_slot_cwd(n, if pl > 0 { &pcwd[..pl] } else { b"/" });
    // The child inherits the parent's open file descriptors (POSIX fork). If the
    // parent slot can't be found (shouldn't happen), fall back to fresh console.
    match slot_of_pid(parent_pid) {
        Some(ps) => crate::vfs::clone_fds(n, ps),
        None => crate::vfs::init_console_fds(n),
    }
    // fork inherits the parent's FPU state: the parent is the one running this
    // syscall, so its live x87/SSE registers are the state to copy — FXSAVE them
    // straight into the child's save area.
    fpu::save(fpu_area(n));
    // fork inherits the parent's signal dispositions and blocked mask, but starts
    // with an empty pending set (POSIX). alloc_slot() reset this slot to TCB_INIT
    // (handlers default, masks clear); copy the parent's over the top.
    if let Some(ps) = slot_of_pid(parent_pid) {
        (*t.add(n)).sig_handlers = (*t.add(ps)).sig_handlers;
        (*t.add(n)).sig_blocked = (*t.add(ps)).sig_blocked;
    }
    with_sched(|s| s.task_ready());
}

// (Removed `block_current`: a check-then-block with a gap between the caller's
// readiness test and marking the task Blocked, so a wake landing in that gap was
// lost. `wait` was its last user and intermittently hung the shell because of it;
// it now uses `block_current_until`, which folds the check and the block into one
// interrupts-off step. Use `block_current_until` for all blocking.)

/// Block the current process until `ready()` returns true, yielding the CPU
/// between checks. Unlike `block_current`, the decision to keep waiting is made
/// *atomically* with marking the task Blocked (interrupts off), and `ready()` is
/// re-evaluated after every wake — so this is immune to lost wakeups (a wake that
/// arrives between the check and the block) and to early/spurious wakes (it
/// sleeps again while `ready()` is still false). The disk driver uses it to wait
/// on a real DMA completion (the device-written status byte) rather than on the
/// mere fact of being woken, which previously let a sector write return before it
/// had committed. Restores IF=0 on return (the caller is a syscall handler).
pub fn block_current_until(ready: impl Fn() -> bool) {
    unsafe {
        let cur = (&raw const CURRENT).read();
        loop {
            // Check the completion condition and decide to block in one
            // interrupts-off step: a completion IRQ can't slip in between.
            let done = interrupts::without_interrupts(|| {
                if ready() {
                    true
                } else {
                    (*tcbs().add(cur)).state = RunState::Blocked;
                    false
                }
            });
            if done {
                interrupts::disable();
                return;
            }
            // Yield until something wakes us, then loop to re-check `ready()`.
            while (*tcbs().add(cur)).state == RunState::Blocked {
                interrupts::wait_for_interrupt_enabled();
            }
        }
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

/// The scheduler slot of the currently-running context. The per-process fd table
/// (`vfs`) is indexed by this so each process sees its own descriptors.
pub fn current_slot() -> usize {
    unsafe { (&raw const CURRENT).read() }
}

/// The parent pid of process `pid` (0 if none / not found). Used by the signal
/// path to notify a parent (SIGCHLD) when a child stops.
pub fn parent_of(pid: u32) -> Option<u32> {
    slot_of_pid(pid).map(|i| unsafe { (*tcbs().add(i)).parent_pid })
}

/// The scheduler slot owning process `pid` (skipping freed slots), or None.
fn slot_of_pid(pid: u32) -> Option<usize> {
    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        for i in 0..n {
            if (*t.add(i)).pid == pid && (*t.add(i)).state != RunState::Free {
                return Some(i);
            }
        }
        None
    }
}

/// Snapshot the live process table (S9 `ps`): fill `out` with a
/// `(pid, parent_pid, state_code)` tuple for each non-free slot that owns a
/// real process (`pid != 0` — skips the boot/idle context in slot 0), and
/// return the count written (capped at `out.len()`). State codes:
/// `1`=Runnable, `2`=Blocked, `3`=Dead/zombie. Taken with interrupts off so the
/// snapshot is consistent against a concurrent fork/exit. Pure read — no
/// lifecycle, so it's native (not a Frame system).
pub fn live_procs(out: &mut [(u32, u32, u8)]) -> usize {
    unsafe {
        interrupts::without_interrupts(|| {
            let t = tcbs();
            let n = (&raw const N).read();
            let mut k = 0usize;
            for i in 0..n {
                if k >= out.len() {
                    break;
                }
                let tcb = &*t.add(i);
                if tcb.pid == 0 || tcb.state == RunState::Free {
                    continue;
                }
                let code = match tcb.state {
                    RunState::Runnable => 1u8,
                    RunState::Blocked => 2,
                    RunState::Dead => 3,
                    RunState::Stopped => 4, // ps reports 'T'
                    RunState::Free => continue,
                };
                out[k] = (tcb.pid, tcb.parent_pid, code);
                k += 1;
            }
            k
        })
    }
}

// ---------------------------------------------------------------------------
// Signals (S10) — native plumbing around the Process lifecycle FSM
// ---------------------------------------------------------------------------
//
// Generation sets a pending bit on the target's TCB; delivery happens to the
// *current* process at the syscall-return boundary (usermode::syscall_dispatch),
// where there's a full TrapFrame to either redirect to a handler or act on. A
// signal sent to a process blocked in a syscall flips it Runnable so it returns
// from the block and the delivery check fires. (Interrupting an in-progress
// blocking syscall with EINTR is a documented follow-up; today the block simply
// completes normally, then the pending signal is delivered.)

/// Whether a live process with `pid` exists (POSIX `kill(pid, 0)` existence
/// check). False for a freed/never-existed pid.
pub fn signal_exists(pid: u32) -> bool {
    slot_of_pid(pid).is_some()
}

/// Send signal `sig` (1..=31) to process `pid`: set the pending bit. If the
/// target is Blocked, flip it Runnable so it returns from its blocking syscall
/// and the delivery check at the syscall boundary fires. Returns false if no
/// such live process. Pure native — safe from an interrupt handler (the console
/// RX IRQ calls this for Ctrl-C/Ctrl-Z).
pub fn send_signal(pid: u32, sig: u32) -> bool {
    if sig == 0 || sig as usize >= NSIG {
        return signal_exists(pid); // sig 0 = existence check, no delivery
    }
    unsafe {
        interrupts::without_interrupts(|| match slot_of_pid(pid) {
            Some(i) => {
                let t = tcbs();
                (*t.add(i)).sig_pending |= 1 << sig;
                let st = (*t.add(i)).state;
                if st == RunState::Blocked {
                    // Wake a blocked target so it returns from its syscall and
                    // takes delivery at the boundary.
                    (*t.add(i)).state = RunState::Runnable;
                } else if st == RunState::Stopped
                    && (sig == SIGCONT_NUM || sig == SIGKILL_NUM)
                {
                    // SIGCONT resumes a stopped process; SIGKILL also un-stops it
                    // so it runs far enough to take the (unblockable) kill at its
                    // next boundary. The resume is the *sender's* action — the
                    // stopped target can't process a signal while off-CPU.
                    (*t.add(i)).state = RunState::Runnable;
                    // "continued" is logged HERE, on the resume, so it's emitted
                    // exactly once regardless of HOW the process was stopped — the
                    // syscall-path park (do_stop_current) and the timer-path mark
                    // (deliver_on_preempt) both just become Runnable here. (Not for
                    // SIGKILL: that un-stop is to let it die, not resume.)
                    if sig == SIGCONT_NUM {
                        serial::write_str("[signal] pid ");
                        serial::write_u32_decimal(pid);
                        serial::writeln(" continued");
                    }
                }
                true
            }
            None => false,
        })
    }
}

/// Stop the *current* process (SIGTSTP/SIGSTOP default action): mark it Stopped
/// and park until a SIGCONT (or SIGKILL) flips it Runnable again. Mirrors
/// `block_current_until` but for job-control suspend — the round-robin skips
/// Stopped, so the CPU goes to other work; the parked kernel context resumes
/// here when `send_signal` un-stops us. Stopping is unconditional (no readiness
/// predicate), so there's no lost-wakeup window. Returns with IF=0 (the caller
/// is the signal-delivery path inside a syscall handler).
/// Mark the current process Stopped WITHOUT parking (S10 2d). Used by the
/// timer-path delivery from inside `schedule()`, which can't park — it just
/// flags the state and lets the round-robin deschedule it (a later SIGCONT flips
/// it Runnable). Contrast `stop_current_and_wait`, which parks the caller.
pub fn mark_current_stopped() {
    unsafe {
        let cur = (&raw const CURRENT).read();
        (*tcbs().add(cur)).state = RunState::Stopped;
    }
}

pub fn stop_current_and_wait() {
    unsafe {
        let cur = (&raw const CURRENT).read();
        interrupts::without_interrupts(|| {
            (*tcbs().add(cur)).state = RunState::Stopped;
        });
        while (*tcbs().add(cur)).state == RunState::Stopped {
            interrupts::wait_for_interrupt_enabled();
        }
        interrupts::disable();
    }
}

/// Pop the lowest-numbered deliverable signal for the *current* process —
/// pending and not blocked — clearing its pending bit. Returns 0 if none.
/// Called by the delivery path at the syscall-return boundary.
pub fn take_deliverable_signal() -> u32 {
    unsafe {
        interrupts::without_interrupts(|| {
            let cur = (&raw const CURRENT).read();
            let t = tcbs();
            let deliverable = (*t.add(cur)).sig_pending & !(*t.add(cur)).sig_blocked;
            if deliverable == 0 {
                return 0;
            }
            let sig = deliverable.trailing_zeros(); // lowest set bit = lowest signal
            (*t.add(cur)).sig_pending &= !(1 << sig);
            sig
        })
    }
}

/// The current process's handler VA for `sig`: 0 = SIG_DFL (default action),
/// 1 (SIG_IGN) = ignore, else a user handler address.
pub fn signal_handler(sig: u32) -> u64 {
    if sig as usize >= NSIG {
        return 0;
    }
    unsafe {
        let cur = (&raw const CURRENT).read();
        (*tcbs().add(cur)).sig_handlers[sig as usize]
    }
}

/// Set the current process's handler for `sig` (0 = SIG_DFL, 1 = SIG_IGN, else a
/// user VA). Returns the previous handler. Used by the sigaction syscall (2b).
pub fn set_signal_handler(sig: u32, handler: u64) -> u64 {
    if sig as usize >= NSIG {
        return 0;
    }
    unsafe {
        let cur = (&raw const CURRENT).read();
        let prev = (*tcbs().add(cur)).sig_handlers[sig as usize];
        (*tcbs().add(cur)).sig_handlers[sig as usize] = handler;
        prev
    }
}

// Resetting signal *dispositions* on exec is done inline in `exec_into` (a fresh
// image can't keep the old one's handler VAs), alongside the brk + FPU resets —
// pending + blocked sets persist there per POSIX.

/// The current process's registered restorer trampoline VA (0 = none).
pub fn signal_restorer() -> u64 {
    unsafe {
        let cur = (&raw const CURRENT).read();
        (*tcbs().add(cur)).sig_restorer
    }
}

/// Register the current process's restorer trampoline VA (sigaction supplies it
/// once; all signals share it). A no-op if `restorer` is 0.
pub fn set_signal_restorer(restorer: u64) {
    if restorer == 0 {
        return;
    }
    unsafe {
        let cur = (&raw const CURRENT).read();
        (*tcbs().add(cur)).sig_restorer = restorer;
    }
}

/// Apply a sigprocmask operation to the current process. `how`: 0 = SETMASK
/// (replace), 1 = BLOCK (OR in), 2 = UNBLOCK (clear). SIGKILL/SIGSTOP can't be
/// blocked (POSIX) — those bits are always cleared. Returns the previous mask.
pub fn set_signal_mask(how: u32, mask: u32) -> u32 {
    unsafe {
        let cur = (&raw const CURRENT).read();
        let prev = (*tcbs().add(cur)).sig_blocked;
        let mut next = match how {
            1 => prev | mask,  // BLOCK
            2 => prev & !mask, // UNBLOCK
            _ => mask,         // SETMASK (0)
        };
        next &= !((1 << SIGKILL_NUM) | (1 << SIGSTOP_NUM)); // can't block these
        (*tcbs().add(cur)).sig_blocked = next;
        prev
    }
}

/// SIGKILL / SIGSTOP signal numbers — unmaskable per POSIX (mirrors the values
/// named in usermode; kept here so set_signal_mask can enforce the rule without
/// a cross-module dependency).
const SIGKILL_NUM: u32 = 9;
const SIGSTOP_NUM: u32 = 19;
const SIGCONT_NUM: u32 = 18;

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
                    crate::vfs::clear_fds(i); // close the reaped process's fds
                    (*t.add(i)) = TCB_INIT; // free the slot
                    return Some((pid, pml4));
                }
            }
            None
        })
    }
}

/// Reap the *specific* dead child `target` of `parent_pid` (vs `reap_dead_child`,
/// which takes any one). Returns its (pid, pml4) if it was a Dead child, else
/// None. `waitpid(target)` uses this so it reaps ONLY what it waited on, leaving
/// other dead children (e.g. finished background jobs) as zombies for the shell's
/// prompt-time harvest to collect — which is what keeps the job table's
/// "[id]+ Done" reports from being silently lost to a foreground wait (S10 fix).
pub fn reap_dead_pid(parent_pid: u32, target: u32) -> Option<(u32, u64)> {
    unsafe {
        interrupts::without_interrupts(|| {
            let t = tcbs();
            let n = (&raw const N).read();
            for i in 1..n {
                if (*t.add(i)).parent_pid == parent_pid
                    && (*t.add(i)).pid == target
                    && (*t.add(i)).state == RunState::Dead
                {
                    let pid = (*t.add(i)).pid;
                    let pml4 = (*t.add(i)).pml4;
                    crate::vfs::clear_fds(i);
                    (*t.add(i)) = TCB_INIT;
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

/// Whether `parent_pid` still has a specific child `child_pid` tracked (alive,
/// blocked, or exited-unreaped). False ⇒ `waitpid(child_pid)` returns ECHILD
/// (the child was already reaped, or never belonged to this parent).
pub fn has_child(parent_pid: u32, child_pid: u32) -> bool {
    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        for i in 1..n {
            let tcb = &*t.add(i);
            if tcb.parent_pid == parent_pid && tcb.pid == child_pid && tcb.state != RunState::Free {
                return true;
            }
        }
        false
    }
}

/// Whether `parent_pid` has a *reapable* (exited, `Dead`) child matching
/// `target` (`0` = any child). This is the predicate `wait` blocks on via
/// `block_current_until`, so the check-and-block is atomic against a child
/// exiting (which sets it `Dead` and wakes us) — no lost wakeup.
pub fn child_reapable(parent_pid: u32, target: u32) -> bool {
    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        for i in 1..n {
            let tcb = &*t.add(i);
            if tcb.parent_pid == parent_pid
                && tcb.state == RunState::Dead
                && (target == 0 || tcb.pid == target)
            {
                return true;
            }
        }
        false
    }
}

/// Whether `parent_pid` has a *stopped* (job-control suspended) child matching
/// `target` (`0` = any). The wait path blocks on this too (POSIX WUNTRACED), so
/// the foreground shell wakes when the job it's waiting on is Ctrl-Z'd.
pub fn child_stopped(parent_pid: u32, target: u32) -> bool {
    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        for i in 1..n {
            let tcb = &*t.add(i);
            if tcb.parent_pid == parent_pid
                && tcb.state == RunState::Stopped
                && (target == 0 || tcb.pid == target)
            {
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
    // New image ⇒ signal handlers reset to default (the old image's handler VAs
    // are meaningless in the new one). Pending + blocked sets persist (POSIX).
    (*tcbs().add(cur)).sig_handlers = [0; NSIG];
                                                  // New image ⇒ fresh FPU: reset both the live registers and the saved area to
                                                  // the clean template, so the old image's x87/SSE state (esp. MXCSR) can't
                                                  // leak into the new program before its first context switch (B11-3a).
    let c = fpu::clean();
    fpu_area(cur).write(c);
    fpu::restore(fpu_area(cur));
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

// --- per-process current working directory (B11-3 follow-up) ---------------

/// Store `path` (a canonical absolute path, ≤ CWD_MAX bytes) as TCB slot `n`'s
/// cwd. Over-long paths are rejected by the caller (chdir), so this truncates
/// defensively only.
unsafe fn set_slot_cwd(n: usize, path: &[u8]) {
    let len = path.len().min(CWD_MAX);
    let t = tcbs();
    (&mut (*t.add(n)).cwd)[..len].copy_from_slice(&path[..len]);
    (*t.add(n)).cwd_len = len as u16;
}

/// Copy the current process's cwd into `out`, returning the byte length written
/// (0 if unset — callers treat that as "/").
pub fn cwd_current(out: &mut [u8]) -> usize {
    unsafe {
        let cur = (&raw const CURRENT).read();
        let t = tcbs();
        let len = ((*t.add(cur)).cwd_len as usize).min(out.len());
        out[..len].copy_from_slice(&(&(*t.add(cur)).cwd)[..len]);
        len
    }
}

/// Set the current process's cwd to `path` (a canonical absolute path). Returns
/// false if it doesn't fit in CWD_MAX.
pub fn set_cwd_current(path: &[u8]) -> bool {
    if path.len() > CWD_MAX {
        return false;
    }
    unsafe {
        let cur = (&raw const CURRENT).read();
        set_slot_cwd(cur, path);
    }
    true
}

/// Copy the cwd of the process with pid `pid` into `out` (used so a `fork`ed
/// child inherits its parent's cwd). Returns the byte length, 0 if not found.
fn cwd_of_pid(pid: u32, out: &mut [u8]) -> usize {
    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        for i in 0..n {
            if (*t.add(i)).pid == pid && (*t.add(i)).state != RunState::Free {
                let len = ((*t.add(i)).cwd_len as usize).min(out.len());
                out[..len].copy_from_slice(&(&(*t.add(i)).cwd)[..len]);
                return len;
            }
        }
        0
    }
}

/// Initialize the scheduler: create the Frame `Scheduler`, reserve the boot
/// context, and capture the kernel address space. Call once before spawning.
pub fn init() {
    unsafe {
        let p = &raw mut SCHED;
        *p = Some(Scheduler::__create());
        let q = &raw mut IO_SCHED;
        *q = Some(IoScheduler::__create());
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

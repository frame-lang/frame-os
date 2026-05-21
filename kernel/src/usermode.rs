// kernel/src/usermode.rs
//
// Ring 3 + `syscall`/`sysret`, and user processes as preemptible, scheduled
// entities (B3 Steps 1b–5a). Pure native — the user/kernel boundary.
//
// Step 5a turned user programs into real scheduled processes: each gets its
// own address space (PML4) and ring-0 kernel stack, the scheduler switches
// CR3 + TSS.RSP0 on every switch (sched.rs), and a process first enters ring 3
// via the scheduler's synthetic `iretq` frame — not a one-shot `enter_user`.
// A process is preemptible in ring 3 (IF=1); it leaves by `exit` (or a fatal
// fault) which marks it dead and yields to the scheduler, no longjmp.
//
// Single-core simplification (locked B3 decision): the syscall entry switches
// to the current process's kernel stack via a static (`CURRENT_KSTACK`, owned
// by sched) rather than swapgs + per-CPU GS. Syscalls run with IF=0 (FMASK
// clears it), so they aren't preempted and the single `USER_RSP_SAVE` is safe.
// Per-CPU GS arrives at B7 (SMP).
//
// Syscall ABI: rax = number, args in rdi/rsi, return in rax.
//   0 = write_char(rdi = byte) → serial; returns 1
//   1 = exit(rdi = code)       → mark the Process $Zombie + yield (never returns)

use core::arch::{asm, global_asm};

use crate::frame_systems::{ElfLoader, ProcessTable, SyscallDispatcher};
use crate::{paging, sched, serial};

// The syscall dispatcher HSM (B3 Step 2). Driven synchronously from the
// syscall entry; single instance, single-core.
static mut DISPATCHER: Option<SyscallDispatcher> = None;

// The process table (B3 Step 3): one global instance holding the Process
// lifecycle for every user process the scheduler runs.
static mut PROC_TABLE: Option<ProcessTable> = None;

const MAX_PROCS: u32 = 64;

// MSR numbers.
const IA32_EFER: u32 = 0xC000_0080;
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;

// The syscall entry stub saves the user rsp here across the kernel call. A
// single global is safe: syscalls run with IF=0 (no preemption), so only one
// is ever in flight. (The per-process kernel *stack* it switches to is
// `CURRENT_KSTACK`, owned + updated by the scheduler.)
#[no_mangle]
static mut USER_RSP_SAVE: u64 = 0;

// The trap frame of the syscall currently being serviced (set by
// syscall_dispatch). `fork` reads it to copy the caller's full register state
// into the child. Safe as a single global: syscalls run with IF=0 (no
// preemption), so only one is ever in flight.
static mut CURRENT_TRAP_FRAME: *mut TrapFrame = core::ptr::null_mut();

// An `exit` syscall records its code here rather than diverging inside the
// SyscallDispatcher handler — diverging there would leave the (shared, global)
// dispatcher stuck in $Executing, corrupting it for the next process. `>= 0`
// means an exit is pending; `syscall_dispatch` honors it AFTER the Frame
// dispatch returns cleanly to $Validating. (IF=0 in syscalls, so single-flight.)
static mut PENDING_EXIT: i64 = -1;

// Likewise, `wait` BLOCKS — which must not happen inside the SyscallDispatcher
// handler (it would hold the shared dispatcher in $Executing, so a concurrent
// child's syscalls would be dropped). The handler sets this flag; the actual
// block + reap happens in `syscall_dispatch` after the dispatch completes.
static mut PENDING_WAIT: bool = false;

// The freestanding user programs (B3 Step 4), built by kernel/build.rs from
// the `user/` crate and baked into the kernel image. `hello` prints "hello
// from ELF" and exit(42)s; `faulter` reads kernel memory to trigger the
// isolation path (#PF U/S set → killed, kernel survives).
static USER_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_hello.elf"));
static USER_FAULTER_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_faulter.elf"));
// `forker` (B3 Step 5b) forks into two concurrent processes.
static USER_FORKER_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_forker.elf"));
// `spawner` (B3 Step 5c) forks + execs `hello` in the child.
static USER_SPAWNER_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_spawner.elf"));
// `waiter` (B3 Step 5d) forks a child and wait()s to reap it.
static USER_WAITER_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_waiter.elf"));

global_asm!(
    // syscall entry: rcx=user RIP, r11=user RFLAGS, rax=num, rdi/rsi=args.
    // Switch to the current process's kernel stack (CURRENT_KSTACK), then build
    // a FULL trap frame identical in layout to the timer ISR's (15 GPRs + the
    // iretq frame), pass its address to `syscall_dispatch`, restore, and return
    // via `iretq` (not sysret). The uniform frame is what lets `fork` copy a
    // process's complete user state for the child. IF is 0 here (FMASK), so the
    // single USER_RSP_SAVE is safe and the syscall isn't preempted mid-flight.
    ".global syscall_entry",
    "syscall_entry:",
    "  mov [rip + USER_RSP_SAVE], rsp",
    "  mov rsp, [rip + CURRENT_KSTACK]",
    // iretq frame (high→low): SS, RSP, RFLAGS, CS, RIP. `syscall` left the user
    // RIP in rcx and RFLAGS in r11; the user RSP is in USER_RSP_SAVE.
    "  push 0x1b",                            // SS  = USER_DATA | 3
    "  push qword ptr [rip + USER_RSP_SAVE]", // user RSP
    "  push r11",                             // RFLAGS
    "  push 0x23",                            // CS  = USER_CODE | 3
    "  push rcx",                             // RIP
    // 15 GPRs, same order as isr_timer (rax first → r15 last). rcx/r11 here are
    // the syscall-clobbered values (RIP/RFLAGS); harmless — the ABI says the
    // user treats them as clobbered.
    "  push rax",
    "  push rbx",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push rbp",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  push r12",
    "  push r13",
    "  push r14",
    "  push r15",
    "  mov rdi, rsp",          // arg0 = &TrapFrame (points at saved r15)
    "  call syscall_dispatch", // reads num/args from the frame, writes frame.rax
    "  pop r15",
    "  pop r14",
    "  pop r13",
    "  pop r12",
    "  pop r11",
    "  pop r10",
    "  pop r9",
    "  pop r8",
    "  pop rbp",
    "  pop rdi",
    "  pop rsi",
    "  pop rdx",
    "  pop rcx",
    "  pop rbx",
    "  pop rax",
    "  iretq",
);

extern "C" {
    fn syscall_entry();
}

/// The full user register state captured on every syscall/interrupt entry —
/// 15 GPRs plus the `iretq` frame, laid out to match the push order in
/// `syscall_entry` and `isr_timer`. `fork` copies one of these to give the
/// child the parent's exact resumption state (with rax forced to 0).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

fn wrmsr(msr: u32, val: u64) {
    let lo = val as u32;
    let hi = (val >> 32) as u32;
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") lo, in("edx") hi, options(nostack, preserves_flags));
    }
}

fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nostack, preserves_flags));
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Program the syscall MSRs: enable SCE, set the segment bases (STAR), the
/// entry point (LSTAR), and the RFLAGS mask (clear IF on entry). The syscall
/// stack is per-process (`CURRENT_KSTACK`, set by the scheduler), so there's
/// no static syscall stack to set up here.
fn init() {
    wrmsr(IA32_EFER, rdmsr(IA32_EFER) | 1); // SCE
    wrmsr(IA32_STAR, (0x08u64 << 32) | (0x10u64 << 48));
    wrmsr(IA32_LSTAR, syscall_entry as *const () as u64);
    wrmsr(IA32_FMASK, 0x200); // clear IF on syscall entry
    unsafe {
        let p = &raw mut DISPATCHER;
        *p = Some(SyscallDispatcher::__create());
    }
}

/// Rust half of the syscall entry stub. Reads the syscall number + args from
/// the trap frame, routes them through the `SyscallDispatcher` HSM (validate →
/// execute, or `=> $^` reject), and writes the result back into `frame.rax`
/// (the stub restores it on the way out). The frame pointer is stashed in
/// `CURRENT_TRAP_FRAME` so `fork` can copy the caller's full state.
#[no_mangle]
extern "C" fn syscall_dispatch(frame: *mut TrapFrame) {
    unsafe {
        (&raw mut CURRENT_TRAP_FRAME).write(frame);
    }
    let f = unsafe { &mut *frame };
    let (num, a0, a1) = (f.rax, f.rdi, f.rsi);
    let d = unsafe {
        let p = &raw mut DISPATCHER;
        (*p).as_mut().expect("dispatcher initialized")
    };
    d.request(num, a0, a1);
    f.rax = d.result();

    // Honor a pending wait AFTER the dispatcher is back in $Validating — the
    // block must not happen inside the handler. do_wait_loop blocks until a
    // child exits, reaps it, and returns the status into the caller's frame.
    if unsafe { (&raw const PENDING_WAIT).read() } {
        unsafe {
            (&raw mut PENDING_WAIT).write(false);
        }
        f.rax = do_wait_loop();
    }

    // Honor a pending exit AFTER the dispatcher has returned to $Validating —
    // diverging inside the handler would leave it stuck in $Executing.
    let pending = unsafe { (&raw const PENDING_EXIT).read() };
    if pending >= 0 {
        unsafe {
            (&raw mut PENDING_EXIT).write(-1);
        }
        do_exit(pending as i32); // prints, $Zombie, yields — never returns
    }
}

/// Borrow the global process table.
fn proc_table() -> &'static mut ProcessTable {
    unsafe {
        let p = &raw mut PROC_TABLE;
        (*p).as_mut().expect("process table initialized")
    }
}

/// Validation predicate, called by the dispatcher's `$Validating` state.
/// 0 = write_char, 1 = exit, 2 = fork, 3 = exec(prog_id), 4 = wait.
pub fn is_known_syscall(num: u64) -> bool {
    num <= 4
}

/// Perform a (validated) syscall, called by the dispatcher's `$Executing`
/// enter handler. `write_char` returns 1; `exit` marks the process `$Zombie`
/// and yields to the scheduler (never returns); `fork` returns the child pid.
pub fn perform_syscall(num: u64, a0: u64, _a1: u64) -> u64 {
    match num {
        0 => {
            serial::write_byte(a0 as u8);
            1
        }
        1 => {
            // Record the exit; the actual teardown + yield happens in
            // syscall_dispatch once the dispatcher is back in $Validating
            // (diverging here would corrupt the shared dispatcher).
            unsafe {
                (&raw mut PENDING_EXIT).write(a0 as i64);
            }
            0
        }
        2 => do_fork(),
        3 => do_exec(a0),
        4 => {
            // Record the wait; syscall_dispatch runs the (blocking) reap loop
            // after the dispatcher returns to $Validating.
            unsafe {
                (&raw mut PENDING_WAIT).write(true);
            }
            0
        }
        _ => u64::MAX, // unreachable: validated by is_known_syscall
    }
}

/// `wait`: block until a child exits, reap it (collect status, free its
/// `Process` slot + address space), and return its exit code. Returns
/// `u64::MAX` (ECHILD) if the caller has no children. The blocking is the one
/// place a syscall suspends: `sched::block_current` yields to the scheduler and
/// returns once a child's exit (SIGCHLD) wakes us. Called from `syscall_dispatch`
/// (not the handler) so the shared dispatcher stays available to the child.
fn do_wait_loop() -> u64 {
    let me = sched::current_pid();
    loop {
        if let Some((child_pid, child_pml4)) = sched::reap_dead_child(me) {
            let status = proc_table().reap_pid(child_pid); // $Zombie → $Reaped, slot freed
            unsafe { paging::free_address_space(child_pml4) }; // teardown
            serial::write_str("[wait] pid ");
            serial::write_u32_decimal(me);
            serial::write_str(" reaped child pid ");
            serial::write_u32_decimal(child_pid);
            serial::write_str(" (exit ");
            write_exit_code(status);
            serial::write_str("); table count ");
            serial::write_u32_decimal(proc_table().count());
            serial::writeln("");
            return status as u64;
        }
        if !sched::has_children(me) {
            return u64::MAX; // ECHILD
        }
        // A living child exists but none have exited yet — block until one does.
        sched::block_current();
    }
}

/// Map an `exec` program id to its baked ELF. (No filesystem yet — programs are
/// selected by id; B4 replaces this with loading from disk.)
fn exec_elf(prog_id: u64) -> Option<&'static [u8]> {
    match prog_id {
        0 => Some(USER_ELF), // "hello"
        _ => None,
    }
}

/// `exec`: replace the calling process's image with a freshly loaded program.
/// The process keeps its pid + kernel stack; its address space + trap frame are
/// replaced so the syscall returns into the new program at its entry. On an
/// unknown program id, returns u64::MAX and the caller keeps running.
fn do_exec(prog_id: u64) -> u64 {
    let Some(elf) = exec_elf(prog_id) else {
        return u64::MAX;
    };
    // Load the new program into a fresh address space.
    let new_pml4 = unsafe { paging::new_address_space() };
    crate::elf::prepare(elf, new_pml4);
    let mut loader = ElfLoader::__create();
    if loader.is_failed() {
        return u64::MAX;
    }
    let entry = loader.entry();
    let user_rsp = loader.user_stack_top();

    serial::write_str("[exec] pid ");
    serial::write_u32_decimal(sched::current_pid());
    serial::write_str(" exec'd program ");
    serial::write_u32_decimal(prog_id as u32);
    serial::writeln("");

    // Swap the current process onto the new address space, then reset its trap
    // frame to enter the new program fresh (zeroed GPRs, new RIP/RSP). The
    // syscall stub's iretq returns into it. rax is set to 0 by syscall_dispatch.
    unsafe {
        sched::exec_into(new_pml4);
        let f = &mut *(&raw const CURRENT_TRAP_FRAME).read();
        *f = TrapFrame {
            rip: entry,
            rsp: user_rsp,
            cs: 0x23,      // USER_CODE | 3
            ss: 0x1b,      // USER_DATA | 3
            rflags: 0x202, // IF=1
            ..core::mem::zeroed()
        };
    }
    0
}

/// Finish a voluntary `exit`: report it, move the Process to `$Zombie`, and
/// yield to the scheduler (mark dead + park). Never returns — the next timer
/// tick switches away and this process is never resumed. Called from
/// `syscall_dispatch` after the SyscallDispatcher has returned to $Validating.
fn do_exit(code: i32) -> ! {
    serial::write_str("\n[user] exited with code ");
    write_exit_code(code);
    serial::writeln("");
    let pid = sched::current_pid();
    proc_table().exit_pid(pid, code);
    serial::write_str("[proc] pid ");
    serial::write_u32_decimal(pid);
    serial::write_str(" exited -> ");
    serial::writeln(&proc_table().pid_state(pid));
    sched::exit_current()
}

/// `fork`: duplicate the calling process. Eager-copy its address space, copy
/// its trap frame (with rax forced to 0 for the child), admit the child to the
/// scheduler, and return the child's pid to the parent. The child resumes at
/// the fork-return point in ring 3 with rax = 0 (the scheduler `iretq`s it from
/// the copied frame); it never runs this code.
fn do_fork() -> u64 {
    // Copy the caller's trap frame (set by syscall_dispatch).
    let parent_frame = unsafe {
        let p = (&raw const CURRENT_TRAP_FRAME).read();
        *p
    };
    let child_pml4 = unsafe { paging::fork_address_space(paging::current_pml4()) };
    let child_pid = proc_table().spawn(); // child Process: $Created → $Ready
    let parent_pid = sched::current_pid();
    let mut child_frame = parent_frame;
    child_frame.rax = 0; // fork() returns 0 in the child
    unsafe {
        sched::spawn_user_from_frame(child_pml4, &child_frame, child_pid, parent_pid);
    }
    serial::write_str("[fork] pid ");
    serial::write_u32_decimal(parent_pid);
    serial::write_str(" forked child pid ");
    serial::write_u32_decimal(child_pid);
    serial::writeln("");
    child_pid as u64 // fork() returns the child pid in the parent
}

/// Kill the currently-running user process from inside the #PF handler (B3
/// Step 4b). Marks the process `$Zombie` (killed sentinel), then yields to the
/// scheduler — abandoning the faulting ring-3 thread and the #PF stack. Never
/// returns. The kernel survives a misbehaving user program.
pub fn kill_current_user_process() -> ! {
    let pid = sched::current_pid();
    proc_table().kill_pid(pid); // → $Zombie (exit_code = -1)
    serial::write_str("[proc] pid ");
    serial::write_u32_decimal(pid);
    serial::write_str(" killed -> ");
    serial::writeln(&proc_table().pid_state(pid));
    sched::exit_current() // mark dead + yield; never returns
}

/// Print an i32 exit code (negative for a killed process).
fn write_exit_code(code: i32) {
    if code < 0 {
        serial::write_byte(b'-');
        serial::write_u32_decimal((-code) as u32);
    } else {
        serial::write_u32_decimal(code as u32);
    }
}

/// Load one baked ELF into a fresh address space, admit it as a scheduled
/// `Process`, and run it under the preemptive scheduler until it leaves the CPU
/// (clean `exit` or a fatal fault that kills it). Then reap it.
///
/// The process is a real scheduled entity: its own PML4 + kernel stack, entered
/// in ring 3 via the scheduler's synthetic `iretq` frame, preemptible by the
/// timer. The boot context idles in `run_until_idle` until the process exits.
fn run_one(elf: &'static [u8], label: &str) {
    // A fresh address space (kernel higher-half mirrored in) for this process.
    let pml4 = unsafe { paging::new_address_space() };
    crate::elf::prepare(elf, pml4);
    let mut loader = ElfLoader::__create();
    if loader.is_failed() {
        serial::write_str("[elf] load failed: ");
        serial::writeln(&loader.error());
        return;
    }
    let entry = loader.entry();
    let user_rsp = loader.user_stack_top();
    serial::write_str("[elf] loaded ");
    serial::write_str(label);
    serial::write_str(", entry 0x");
    serial::write_hex_u64(entry);
    serial::writeln("");

    let pid = proc_table().spawn(); // $Created → $Ready
    serial::write_str("[proc] spawned pid ");
    serial::write_u32_decimal(pid);
    serial::write_str(" (");
    serial::write_str(&proc_table().pid_state(pid));
    serial::writeln(")");

    // Admit to the scheduler and run until it exits (the boot context idles).
    sched::init();
    unsafe {
        sched::spawn_user(pml4, entry, user_rsp, pid);
    }
    serial::writeln("[sched] user process scheduled (preemptible in ring 3)");
    sched::run_until_idle();
    serial::writeln("[sched] user process left the CPU");

    // Reap the process ($Zombie → $Reaped, freeing the table slot).
    // NOTE (Step 5a): the process's address space + mapped frames are leaked
    // here — proper teardown lands with wait()/reap at Step 5d.
    let code = proc_table().reap_pid(pid);
    serial::write_str("[proc] reaped pid ");
    serial::write_u32_decimal(pid);
    serial::write_str("; exit ");
    write_exit_code(code);
    serial::write_str("; table count ");
    serial::write_u32_decimal(proc_table().count());
    serial::writeln("");
}

/// Run the user-mode demo: set up syscall MSRs and the process table, then run
/// two baked programs as scheduled processes — `hello` (clean exit) and
/// `faulter` (reads kernel memory → #PF → killed, kernel survives).
pub fn run() {
    init();

    unsafe {
        let p = &raw mut PROC_TABLE;
        *p = Some(ProcessTable::__create(MAX_PROCS));
    }

    run_one(USER_ELF, "hello");
    run_one(USER_FAULTER_ELF, "faulter");
    run_one(USER_FORKER_ELF, "forker");
    run_one(USER_SPAWNER_ELF, "spawner");
    run_one(USER_WAITER_ELF, "waiter");
}

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

// The freestanding user programs (B3 Step 4), built by kernel/build.rs from
// the `user/` crate and baked into the kernel image. `hello` prints "hello
// from ELF" and exit(42)s; `faulter` reads kernel memory to trigger the
// isolation path (#PF U/S set → killed, kernel survives).
static USER_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_hello.elf"));
static USER_FAULTER_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_faulter.elf"));

global_asm!(
    // syscall entry: rcx=user RIP, r11=user RFLAGS, rax=num, rdi/rsi=args.
    // Switch to the *current process's* kernel stack (CURRENT_KSTACK, owned by
    // the scheduler), marshal args to the SysV ABI, dispatch, restore, sysretq.
    // IF is 0 here (FMASK clears it), so the single USER_RSP_SAVE is safe and
    // the syscall can't be preempted mid-flight.
    ".global syscall_entry",
    "syscall_entry:",
    "  mov [rip + USER_RSP_SAVE], rsp",
    "  mov rsp, [rip + CURRENT_KSTACK]",
    // Preserve the user's registers across the kernel call. Per the syscall
    // ABI only rax (return) and rcx/r11 (clobbered by `syscall` itself) may
    // change; everything else must come back intact. `call syscall_dispatch`
    // is a SysV C call that preserves rbx/rbp/r12-r15 but clobbers the
    // caller-saved set, so we save rcx/r11 (also needed for sysretq) plus the
    // caller-saved GPRs the user may have live across the syscall.
    "  push rcx", // user RIP (sysret needs it in rcx)
    "  push r11", // user RFLAGS (sysret needs it in r11)
    "  push rdi",
    "  push rsi",
    "  push rdx",
    "  push r8",
    "  push r9",
    "  push r10",
    "  mov rdx, rsi",          // arg1  → SysV 3rd
    "  mov rsi, rdi",          // arg0  → SysV 2nd
    "  mov rdi, rax",          // number → SysV 1st
    "  call syscall_dispatch", // result in rax
    "  pop r10",
    "  pop r9",
    "  pop r8",
    "  pop rdx",
    "  pop rsi",
    "  pop rdi",
    "  pop r11",
    "  pop rcx",
    "  mov rsp, [rip + USER_RSP_SAVE]",
    "  sysretq",
);

extern "C" {
    fn syscall_entry();
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

/// Rust half of the syscall entry stub. Routes the syscall through the
/// `SyscallDispatcher` HSM (validate → execute, or `=> $^` reject) and
/// returns its result.
#[no_mangle]
extern "C" fn syscall_dispatch(num: u64, a0: u64, a1: u64) -> u64 {
    let d = unsafe {
        let p = &raw mut DISPATCHER;
        (*p).as_mut().expect("dispatcher initialized")
    };
    d.request(num, a0, a1);
    d.result()
}

/// Borrow the global process table.
fn proc_table() -> &'static mut ProcessTable {
    unsafe {
        let p = &raw mut PROC_TABLE;
        (*p).as_mut().expect("process table initialized")
    }
}

/// Validation predicate, called by the dispatcher's `$Validating` state.
pub fn is_known_syscall(num: u64) -> bool {
    num == 0 || num == 1
}

/// Perform a (validated) syscall, called by the dispatcher's `$Executing`
/// enter handler. `write_char` returns 1; `exit` marks the process `$Zombie`
/// and yields to the scheduler (never returns).
pub fn perform_syscall(num: u64, a0: u64, _a1: u64) -> u64 {
    match num {
        0 => {
            serial::write_byte(a0 as u8);
            1
        }
        1 => {
            serial::write_str("\n[user] exited with code ");
            serial::write_u32_decimal(a0 as u32);
            serial::writeln("");
            // Record the voluntary exit ($Ready → $Zombie), then yield: mark
            // this process dead in the scheduler and park. The next timer tick
            // switches away and this process never resumes. (ProcessTable is a
            // different Frame instance than the SyscallDispatcher we're inside —
            // no reentrancy hazard.)
            let pid = sched::current_pid();
            proc_table().exit_pid(pid, a0 as i32);
            serial::write_str("[proc] pid ");
            serial::write_u32_decimal(pid);
            serial::write_str(" exited -> ");
            serial::writeln(&proc_table().pid_state(pid));
            sched::exit_current() // never returns
        }
        _ => u64::MAX, // unreachable: validated by is_known_syscall
    }
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
}

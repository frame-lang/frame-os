// kernel/src/usermode.rs
//
// Ring 3 + `syscall`/`sysret` (B3 Step 1b). Pure native — the user/kernel
// boundary. The single hardest chunk in the project: MSR setup, the syscall
// entry stub (stack switch + save/restore + `sysretq`), the `iretq` into
// ring 3, and a longjmp back to the kernel on `exit`.
//
// Single-core simplification (locked B3 decision): the syscall entry
// switches stacks via plain statics (USER_RSP_SAVE / KERNEL_SYSCALL_RSP)
// rather than swapgs + per-CPU GS. Per-CPU arrives at B7 (SMP).
//
// Syscall ABI (minimal at Step 1b): rax = number, args in rdi/rsi/...,
// return in rax.
//   0 = write_char(rdi = byte) → serial; returns 1
//   1 = exit(rdi = code)       → report + longjmp back to the kernel

use core::arch::{asm, global_asm};

use crate::frame_systems::{ElfLoader, ProcessTable, SyscallDispatcher};
use crate::{paging, serial};

// The syscall dispatcher HSM (B3 Step 2). Driven synchronously from the
// syscall entry; single instance, single-core.
static mut DISPATCHER: Option<SyscallDispatcher> = None;

// The process table (B3 Step 3). One global instance. The single ring-3
// program below gets a Process entry whose lifecycle the kernel drives:
// spawn → $Ready, then it runs natively (no $Running state — "on the CPU" is
// native scheduler state), then the exit syscall moves it → $Zombie, and the
// kernel reaps it here → $Reaped (freeing the slot). Driving ProcessTable from
// inside a SyscallDispatcher handler is safe: they are independent Frame
// instances, so this is not a non-reentrant re-entry.
static mut PROC_TABLE: Option<ProcessTable> = None;
static mut USER_PROC_PID: u32 = 0;

const MAX_PROCS: u32 = 64;

// MSR numbers.
const IA32_EFER: u32 = 0xC000_0080;
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;

// Statics the asm stubs reference by name (RIP-relative). #[no_mangle] so the
// symbol is exactly these names.
#[no_mangle]
static mut USER_RSP_SAVE: u64 = 0;
#[no_mangle]
static mut KERNEL_SYSCALL_RSP: u64 = 0;
#[no_mangle]
static mut KERNEL_LONGJMP_RSP: u64 = 0;

const SYSCALL_STACK_SIZE: usize = 16 * 1024;
static mut SYSCALL_STACK: [u8; SYSCALL_STACK_SIZE] = [0; SYSCALL_STACK_SIZE];

// The freestanding user program (B3 Step 4), built by kernel/build.rs from the
// `user/` crate and baked into the kernel image. A real ELF — the `ElfLoader`
// HSM parses + maps it, replacing the hand-assembled blob the Step-1b demo
// used. It prints "hello from ELF" via write_char syscalls and exit(42)s.
static USER_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_hello.elf"));

global_asm!(
    // syscall entry: rcx=user RIP, r11=user RFLAGS, rax=num, rdi/rsi=args.
    // Switch to the kernel syscall stack (statics; single-core), marshal
    // args to the SysV ABI, dispatch, restore, sysretq.
    ".global syscall_entry",
    "syscall_entry:",
    "  mov [rip + USER_RSP_SAVE], rsp",
    "  mov rsp, [rip + KERNEL_SYSCALL_RSP]",
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
    // enter_user(rdi = entry VA, rsi = user stack top): save kernel context
    // for the longjmp, build an iretq frame for ring 3 (IF off for the Step
    // 1b demo), iretq.
    ".global enter_user",
    "enter_user:",
    "  push rbp",
    "  push rbx",
    "  push r12",
    "  push r13",
    "  push r14",
    "  push r15",
    "  mov [rip + KERNEL_LONGJMP_RSP], rsp",
    "  push 0x1b", // user SS  (0x18 | 3)
    "  push rsi",  // user RSP
    "  push 0x2",  // RFLAGS (reserved bit1=1, IF=0)
    "  push 0x23", // user CS  (0x20 | 3)
    "  push rdi",  // user RIP
    "  iretq",
    // resume_kernel_after_user(): restore the kernel context saved by
    // enter_user and return to enter_user's caller. Called from the exit
    // syscall — never returns to the syscall handler.
    ".global resume_kernel_after_user",
    "resume_kernel_after_user:",
    "  mov rsp, [rip + KERNEL_LONGJMP_RSP]",
    "  pop r15",
    "  pop r14",
    "  pop r13",
    "  pop r12",
    "  pop rbx",
    "  pop rbp",
    "  ret",
);

extern "C" {
    fn syscall_entry();
    fn enter_user(entry: u64, user_stack_top: u64);
    fn resume_kernel_after_user() -> !;
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
/// entry point (LSTAR), and the RFLAGS mask (clear IF on entry).
fn init() {
    unsafe {
        let top = (&raw mut SYSCALL_STACK).add(1) as u64 & !0xF;
        (&raw mut KERNEL_SYSCALL_RSP).write(top);
    }
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
/// enter handler. `write_char` returns 1; `exit` longjmps back to the kernel
/// (never returns).
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
            // Record the voluntary exit in the process table: $Ready → $Zombie.
            // (This is a different Frame instance than the SyscallDispatcher we
            // are currently dispatching inside — no reentrancy hazard.)
            let pid = unsafe { (&raw const USER_PROC_PID).read() };
            proc_table().exit_pid(pid, a0 as i32);
            serial::write_str("[proc] pid ");
            serial::write_u32_decimal(pid);
            serial::write_str(" exited -> ");
            serial::writeln(&proc_table().pid_state(pid));
            unsafe { resume_kernel_after_user() } // never returns
        }
        _ => u64::MAX, // unreachable: validated by is_known_syscall
    }
}

/// Run the user-mode demo: set up syscall MSRs, load the baked ELF via the
/// `ElfLoader` HSM, admit it as a `Process`, enter ring 3, and return here when
/// the user exits — then reap the process and free the ELF's pages.
pub fn run() {
    init();

    // Load the baked user ELF into the current address space (the ElfLoader
    // phases cascade from construction; it rests in $Done or $Failed).
    let pml4 = paging::current_pml4();
    crate::elf::prepare(USER_ELF, pml4);
    let mut loader = ElfLoader::__create();
    if loader.is_failed() {
        serial::write_str("[elf] load failed: ");
        serial::writeln(&loader.error());
        return;
    }
    let entry = loader.entry();
    let stack_top = loader.user_stack_top();
    serial::write_str("[elf] loaded user program, entry 0x");
    serial::write_hex_u64(entry);
    serial::writeln("");

    // Stand up the process table and admit the user program as a Process.
    unsafe {
        let p = &raw mut PROC_TABLE;
        *p = Some(ProcessTable::__create(MAX_PROCS));
    }
    let pid = proc_table().spawn(); // $Created → $Ready
    unsafe {
        (&raw mut USER_PROC_PID).write(pid);
    }
    serial::write_str("[proc] spawned pid ");
    serial::write_u32_decimal(pid);
    serial::write_str(" (");
    serial::write_str(&proc_table().pid_state(pid));
    serial::writeln(")");

    serial::writeln("[user] entering ring 3...");
    unsafe {
        enter_user(entry, stack_top);
    }

    serial::writeln("[user] back in kernel after user exit");

    // Reap the exited process: $Zombie → $Reaped, freeing its table slot.
    let code = proc_table().reap_pid(pid);
    serial::write_str("[proc] reaped pid ");
    serial::write_u32_decimal(pid);
    serial::write_str("; exit ");
    serial::write_u32_decimal(code as u32);
    serial::write_str("; table count ");
    serial::write_u32_decimal(proc_table().count());
    serial::writeln("");

    // Free the ELF's mapped pages (segments + user stack).
    crate::elf::cleanup();
}

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

use crate::frame_systems::SyscallDispatcher;
use crate::{frames, paging, serial};

// The syscall dispatcher HSM (B3 Step 2). Driven synchronously from the
// syscall entry; single instance, single-core.
static mut DISPATCHER: Option<SyscallDispatcher> = None;

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

// Hand-assembled ring-3 program: write 'A', write 'B', exit(42).
//   mov rax,0; mov rdi,'A'; syscall;  mov rax,0; mov rdi,'B'; syscall;
//   mov rax,1; mov rdi,42; syscall
#[rustfmt::skip]
static USER_BLOB: [u8; 48] = [
    0x48, 0xC7, 0xC0, 0x00, 0x00, 0x00, 0x00, // mov rax, 0 (write_char)
    0x48, 0xC7, 0xC7, 0x41, 0x00, 0x00, 0x00, // mov rdi, 0x41 'A'
    0x0F, 0x05,                               // syscall
    0x48, 0xC7, 0xC0, 0x00, 0x00, 0x00, 0x00, // mov rax, 0
    0x48, 0xC7, 0xC7, 0x42, 0x00, 0x00, 0x00, // mov rdi, 0x42 'B'
    0x0F, 0x05,                               // syscall
    0x48, 0xC7, 0xC0, 0x01, 0x00, 0x00, 0x00, // mov rax, 1 (exit)
    0x48, 0xC7, 0xC7, 0x2A, 0x00, 0x00, 0x00, // mov rdi, 42
    0x0F, 0x05,                               // syscall
];

global_asm!(
    // syscall entry: rcx=user RIP, r11=user RFLAGS, rax=num, rdi/rsi=args.
    // Switch to the kernel syscall stack (statics; single-core), marshal
    // args to the SysV ABI, dispatch, restore, sysretq.
    ".global syscall_entry",
    "syscall_entry:",
    "  mov [rip + USER_RSP_SAVE], rsp",
    "  mov rsp, [rip + KERNEL_SYSCALL_RSP]",
    "  push rcx",     // user RIP (sysret needs it)
    "  push r11",     // user RFLAGS
    "  mov rdx, rsi", // arg1
    "  mov rsi, rdi", // arg0
    "  mov rdi, rax", // syscall number
    "  call syscall_dispatch",
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
            unsafe { resume_kernel_after_user() } // never returns
        }
        _ => u64::MAX, // unreachable: validated by is_known_syscall
    }
}

/// Run the Step 1b demo: set up syscall MSRs, map a user code page (the
/// blob) + a user stack, enter ring 3, and return here when the user exits.
pub fn run() {
    init();

    const CODE_VA: u64 = 0x0000_0000_1000_0000;
    const STACK_VA: u64 = 0x0000_0000_2000_0000;

    let code_frame = frames::alloc_frame().expect("frame alloc");
    let stack_frame = frames::alloc_frame().expect("frame alloc");

    serial::writeln("[user] entering ring 3...");
    unsafe {
        // Copy the blob into the code frame via the HHDM, then map it
        // user-executable (no WRITABLE) and a user-writable stack.
        let dst = frames::phys_to_virt(code_frame);
        core::ptr::copy_nonoverlapping((&raw const USER_BLOB) as *const u8, dst, USER_BLOB.len());
        let pml4 = paging::current_pml4();
        paging::map_in(pml4, CODE_VA, code_frame, paging::USER);
        paging::map_in(pml4, STACK_VA, stack_frame, paging::USER | paging::WRITABLE);

        enter_user(CODE_VA, STACK_VA + 4096 - 16);
    }

    serial::writeln("[user] back in kernel after user exit");
    frames::free_frame(code_frame);
    frames::free_frame(stack_frame);
}

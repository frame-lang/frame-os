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
// Syscall ABI: rax = number, args in rdi/rsi/rdx, return in rax.
//   0 = write_char(rdi = byte)               → serial; returns 1
//   1 = exit(rdi = code)                      → mark the Process $Zombie + yield
//   2 = fork()                                → child pid (parent) / 0 (child)
//   3 = exec(rdi = prog_id)                   → replace image (baked program)
//   4 = wait()                                → reap a child, returns its status
//   5 = open(rdi = path_ptr, rsi = path_len)  → fd, or u64::MAX (B4 Step 4)
//   6 = read(rdi = fd, rsi = buf, rdx = len)  → bytes read, 0 = EOF (B4 Step 4)
//   7 = close(rdi = fd)                       → (B4 Step 4)
//   8 = exec(rdi = path_ptr, rsi = path_len)  → replace image from disk (B4 4)
//   9 = read_line(rdi = buf, rsi = len)       → bytes read; blocks (B8)
//  10 = brk(rdi = new_end)                    → grow/shrink heap; new break (B9)
//  11 = exec_argv(rdi = buf, rsi = len,       → exec w/ argv; argv[0]=path (B9)
//                 rdx = argc)                    buf = argc NUL-terminated args
//   5 = open(rdi = path, rsi = len,           → fd; flags bit0: 0=read 1=write
//            rdx = flags)                         (B9-3 extended #5 with flags)
//  12 = write(rdi = fd, rsi = buf, rdx = len) → bytes written to a file (B9-3)
//  13 = lseek(rdi = fd, rsi = off, rdx = wh)  → new offset; wh 0/1/2 (B9-3)
//  14 = fstat(rdi = fd)                       → file size (B9-3)
//  15 = stat(rdi = path, rsi = len)           → file size, or MAX (B9-3)
//  16 = dup(rdi = fd)                         → new fd (B9-3)
//  17 = unlink(rdi = path, rsi = len)         → 0 ok / MAX; delete a file (B11-3)
//  18 = time()                                → wall-clock Unix epoch seconds,
//                                                read from the CMOS RTC (B11-3)
//  19 = chdir(rdi = path, rsi = len)          → 0 ok / MAX; set the cwd (B11-3)
//  20 = getcwd(rdi = buf, rsi = len)          → path bytes written / MAX (B11-3)
//  21 = readdir(rdi = path, rsi = len,        → bytes of NUL-separated entry
//               rdx = buf, r10 = buflen)         names / MAX if not a dir (S1)

use core::arch::{asm, global_asm};

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::frame_systems::{ElfLoader, ProcessTable, SyscallDispatcher};
use crate::{frames, paging, sched, serial};

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
// The pid `wait` should wait for: a *specific* child (so the shell can't race
// ahead of it), or 0 = "any child" (POSIX `wait`). Set alongside PENDING_WAIT.
static mut PENDING_WAIT_PID: u32 = 0;

// `read_line` (B8) likewise BLOCKS until the user types a newline — which must
// not happen inside the dispatcher handler. The handler records the user buffer
// pointer (0 = none) + length; the blocking line read happens in
// `syscall_dispatch` afterward, writing the byte count into the caller's frame.
static mut PENDING_READLINE_BUF: u64 = 0;
static mut PENDING_READLINE_LEN: u64 = 0;

// A `read` (#6) on a pipe read end (S6) BLOCKS until data arrives or every
// writer closes — and, like `read_line`, must not block inside the dispatcher
// handler (the shared FSM would be parked in `$Executing`). The handler records
// the read end fd (`u64::MAX` = none) + the user buffer + length; the blocking
// pipe read runs in `syscall_dispatch` afterward, yielding to the writer.
static mut PENDING_PIPEREAD_FD: u64 = u64::MAX;
static mut PENDING_PIPEREAD_BUF: u64 = 0;
static mut PENDING_PIPEREAD_LEN: u64 = 0;

// `exec` (3/8/11) reads the program off disk — a *blocking* virtio read (B4)
// that re-enables interrupts and yields. It must NOT run inside the dispatcher
// handler: (1) it would block with the shared `SyscallDispatcher` parked in
// `$Executing`, so a concurrent process's syscall re-enters the non-reentrant
// FSM; (2) `exec_image*` installs the new program's register frame, and reading
// the *global* `CURRENT_TRAP_FRAME` after the blocking read picks up whichever
// process syscalled meanwhile — corrupting the wrong process's saved frame.
// So, like `wait`/`read_line`, the handler only *records* the request; the
// blocking load + frame install happen in `syscall_dispatch` (FSM back in
// `$Validating`), operating on the caller's own `frame` pointer — never the
// global. `-1` = none; otherwise the syscall number (3/8/11) + its args.
static mut PENDING_EXEC: i64 = -1;
static mut PENDING_EXEC_A0: u64 = 0;
static mut PENDING_EXEC_A1: u64 = 0;
static mut PENDING_EXEC_A2: u64 = 0;

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
// `coexec` forks two children that exec *different programs from disk at once* —
// the regression test for the per-exec scratch buffers (the ELF_BUF/ARGV_BUF race).
static USER_COEXEC_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_coexec.elf"));
// `brktest` (B9-1) grows its heap by 1 MiB via `brk` and verifies the new memory.
static USER_BRKTEST_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_brktest.elf"));
// `fwtest` (B9-3) exercises the file write path: write/lseek/fstat/dup/read-back.
static USER_FWTEST_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_fwtest.elf"));
// `fputest` (B11-3a) forks two FPU users that verify xmm0..7 survive preemption.
static USER_FPUTEST_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_fputest.elf"));
// `shell` (B4 Step 4a) cats `/motd` then execs `/bin/hello` from disk by path.
static USER_SHELL_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_shell.elf"));
// `frameshell` (B4 Step 4b) tokenizes command lines with the *same* parser.frs
// the hosted shell uses — the "one source, two targets" demonstration.
static USER_FRAMESHELL_ELF: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/user_frameshell.elf"));
// `ish` (B8) is the interactive shell: a REPL reading live console input
// (read_line) that fork+exec+waits programs — so it survives running them.
#[cfg(feature = "interactive")]
static USER_ISH_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/user_ish.elf"));

// `exec`'s scratch buffers (the ELF image + the packed argv) are now allocated
// **per-exec on the kernel heap**, not in shared statics — see `read_exec_elf`
// and `do_exec_argv`. The old `ELF_BUF`/`ARGV_BUF` statics assumed single-flight
// exec, but `exec` does a *blocking* virtio read that re-enables interrupts and
// yields (the same hazard the B11 trap-frame race exposed): a second process
// exec'ing concurrently would overwrite the shared buffer mid-read, so the first
// process's loader would map the second's program. A distinct heap buffer per
// in-flight exec removes the race the same way the codebase prefers — per-process
// state, not a global serialization lock.

// Caps for `exec_argv` (B9-2): the packed argv copy is bounded to ARGV_BUF_SIZE
// bytes, and at most MAX_ARGS argument pointers are laid onto the new stack.
const ARGV_BUF_SIZE: usize = 1024;
const MAX_ARGS: usize = 32;

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

    // Honor a pending exec AFTER the dispatcher is back in $Validating (B11 fix).
    // The disk read blocks (re-enabling interrupts); doing it inside the handler
    // would re-enter the non-reentrant dispatcher AND let a concurrent syscall
    // clobber the global trap-frame pointer before the new image is installed.
    // Run it on *this* caller's `frame` (captured args first, so a concurrent
    // exec during the read can't disturb us). On success it has replaced the
    // image (`f` now holds the new program's frame); on failure rax = u64::MAX.
    let pending_exec = unsafe { (&raw const PENDING_EXEC).read() };
    if pending_exec >= 0 {
        let (ea0, ea1, ea2) = unsafe {
            (
                (&raw const PENDING_EXEC_A0).read(),
                (&raw const PENDING_EXEC_A1).read(),
                (&raw const PENDING_EXEC_A2).read(),
            )
        };
        unsafe {
            (&raw mut PENDING_EXEC).write(-1);
        }
        f.rax = run_pending_exec(frame, pending_exec, ea0, ea1, ea2);
    }

    // Honor a pending wait AFTER the dispatcher is back in $Validating — the
    // block must not happen inside the handler. do_wait_loop blocks until a
    // child exits, reaps it, and returns the status into the caller's frame.
    if unsafe { (&raw const PENDING_WAIT).read() } {
        let target = unsafe {
            (&raw mut PENDING_WAIT).write(false);
            (&raw const PENDING_WAIT_PID).read()
        };
        f.rax = do_wait_loop(target);
    }

    // Honor a pending read_line AFTER the dispatcher settles (B8): block until a
    // console line is ready, copy it into the caller's buffer, return the length.
    let rl_buf = unsafe { (&raw const PENDING_READLINE_BUF).read() };
    if rl_buf != 0 {
        let rl_len = unsafe { (&raw const PENDING_READLINE_LEN).read() };
        unsafe {
            (&raw mut PENDING_READLINE_BUF).write(0);
        }
        f.rax = do_read_line_loop(rl_buf, rl_len);
    }

    // Honor a pending pipe read AFTER the dispatcher settles (S6): block until
    // the pipe has data or every writer has closed, then return the byte count.
    let pr_fd = unsafe { (&raw const PENDING_PIPEREAD_FD).read() };
    if pr_fd != u64::MAX {
        let pr_buf = unsafe { (&raw const PENDING_PIPEREAD_BUF).read() };
        let pr_len = unsafe { (&raw const PENDING_PIPEREAD_LEN).read() };
        unsafe {
            (&raw mut PENDING_PIPEREAD_FD).write(u64::MAX);
        }
        f.rax = do_pipe_read_loop(pr_fd, pr_buf, pr_len);
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
/// 0=write_char 1=exit 2=fork 3=exec(prog_id) 4=wait 5=open 6=read 7=close
/// 8=exec(path) 9=read_line 10=brk 11=exec_argv 12=write 13=lseek 14=fstat
/// 15=stat 16=dup 17=unlink 18=time 19=chdir 20=getcwd 21=readdir 22=dup2
/// 23=pipe 24=mkdir 25=rmdir 26=rename 27=ps. (B4 Step 4 added the file-I/O +
/// exec-from-disk syscalls; 17–21 are B11-3 follow-ups; 22 is the S5 redirection
/// primitive; 23 is the S6 pipe; 24/25 are the S7 directory ops; 26 is S8 mv;
/// 27 is the S9 process-table snapshot.)
pub fn is_known_syscall(num: u64) -> bool {
    num <= 27
}

/// Block until the console has a complete line, copy it into the user buffer
/// `buf` (up to `len`), and return the byte count (B8). Runs in syscall context
/// after the dispatcher settles. Waits interrupt-enabled so the serial RX IRQ
/// can fill the line (and the timer can preempt); the user buffer is a VA mapped
/// in the current address space (CR3 unchanged during a syscall).
fn do_read_line_loop(buf: u64, len: u64) -> u64 {
    let len = len as usize;
    loop {
        let dst = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len) };
        if let Some(n) = crate::console::take_line(dst) {
            crate::interrupts::disable();
            return n as u64;
        }
        crate::interrupts::wait_for_interrupt_enabled(); // sti + hlt; RX IRQ fills the line
    }
}

/// Record a deferred pipe read (S6): a `read` on a pipe read end that may block.
fn record_pending_pipe_read(fd: u64, buf: u64, len: u64) {
    unsafe {
        (&raw mut PENDING_PIPEREAD_FD).write(fd);
        (&raw mut PENDING_PIPEREAD_BUF).write(buf);
        (&raw mut PENDING_PIPEREAD_LEN).write(len);
    }
}

/// Block until pipe read-end `fd` has bytes (copy + return them) or every writer
/// has closed (return 0 = EOF). Runs after the dispatcher settles. Waits
/// interrupt-enabled between polls so the timer preempts and the *writer* process
/// gets the CPU — the producer side of the pipe runs on the same single core.
fn do_pipe_read_loop(fd: u64, buf: u64, len: u64) -> u64 {
    let fd = fd as usize;
    let len = len as usize;
    loop {
        let dst = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len) };
        let n = crate::vfs::read(fd, dst);
        if n > 0 {
            crate::interrupts::disable();
            return n as u64;
        }
        if !crate::vfs::pipe_writers_open(fd) {
            crate::interrupts::disable();
            return 0; // empty + no writers ⇒ end-of-file
        }
        crate::interrupts::wait_for_interrupt_enabled(); // yield: let the writer run
    }
}

/// The third syscall argument (rdx), read from the current trap frame. The
/// SyscallDispatcher only carries num/a0/a1; 3-arg syscalls (`read`) read the
/// extra arg here, the same way `fork`/`exec` read the frame directly.
fn arg2() -> u64 {
    unsafe { (*(&raw const CURRENT_TRAP_FRAME).read()).rdx }
}

/// The fourth syscall argument, read from the current trap frame. Per the
/// SysV/Linux syscall convention the 4th arg is in r10 (rcx is clobbered by the
/// `syscall` instruction). Used by 4-arg syscalls like `readdir` (#21).
fn arg3() -> u64 {
    unsafe { (*(&raw const CURRENT_TRAP_FRAME).read()).r10 }
}

/// Copy up to `out.len()` bytes from a user pointer into `out`, returning the
/// count. Safe because a syscall runs in the caller's address space (CR3
/// unchanged), so user VAs are mapped.
unsafe fn copy_from_user(ptr: u64, len: usize, out: &mut [u8]) -> usize {
    let n = len.min(out.len());
    core::ptr::copy_nonoverlapping(ptr as *const u8, out.as_mut_ptr(), n);
    n
}

/// Copy a user path (`ptr`/`len`) and resolve it against the caller's current
/// working directory into a canonical absolute path in `out` (B11-3 follow-up).
/// Returns the canonical byte length, or `None` for an empty path or one that
/// doesn't fit. The path syscalls run this so relative paths honor the cwd.
fn resolve_user_path(ptr: u64, len: usize, out: &mut [u8]) -> Option<usize> {
    let mut raw = [0u8; 256];
    let n = unsafe { copy_from_user(ptr, len, &mut raw) };
    if n == 0 {
        return None;
    }
    let mut cwd = [0u8; 256];
    let cl = sched::cwd_current(&mut cwd);
    let cwd: &[u8] = if cl > 0 { &cwd[..cl] } else { b"/" };
    crate::fs::resolve(cwd, &raw[..n], out)
}

/// `brk(new_end)`: grow or shrink the calling process's heap (B9-1). A growable
/// heap is what real toolchains need — the program-image heap is a fixed static.
///   - `new_end == 0` → a *query*: return the current program break unchanged.
///   - `new_end > break` → grow: map fresh, zeroed USER|WRITABLE pages over the
///     gap `[break, new_end)` into the process's address space.
///   - `new_end < break` → shrink: unmap + free the pages over `[new_end, break)`.
/// Returns the new break, or the *unchanged* break on out-of-memory (so the
/// caller's allocator sees the request was refused). `new_end` is rounded up to
/// a page boundary; the heap lives in its own VA region (`sched::USER_HEAP_BASE`)
/// that never overlaps the image or the stack. A syscall runs in the caller's
/// address space (CR3 unchanged), so mapping here targets the right space.
fn do_brk(new_end: u64) -> u64 {
    const PAGE: u64 = 4096;
    let cur = sched::current_heap_brk();
    if new_end == 0 {
        return cur; // query
    }
    // Round the requested break up to a whole page.
    let target = (new_end + PAGE - 1) & !(PAGE - 1);
    let pml4 = paging::current_pml4();
    if target > cur {
        // Grow: map a fresh zeroed frame for each page in [cur, target).
        let mut va = cur;
        while va < target {
            let Some(frame) = frames::alloc_frame() else {
                // Out of memory: roll back the pages we just mapped and refuse.
                let mut undo = cur;
                while undo < va {
                    if let Some(phys) = paging::translate(undo) {
                        unsafe { paging::unmap(undo) };
                        frames::free_frame(phys);
                    }
                    undo += PAGE;
                }
                return cur;
            };
            unsafe {
                core::ptr::write_bytes(frames::phys_to_virt(frame), 0, PAGE as usize);
                paging::map_in(pml4, va, frame, paging::USER | paging::WRITABLE);
            }
            va += PAGE;
        }
    } else if target < cur {
        // Shrink: unmap + free each page in [target, cur).
        let mut va = target;
        while va < cur {
            if let Some(phys) = paging::translate(va) {
                unsafe { paging::unmap(va) };
                frames::free_frame(phys);
            }
            va += PAGE;
        }
    }
    sched::set_current_heap_brk(target);
    target
}

/// Record a deferred `exec` request (syscall `num` = 3/8/11 + its args). The
/// blocking load + frame install happens in `syscall_dispatch` after the
/// dispatcher returns to `$Validating` (see `PENDING_EXEC`).
fn record_pending_exec(num: u64, a0: u64, a1: u64, a2: u64) {
    unsafe {
        (&raw mut PENDING_EXEC_A0).write(a0);
        (&raw mut PENDING_EXEC_A1).write(a1);
        (&raw mut PENDING_EXEC_A2).write(a2);
        (&raw mut PENDING_EXEC).write(num as i64);
    }
}

/// Run a deferred `exec` on the caller's own `frame` (B11 fix). Called from
/// `syscall_dispatch` with the dispatcher already back in `$Validating`, so the
/// blocking disk read can't re-enter the FSM and the frame install targets the
/// caller — not whatever the global `CURRENT_TRAP_FRAME` points at after the read.
/// Returns the exec result (`u64::MAX` on failure; on success it has installed
/// the new program's frame and the value is irrelevant).
fn run_pending_exec(frame: *mut TrapFrame, num: i64, a0: u64, a1: u64, a2: u64) -> u64 {
    match num {
        3 => do_exec(frame, a0),
        8 => do_exec_path(frame, a0, a1),
        11 => do_exec_argv(frame, a0, a1, a2),
        _ => u64::MAX,
    }
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
        3 => {
            // Defer exec (it blocks on disk + installs the new program's frame);
            // syscall_dispatch runs it after the dispatcher is back in $Validating.
            record_pending_exec(3, a0, 0, 0);
            0
        }
        4 => {
            // wait(target_pid=a0): block until child `a0` exits (0 = any child).
            // Recorded here; syscall_dispatch runs the (blocking) reap loop after
            // the dispatcher returns to $Validating. Waiting for a *specific* pid
            // is what lets the shell serialize — `wait`-any would let it reap an
            // older child and race ahead of the one it just forked.
            unsafe {
                (&raw mut PENDING_WAIT).write(true);
                (&raw mut PENDING_WAIT_PID).write(a0 as u32);
            }
            0
        }
        5 => sys_open(a0, _a1),
        6 => {
            // read(fd=a0, buf_ptr=a1, len=rdx) → bytes read. The buffer is a
            // user VA, mapped in the current address space (CR3 unchanged
            // during a syscall), so we write into it directly. A 0-length read
            // is valid POSIX and returns 0 *without* touching the buffer — tcc
            // issues `read(fd, NULL, 0)`, and forming a slice from a null pointer
            // is UB (a debug-build precondition panic), so guard it here.
            let len = arg2() as usize;
            if len == 0 {
                0
            } else if crate::vfs::is_pipe_read(a0 as usize) {
                // Pipe read: may block until data arrives or every writer closes.
                // Defer to the post-dispatch loop (like read_line) so it can yield
                // to the writer process; the result overwrites rax there.
                record_pending_pipe_read(a0, _a1, arg2());
                0
            } else {
                let buf = unsafe { core::slice::from_raw_parts_mut(_a1 as *mut u8, len) };
                crate::vfs::read(a0 as usize, buf) as u64
            }
        }
        7 => {
            crate::vfs::close(a0 as usize);
            0
        }
        8 => {
            record_pending_exec(8, a0, _a1, 0); // exec from disk by path (deferred)
            0
        }
        9 => {
            // read_line(buf_ptr=a0, len=a1) → bytes read (B8). Blocks until a
            // line is typed; like wait, the actual block happens in
            // syscall_dispatch after the dispatcher returns to $Validating.
            unsafe {
                (&raw mut PENDING_READLINE_BUF).write(a0);
                (&raw mut PENDING_READLINE_LEN).write(_a1);
            }
            0
        }
        10 => do_brk(a0), // brk(new_end) → current/new program break (B9-1)
        11 => {
            record_pending_exec(11, a0, _a1, arg2()); // exec with argv (deferred, B9-2)
            0
        }
        12 => {
            // write(fd=a0, buf=a1, len=rdx) → bytes written (B9-3). The buffer is
            // a user VA mapped in the current address space. Like read, a
            // 0-length write returns 0 without forming a (possibly null) slice.
            let len = arg2() as usize;
            if len == 0 {
                0
            } else {
                let buf = unsafe { core::slice::from_raw_parts(_a1 as *const u8, len) };
                // Routed through the per-process fd table (S5): fd 1/2 are
                // console-output descriptors that emit the whole buffer to the
                // serial console in this one syscall (atomic — syscalls run IF=0
                // on a single core, so a process's line is never split mid-way by
                // a concurrent process or a kernel print), while a redirected fd
                // (e.g. after `dup2(file, 1)`) writes to its file instead.
                crate::vfs::write(a0 as usize, buf) as u64
            }
        }
        13 => {
            // lseek(fd=a0, offset=a1, whence=rdx) → new offset, or u64::MAX (B9-3).
            let off = _a1 as i64;
            let whence = arg2() as u32;
            crate::vfs::seek(a0 as usize, off, whence).map_or(u64::MAX, |p| p as u64)
        }
        14 => crate::vfs::fstat_size(a0 as usize).map_or(u64::MAX, |s| s as u64), // fstat(fd) → size
        15 => sys_stat(a0, _a1),
        16 => crate::vfs::dup(a0 as usize).map_or(u64::MAX, |fd| fd as u64), // dup(fd) → newfd
        17 => sys_unlink(a0, _a1),
        18 => crate::rtc::epoch_secs(), // time() → CMOS RTC wall-clock epoch seconds
        19 => sys_chdir(a0, _a1),
        20 => sys_getcwd(a0, _a1),
        21 => sys_readdir(a0, _a1),
        22 => {
            // dup2(oldfd=a0, newfd=a1) → newfd, or u64::MAX. Repoints newfd at
            // oldfd (closing newfd first). The shell uses this to wire
            // redirection in the forked child before exec (S5).
            crate::vfs::dup2(a0 as usize, _a1 as usize).map_or(u64::MAX, |fd| fd as u64)
        }
        23 => {
            // pipe(fds_ptr=a0) → 0, writing [read_fd, write_fd] as two u64 to the
            // user array; u64::MAX if the pipe pool or fd table is exhausted (S6).
            match crate::vfs::make_pipe() {
                Some((r, w)) => {
                    let arr = a0 as *mut u64;
                    unsafe {
                        arr.write(r as u64);
                        arr.add(1).write(w as u64);
                    }
                    0
                }
                None => u64::MAX,
            }
        }
        24 => sys_mkdir(a0, _a1),
        25 => sys_rmdir(a0, _a1),
        26 => sys_rename(a0, _a1),
        27 => sys_ps(a0, _a1),
        _ => u64::MAX, // unreachable: validated by is_known_syscall
    }
}

// --- path-resolving syscall handlers ---------------------------------------
//
// Each of these owns a 512-byte canonical-path buffer (some two). They are
// `#[inline(never)]` *on purpose*: the kernel runs on a single 16 KiB stack
// (gdt::KERNEL_STACK), and in a debug build (opt-level 0) LLVM does no
// stack-slot coloring — every `let buf = [0u8; 512]` across the `perform_syscall`
// match arms would otherwise get its *own* slot in one giant frame, coexisting
// whether or not that arm runs. Pulling each into its own function means a
// buffer only occupies the stack while that one syscall is actually executing.
// (Folding them back inline overflowed the stack and faulted RIP into NX BSS.)

#[inline(never)]
fn sys_open(a0: u64, a1: u64) -> u64 {
    // open(path_ptr=a0, path_len=a1, flags=rdx) → fd, or u64::MAX on failure.
    // flags bit0: 0 = read (default; back-compat), 1 = write (create/truncate).
    // bit1: append — open for writing without truncating, offset at end-of-file
    // (`>>` redirection, S5).
    let mut canon = [0u8; 512];
    let Some(n) = resolve_user_path(a0, a1 as usize, &mut canon) else {
        return u64::MAX;
    };
    let flags = arg2();
    let append = flags & 2 != 0;
    let write = flags & 1 != 0 || append;
    let r = if write {
        crate::vfs::open_write(&canon[..n], append)
    } else {
        crate::vfs::open_read(&canon[..n])
    };
    r.map_or(u64::MAX, |fd| fd as u64)
}

#[inline(never)]
fn sys_stat(a0: u64, a1: u64) -> u64 {
    // stat(path_ptr=a0, path_len=a1) → file size, or u64::MAX (B9-3).
    let mut canon = [0u8; 512];
    let Some(n) = resolve_user_path(a0, a1 as usize, &mut canon) else {
        return u64::MAX;
    };
    match crate::fs::namei(&canon[..n]) {
        Some(ino) if crate::fs::is_file(ino) => crate::fs::size_of(ino) as u64,
        _ => u64::MAX,
    }
}

#[inline(never)]
fn sys_unlink(a0: u64, a1: u64) -> u64 {
    // unlink(path_ptr=a0, path_len=a1) → 0 on success, u64::MAX if the path
    // doesn't resolve to a regular file. Lets a program delete a file (tcc
    // temp/output overwrite, `rm`).
    let mut canon = [0u8; 512];
    let Some(n) = resolve_user_path(a0, a1 as usize, &mut canon) else {
        return u64::MAX;
    };
    if crate::fs::unlink(&canon[..n]) {
        0
    } else {
        u64::MAX
    }
}

#[inline(never)]
fn sys_chdir(a0: u64, a1: u64) -> u64 {
    // chdir(path_ptr=a0, path_len=a1) → 0 on success, u64::MAX if the path
    // doesn't resolve to a directory (or doesn't fit). Resolves relative to the
    // caller's cwd, then stores the canonical absolute result.
    let mut canon = [0u8; 512];
    let Some(n) = resolve_user_path(a0, a1 as usize, &mut canon) else {
        return u64::MAX;
    };
    match crate::fs::namei(&canon[..n]) {
        Some(ino) if crate::fs::is_dir(ino) => {
            if sched::set_cwd_current(&canon[..n]) {
                0
            } else {
                u64::MAX
            }
        }
        _ => u64::MAX,
    }
}

#[inline(never)]
fn sys_getcwd(a0: u64, a1: u64) -> u64 {
    // getcwd(buf_ptr=a0, buf_len=a1) → bytes written (the path, no NUL — libc
    // appends it), or u64::MAX if the buffer is too small. The user buffer is
    // mapped in the current address space (CR3 unchanged).
    let mut cwd = [0u8; 256];
    let cl = sched::cwd_current(&mut cwd);
    let src: &[u8] = if cl > 0 { &cwd[..cl] } else { b"/" };
    let buflen = a1 as usize;
    if buflen < src.len() {
        u64::MAX
    } else {
        unsafe {
            let dst = core::slice::from_raw_parts_mut(a0 as *mut u8, src.len());
            dst.copy_from_slice(src);
        }
        src.len() as u64
    }
}

#[inline(never)]
fn sys_readdir(a0: u64, a1: u64) -> u64 {
    // readdir(path_ptr=a0, path_len=a1, buf_ptr=rdx, buf_len=r10) → bytes of
    // NUL-separated entry names written to buf, or u64::MAX if the path isn't a
    // directory. Path resolves relative to the caller's cwd.
    let mut canon = [0u8; 512];
    let Some(n) = resolve_user_path(a0, a1 as usize, &mut canon) else {
        return u64::MAX;
    };
    let buf_len = arg3() as usize;
    if buf_len == 0 {
        return u64::MAX;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(arg2() as *mut u8, buf_len) };
    crate::fs::list_dir(&canon[..n], buf).map_or(u64::MAX, |w| w as u64)
}

#[inline(never)]
fn sys_mkdir(a0: u64, a1: u64) -> u64 {
    // mkdir(path_ptr=a0, path_len=a1) → 0, or u64::MAX (parent missing / not a
    // dir, name exists, or no space). Path resolves against cwd (S7).
    let mut canon = [0u8; 512];
    let Some(n) = resolve_user_path(a0, a1 as usize, &mut canon) else {
        return u64::MAX;
    };
    if crate::fs::mkdir(&canon[..n]) {
        0
    } else {
        u64::MAX
    }
}

#[inline(never)]
fn sys_rmdir(a0: u64, a1: u64) -> u64 {
    // rmdir(path_ptr=a0, path_len=a1) → 0, or u64::MAX (not an empty dir).
    let mut canon = [0u8; 512];
    let Some(n) = resolve_user_path(a0, a1 as usize, &mut canon) else {
        return u64::MAX;
    };
    if crate::fs::rmdir(&canon[..n]) {
        0
    } else {
        u64::MAX
    }
}

#[inline(never)]
fn sys_ps(a0: u64, a1: u64) -> u64 {
    // ps(buf_ptr=a0, buf_len=a1) → bytes written: a snapshot of the live process
    // table as packed 12-byte records [pid: u32 LE, ppid: u32 LE, state: u32 LE],
    // one per process (state 1=Runnable/R, 2=Blocked/S, 3=Dead/Z). u64::MAX if the
    // buffer is too small. The user buffer is mapped in the current address space.
    let mut recs = [(0u32, 0u32, 0u8); 8]; // MAX_THREADS
    let n = sched::live_procs(&mut recs);
    let need = n * 12;
    if (a1 as usize) < need {
        return u64::MAX;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(a0 as *mut u8, need) };
    for (i, &(pid, ppid, st)) in recs[..n].iter().enumerate() {
        let o = i * 12;
        buf[o..o + 4].copy_from_slice(&pid.to_le_bytes());
        buf[o + 4..o + 8].copy_from_slice(&ppid.to_le_bytes());
        buf[o + 8..o + 12].copy_from_slice(&(st as u32).to_le_bytes());
    }
    need as u64
}

#[inline(never)]
fn sys_rename(a0: u64, a1: u64) -> u64 {
    // rename(src_ptr=a0, src_len=a1, dst_ptr=rdx, dst_len=r10) → 0, or u64::MAX.
    // Both paths resolve against cwd (S8). Two paths ⇒ 4 args.
    let mut csrc = [0u8; 512];
    let mut cdst = [0u8; 512];
    let Some(sn) = resolve_user_path(a0, a1 as usize, &mut csrc) else {
        return u64::MAX;
    };
    let Some(dn) = resolve_user_path(arg2(), arg3() as usize, &mut cdst) else {
        return u64::MAX;
    };
    if crate::fs::rename(&csrc[..sn], &cdst[..dn]) {
        0
    } else {
        u64::MAX
    }
}

/// `wait`: block until a child exits, reap it (collect status, free its
/// `Process` slot + address space), and return its exit code. Returns
/// `u64::MAX` (ECHILD) if the caller has no children. The blocking is the one
/// place a syscall suspends: `sched::block_current_until` yields to the scheduler
/// and returns once a child's exit (SIGCHLD) wakes us. Called from
/// `syscall_dispatch` (not the handler) so the shared dispatcher stays available
/// to the child.
/// Reap one dead child (`child_pid`, `child_pml4`): collect its exit status, free
/// its `Process` slot + address space, and log it. Shared by the wait paths.
fn reap_child(me: u32, child_pid: u32, child_pml4: u64) -> i32 {
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
    status
}

/// `wait(target)`: block until child `target` exits and reap it, returning its
/// exit code. `target == 0` is POSIX `wait` (reap *any* one child). For a
/// specific `target` we drain *every* currently-dead child (so unrelated
/// zombies can't pile up) and return once the target itself is reaped — which is
/// what lets the shell serialize: it blocks here until the exact child it forked
/// is done, so it can never run the next command ahead of it. `u64::MAX`
/// (ECHILD) if the target (or, for 0, any child) doesn't exist.
fn do_wait_loop(target: u32) -> u64 {
    let me = sched::current_pid();
    // Block until a matching child is reapable (exited) or there is no such child.
    // `block_current_until` makes the check-and-block atomic (interrupts off), so a
    // child that exits between "is it dead?" and "go to sleep" can't be missed —
    // the lost-wakeup bug that plain `block_current` has, which intermittently hung
    // the shell in `waitpid` once it always blocked for a *specific* child.
    let exists = |t: u32| {
        if t == 0 {
            sched::has_children(me)
        } else {
            sched::has_child(me, t)
        }
    };
    sched::block_current_until(|| sched::child_reapable(me, target) || !exists(target));
    // A matching child is now dead (or vanished). Reap the dead children; for a
    // specific target, drain the others too so unrelated zombies can't pile up.
    let result = if target == 0 {
        match sched::reap_dead_child(me) {
            Some((child_pid, child_pml4)) => reap_child(me, child_pid, child_pml4) as u64,
            None => u64::MAX, // ECHILD
        }
    } else {
        let mut target_status: Option<i32> = None;
        while let Some((child_pid, child_pml4)) = sched::reap_dead_child(me) {
            let status = reap_child(me, child_pid, child_pml4);
            if child_pid == target {
                target_status = Some(status);
            }
        }
        target_status.map_or(u64::MAX, |s| s as u64)
    };
    result
}

/// Map an `exec` program id to its baked ELF. (No filesystem yet — programs are
/// selected by id; B4 replaces this with loading from disk.)
fn exec_elf(prog_id: u64) -> Option<&'static [u8]> {
    match prog_id {
        0 => Some(USER_ELF), // "hello"
        _ => None,
    }
}

/// `exec(prog_id)`: replace the calling process's image with a baked program
/// selected by id (B3, no filesystem). Returns u64::MAX on an unknown id (the
/// caller keeps running); otherwise never "returns" to the old image — the
/// syscall resumes into the new program. See `exec_image`.
fn do_exec(frame: *mut TrapFrame, prog_id: u64) -> u64 {
    let Some(elf) = exec_elf(prog_id) else {
        return u64::MAX;
    };
    serial::write_str("[exec] pid ");
    serial::write_u32_decimal(sched::current_pid());
    serial::write_str(" exec'd program ");
    serial::write_u32_decimal(prog_id as u32);
    serial::writeln("");
    exec_image(frame, elf)
}

/// Read the regular file `ino` into a freshly heap-allocated buffer, returned as
/// a leaked `'static` slice plus the byte count read; `None` if the heap can't
/// hold the file (a clean failure — `exec` returns `u64::MAX` and the caller
/// keeps running).
///
/// Per-exec (heap) rather than the old shared `ELF_BUF` static: `exec`'s disk
/// read *blocks* (re-enables interrupts and yields), so two processes exec'ing
/// concurrently across that read would clobber a single shared buffer — the
/// first process's loader would then map the second's program. A distinct buffer
/// per in-flight exec removes the race.
///
/// The caller MUST return the buffer to `free_exec_elf` once the (synchronous,
/// non-blocking) ELF load has consumed the bytes. Freeing is safe at that point
/// precisely because the load never blocks: no other exec can be reading into
/// *this* buffer between here and the loader finishing with it. The handle is a
/// raw `*mut [u8]` (a `Copy` value) so the caller can hold a `&'static` view of
/// the bytes for the loader and still free the same allocation afterwards.
fn read_exec_elf(ino: u32) -> Option<(*mut [u8], usize)> {
    let size = crate::fs::size_of(ino);
    // Fallible allocation: a file too big for the heap fails the exec cleanly
    // rather than aborting the kernel via the global alloc-error handler.
    let mut v: Vec<u8> = Vec::new();
    if v.try_reserve_exact(size).is_err() {
        return None;
    }
    v.resize(size, 0);
    let buf: &'static mut [u8] = Box::leak(v.into_boxed_slice());
    let len = crate::fs::read_file(ino, buf);
    Some((buf as *mut [u8], len))
}

/// Free a buffer obtained from `read_exec_elf` (reconstitute the leaked `Box` so
/// it is dropped). Call only after the ELF load has consumed the bytes.
fn free_exec_elf(buf: *mut [u8]) {
    // SAFETY: `buf` is exactly the boxed slice `read_exec_elf` produced via
    // `Box::leak`. Reconstructing the `Box` from it and dropping it frees it,
    // and it is called once per `read_exec_elf` after the load is complete.
    drop(unsafe { Box::from_raw(buf) });
}

/// `exec(path)`: replace the calling process's image with a program loaded from
/// the filesystem by path (B4 Step 4). Reads the ELF into a per-exec heap buffer
/// (see `read_exec_elf`), hands it to the shared `exec_image`, then frees the
/// buffer. Returns u64::MAX if the path doesn't resolve to a regular file or the
/// image doesn't fit the heap (the caller keeps running and sees the failure).
fn do_exec_path(frame: *mut TrapFrame, path_ptr: u64, path_len: u64) -> u64 {
    let mut path = [0u8; 256];
    let n = unsafe { copy_from_user(path_ptr, path_len as usize, &mut path) };
    let Some(ino) = crate::fs::namei(&path[..n]) else {
        return u64::MAX;
    };
    if !crate::fs::is_file(ino) {
        return u64::MAX;
    }
    let Some((buf, len)) = read_exec_elf(ino) else {
        return u64::MAX;
    };
    // SAFETY: `buf` is a live boxed slice from `read_exec_elf`; the bytes stay
    // valid until `free_exec_elf` below, which runs after the synchronous load.
    let full: &'static [u8] = unsafe { &*buf }; // explicit deref+borrow (no autoref)
    let elf: &'static [u8] = &full[..len];

    serial::write_str("[exec] pid ");
    serial::write_u32_decimal(sched::current_pid());
    serial::write_str(" exec'd ");
    for &c in &path[..n] {
        serial::write_byte(c);
    }
    serial::write_str(" from disk (");
    serial::write_u32_decimal(elf.len() as u32);
    serial::writeln(" bytes)");
    let r = exec_image(frame, elf);
    free_exec_elf(buf); // load done (synchronous, non-blocking) — safe to free
    r
}

/// `exec_argv(buf_ptr, buf_len, argc)` — exec a program *with arguments* (B9-2).
/// `buf` is `argc` NUL-terminated strings concatenated; `argv[0]` is the path to
/// load (so `argv[0]` is the program name, the Unix convention). Reads the ELF
/// off disk like `do_exec_path`, then loads it with the argv laid onto the new
/// program's initial stack. Returns `u64::MAX` on a bad path / load failure.
fn do_exec_argv(frame: *mut TrapFrame, buf_ptr: u64, buf_len: u64, argc: u64) -> u64 {
    let n = (buf_len as usize).min(ARGV_BUF_SIZE);
    let argc = (argc as usize).min(MAX_ARGS);
    // Per-exec heap copy of the packed argv (was the shared `ARGV_BUF` static):
    // it must survive the blocking disk read below, during which a concurrent
    // exec could otherwise overwrite a shared copy before we lay it onto the new
    // program's stack. Bounded to ARGV_BUF_SIZE, so this never out-allocates.
    let mut argv_vec: Vec<u8> = Vec::new();
    if argv_vec.try_reserve_exact(n).is_err() {
        return u64::MAX;
    }
    argv_vec.resize(n, 0);
    let copied = unsafe { copy_from_user(buf_ptr, n, &mut argv_vec) };
    let argv: &[u8] = &argv_vec[..copied];
    // argv[0] is the path: the first NUL-terminated string.
    let path_end = argv.iter().position(|&b| b == 0).unwrap_or(argv.len());
    let path = &argv[..path_end];
    let Some(ino) = crate::fs::namei(path) else {
        return u64::MAX;
    };
    if !crate::fs::is_file(ino) {
        return u64::MAX;
    }
    let Some((buf, len)) = read_exec_elf(ino) else {
        return u64::MAX;
    };
    // SAFETY: see `do_exec_path` — valid until `free_exec_elf` after the load.
    let full: &'static [u8] = unsafe { &*buf }; // explicit deref+borrow (no autoref)
    let elf: &'static [u8] = &full[..len];
    let r = exec_image_args(frame, elf, argv, argc);
    free_exec_elf(buf); // load + stack build done (non-blocking) — safe to free
    r
}

/// Build a System V x86-64 initial process stack on the (now-current, post-
/// `exec_into`) user stack and return the new `rsp` (B9-2). Layout, low → high:
/// `argc`, `argv[0..argc]`, NULL, `envp` NULL, `auxv` AT_NULL — with the argv
/// string bytes copied to the top of the page and the pointers aimed at them.
/// `rsp` is left 16-aligned, as the ABI requires at program entry.
///
/// # Safety
/// The current CR3 must be the new program's address space with its one-page
/// user stack mapped writable; `top` is that stack's top (from the loader).
unsafe fn build_initial_stack(top: u64, argv: &[u8], argc: usize) -> u64 {
    // 1. Copy the packed argv strings to the top of the user stack (8-aligned).
    let strings_len = argv.len();
    let strings_base = (top - strings_len as u64) & !0x7;
    core::ptr::copy_nonoverlapping(argv.as_ptr(), strings_base as *mut u8, strings_len);

    // 2. Record each argv string's address within the copied block.
    let mut ptrs = [0u64; MAX_ARGS];
    let mut idx = 0usize;
    let mut off = 0usize;
    while idx < argc && off < strings_len {
        ptrs[idx] = strings_base + off as u64;
        while off < strings_len && *((strings_base + off as u64) as *const u8) != 0 {
            off += 1;
        }
        off += 1; // step past the NUL
        idx += 1;
    }

    // 3. The pointer block sits below the strings: argc, argv[], NULL, envp NULL,
    //    auxv (AT_NULL only). 16-align the final rsp (at argc), per the ABI.
    let n_words = 1 + argc + 1 + 1 + 2; // argc + argv + argvNULL + envpNULL + auxv(2)
    let rsp = (strings_base - (n_words as u64) * 8) & !0xF;
    let w = rsp as *mut u64;
    *w.add(0) = argc as u64;
    let mut i = 0usize;
    while i < argc {
        *w.add(1 + i) = ptrs[i];
        i += 1;
    }
    *w.add(1 + argc) = 0; // argv terminator
    *w.add(2 + argc) = 0; // envp terminator (no env yet)
    *w.add(3 + argc) = 0; // auxv: AT_NULL type
    *w.add(4 + argc) = 0; // auxv: AT_NULL value
    rsp
}

/// Like `exec_image`, but lays `argv` onto the new program's initial stack
/// (B9-2). The process enters the new program with `rsp` pointing at `argc`.
///
/// `frame` is the *caller's own* trap frame (its kernel-stack location), passed
/// down from `syscall_dispatch` rather than read from the global
/// `CURRENT_TRAP_FRAME`: this runs after a blocking disk read, during which a
/// concurrent process's syscall may have overwritten the global, so using it
/// here would install the new image into the wrong process's frame.
fn exec_image_args(frame: *mut TrapFrame, elf: &'static [u8], argv: &[u8], argc: usize) -> u64 {
    let new_pml4 = unsafe { paging::new_address_space() };
    crate::elf::prepare(elf, new_pml4);
    let mut loader = ElfLoader::__create();
    if loader.is_failed() {
        return u64::MAX;
    }
    let entry = loader.entry();
    let top = loader.user_stack_top();
    unsafe {
        sched::exec_into(new_pml4); // CR3 = new space; the user stack is now mapped
        let new_rsp = build_initial_stack(top, argv, argc);
        let f = &mut *frame;
        *f = TrapFrame {
            rip: entry,
            rsp: new_rsp,
            cs: 0x23,      // USER_CODE | 3
            ss: 0x1b,      // USER_DATA | 3
            rflags: 0x202, // IF=1
            ..core::mem::zeroed()
        };
    }
    0
}

/// Replace the current process's image with `elf`: load it into a fresh address
/// space, swap the process onto it, and reset its trap frame to enter the new
/// program (zeroed GPRs, new RIP/RSP). The process keeps its pid + kernel stack.
/// The syscall stub's `iretq` then resumes into the new program; `rax` is set to
/// 0 by `syscall_dispatch`. Returns u64::MAX if the ELF fails to load. `frame`
/// is the caller's own trap frame (see `exec_image_args` for why it's passed in
/// rather than read from the global).
fn exec_image(frame: *mut TrapFrame, elf: &'static [u8]) -> u64 {
    let new_pml4 = unsafe { paging::new_address_space() };
    crate::elf::prepare(elf, new_pml4);
    let mut loader = ElfLoader::__create();
    if loader.is_failed() {
        return u64::MAX;
    }
    let entry = loader.entry();
    let user_rsp = loader.user_stack_top();

    unsafe {
        sched::exec_into(new_pml4);
        let f = &mut *frame;
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

    // B9-1: the growable heap. `brktest` grows its heap by 1 MiB via the `brk`
    // syscall and verifies the new memory — proving the kernel demand-maps real
    // per-process memory far beyond the fixed program-image heap.
    run_one(USER_BRKTEST_ELF, "brktest");

    // B9-3: the file write path. `fwtest` creates /tmp.txt and round-trips
    // write / lseek / fstat / dup / read through the on-disk filesystem.
    run_one(USER_FWTEST_ELF, "fwtest");

    // B11-3a: FPU/SSE state preserved across context switches. `fputest` forks
    // two processes that pin distinct sentinels into xmm0..7 and verify they
    // survive preemptive interleaving — proving the scheduler saves/restores the
    // FPU register file (the foundation for the on-device C toolchain's floats).
    run_one(USER_FPUTEST_ELF, "fputest");

    // B4 Step 4a: a scripted shell that uses the file-I/O syscalls (open/read/
    // close) to `cat /motd`, then `exec`s `/bin/hello` *from disk by path* —
    // the new image replaces the shell and runs to its own exit(42). Requires
    // the FS to be mounted (kmain runs fs::run_demo before usermode::run).
    run_one(USER_SHELL_ELF, "shell");

    // B4 Step 4b: the Frame-driven shell. It tokenizes its command lines with
    // the *same* `parser.frs` the hosted shell compiles — now running in ring 3
    // (no_std + a bump heap). It cats a quoted path (`cat "/motd"`, exercising
    // the Parser's $InQuotedString state in userspace) then execs `/bin/hello`.
    run_one(USER_FRAMESHELL_ELF, "frameshell");

    // Concurrent exec: `coexec` forks two children that `exec_argv` *different*
    // programs from disk at the same time (child A → /bin/hello, child B →
    // /bin/argtest "Z"). Their blocking disk reads interleave, so each must load
    // its own image from a per-exec scratch buffer — the old shared ELF_BUF /
    // ARGV_BUF statics would let one child's read clobber the other's. The
    // parent reaps both and prints "coexec: all done"; the smoke test checks
    // argtest's "argv[1]=Z" survived child A's concurrent exec.
    run_one(USER_COEXEC_ELF, "coexec");
}

/// Launch the interactive shell (B8). Sets up the syscall path + process table,
/// enables the serial console's RX interrupt (IRQ4), then runs `ish` as a
/// process. `ish` loops reading lines (its `read_line` syscall blocks in the
/// kernel until the serial RX IRQ delivers a newline) and fork+exec+waits the
/// programs you type; it returns here only when you type `exit`. Gated behind the
/// `interactive` cargo feature so the default boot (and the smoke suite) is
/// unaffected — see `kmain`.
#[cfg(feature = "interactive")]
pub fn run_interactive_shell() {
    init();
    unsafe {
        let p = &raw mut PROC_TABLE;
        *p = Some(ProcessTable::__create(MAX_PROCS));
    }
    // Turn on console input now that the IDT + PIC are up.
    crate::serial::enable_rx_interrupt();
    crate::pic::unmask_irq(4);
    run_one(USER_ISH_ELF, "ish");
}

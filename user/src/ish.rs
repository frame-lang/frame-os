// Frame OS interactive shell "ish" (B8).
//
// A real REPL, in ring 3: print a prompt, `read_line` from the console (the
// kernel echoes keystrokes + hands back a whole line), tokenize the line with the
// *same* `frame/parser.frs` FSM the hosted shell uses, then dispatch:
//   - `exit`            → leave the shell (the kernel halts)
//   - `help`            → list builtins
//   - `cat <path>...`   → stream files to the console
//   - anything else     → fork + exec the program (`/bin/<cmd>`, or an absolute
//                         path) and wait for it — so the shell *survives* running
//                         a program (exec replaces the *child*, not the shell).
//
// Unlike the scripted `frameshell` (B4), this reads live input (read_line, B8) and
// uses fork/exec/wait so it loops forever instead of being replaced on the first
// exec. The Parser is the same generated FSM; only the I/O around it changed.
//
// Syscall ABI: 0=write_char 1=exit 2=fork 4=wait 5=open 6=read 7=close
//              8=exec_path(path_ptr,len) 9=read_line(buf_ptr,len)

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::panic::PanicInfo;

mod frame_systems;
mod mem;

use frame_systems::Parser;

#[inline(always)]
unsafe fn syscall3(num: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        inlateout("rax") num => ret,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        out("rcx") _,
        out("r11") _,
        options(nostack),
    );
    ret
}

fn write_char(b: u8) {
    unsafe { syscall3(0, b as u64, 0, 0) };
}
fn print(s: &[u8]) {
    for &b in s {
        write_char(b);
    }
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}
fn fork() -> u64 {
    unsafe { syscall3(2, 0, 0, 0) }
}
fn wait() -> u64 {
    unsafe { syscall3(4, 0, 0, 0) }
}
fn open(path: &[u8]) -> u64 {
    unsafe { syscall3(5, path.as_ptr() as u64, path.len() as u64, 0) }
}
fn read(fd: u64, buf: &mut [u8]) -> u64 {
    unsafe { syscall3(6, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}
fn close(fd: u64) {
    unsafe { syscall3(7, fd, 0, 0) };
}
fn exec_path(path: &[u8]) -> u64 {
    unsafe { syscall3(8, path.as_ptr() as u64, path.len() as u64, 0) }
}
fn read_line(buf: &mut [u8]) -> usize {
    unsafe { syscall3(9, buf.as_mut_ptr() as u64, buf.len() as u64, 0) as usize }
}

/// `cat`: stream a file's bytes to the console.
fn cat(path: &str) {
    let fd = open(path.as_bytes());
    if fd == u64::MAX {
        print(b"cat: cannot open ");
        print(path.as_bytes());
        write_char(b'\n');
        return;
    }
    let mut buf = [0u8; 64];
    loop {
        let n = read(fd, &mut buf);
        if n == 0 {
            break;
        }
        print(&buf[..n as usize]);
    }
    close(fd);
}

/// Run an external program: resolve `prog` to a disk path (`/bin/<prog>` unless
/// it's already absolute), then fork — the child execs it from disk, the parent
/// waits. The shell survives because exec replaces the *child's* image.
fn run_external(prog: &str) {
    let mut path = [0u8; 128];
    let resolved: &[u8] = if prog.starts_with('/') {
        prog.as_bytes()
    } else {
        const PREFIX: &[u8] = b"/bin/";
        let pb = prog.as_bytes();
        let n = PREFIX.len() + pb.len();
        if n > path.len() {
            print(b"ish: path too long\n");
            return;
        }
        path[..PREFIX.len()].copy_from_slice(PREFIX);
        path[PREFIX.len()..n].copy_from_slice(pb);
        &path[..n]
    };
    if fork() == 0 {
        // Child: become the program loaded from disk. exec only returns on failure.
        exec_path(resolved);
        print(b"ish: command not found: ");
        print(prog.as_bytes());
        write_char(b'\n');
        exit(127);
    } else {
        // Parent (the shell): reap the child, then loop back to the prompt.
        wait();
    }
}

/// Tokenize one line with the Frame `Parser` FSM and dispatch the first token.
fn run_line(line: &str) {
    let mut p = Parser::__create();
    for c in line.chars() {
        p.consume(c);
    }
    p.finalize();
    let toks: Vec<String> = p.tokens();
    if toks.is_empty() {
        return;
    }
    match toks[0].as_str() {
        "exit" => exit(0),
        "help" => {
            print(b"ish builtins: help, exit, cat <path>...\n");
            print(b"anything else runs /bin/<cmd> from disk (fork+exec+wait)\n");
        }
        "cat" => {
            for path in &toks[1..] {
                cat(path);
            }
        }
        prog => run_external(prog),
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    mem::init();
    print(b"\nFrame OS interactive shell (ish). Type 'help'.\n");
    let mut buf = [0u8; 256];
    loop {
        print(b"frameos$ ");
        let n = read_line(&mut buf);
        if let Ok(line) = core::str::from_utf8(&buf[..n]) {
            run_line(line);
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

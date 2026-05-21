// Frame OS freestanding user program "frameshell" (B4 Step 4b).
//
// The "one source, two targets" demonstration: this ring-3 program tokenizes
// command lines with the *same* `frame/parser.frs` the hosted shell uses — the
// Frame state machine is byte-for-byte the same source, only the surrounding
// environment differs (no_std + a bump heap + raw syscalls here; std + rustyline
// there). The Parser is pure (no native actions), so nothing about it changes;
// it just needed a heap, which `mem.rs` supplies.
//
// It runs a tiny baked script, parsing each line with a fresh `Parser` and
// dispatching on the first token:
//   cat <path>...   → open/read/close each path to the console
//   <path>          → exec the program at <path> from disk (replaces the image)
//
// One line uses a *quoted* path (`cat "/motd"`). The quote handling lives
// entirely in the Parser's `$InQuotedString` state — if it didn't run in ring 3
// the cat would try to open the literal `"/motd"` and fail. A successful cat is
// direct evidence the same parsing states execute here as on the host.
//
// Syscall ABI (B3 + B4): rax = number, args in rdi/rsi/rdx, return in rax.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
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
    unsafe {
        syscall3(0, b as u64, 0, 0);
    }
}

fn open(path: &[u8]) -> u64 {
    unsafe { syscall3(5, path.as_ptr() as u64, path.len() as u64, 0) }
}

fn read(fd: u64, buf: &mut [u8]) -> u64 {
    unsafe { syscall3(6, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

fn close(fd: u64) {
    unsafe {
        syscall3(7, fd, 0, 0);
    }
}

fn exec_path(path: &[u8]) -> u64 {
    unsafe { syscall3(8, path.as_ptr() as u64, path.len() as u64, 0) }
}

fn exit(code: u64) -> ! {
    unsafe {
        syscall3(1, code, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

fn print(s: &[u8]) {
    for &b in s {
        write_char(b);
    }
}

/// `cat`: stream a file's bytes to the console.
fn cat(path: &str) {
    let fd = open(path.as_bytes());
    if fd == u64::MAX {
        print(b"[frameshell] open failed: ");
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

/// Tokenize one line with the Frame `Parser` and dispatch on the first token.
fn run_line(line: &str) {
    print(b"[frameshell] $ ");
    print(line.as_bytes());
    write_char(b'\n');

    // Drive the Parser exactly as the hosted shell does: feed each char, then
    // finalize. The state machine is the same generated code.
    let mut p = Parser::__create();
    for c in line.chars() {
        p.consume(c);
    }
    p.finalize();

    let toks: alloc::vec::Vec<String> = p.tokens();
    if toks.is_empty() {
        return;
    }

    match toks[0].as_str() {
        "cat" => {
            for path in &toks[1..] {
                cat(path);
            }
        }
        // Anything else: treat the first token as a program path and exec it
        // from disk. On success this never returns.
        prog => {
            exec_path(prog.as_bytes());
            print(b"[frameshell] exec failed: ");
            print(prog.as_bytes());
            write_char(b'\n');
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    mem::init();
    print(b"[frameshell] tokenizing with parser.frs (same source as the host shell)\n");

    // A baked script. The first line's path is quoted to exercise the Parser's
    // $InQuotedString state in ring 3; the second execs a program from disk.
    run_line("cat \"/motd\"");
    run_line("/bin/hello"); // never returns on success (hello exit(42)s)

    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

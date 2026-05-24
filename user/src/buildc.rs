// Frame OS "build" — drives the on-device C toolchain through the BuildDriver
// Frame state machine (B11-3e).
//
// This is the Frame *half* of the B11-3 track: B11-3a–d were native mechanics
// (FPU save, ELF loader, frame-libc, the C-shim); the build *lifecycle* —
// compile → link → run, with a failure funnel — is modeled as a state machine
// (`frame/builddriver.frs`, generated to Rust) and merely *driven* here. The
// native mechanism is fork/exec/wait of `/bin/tcc` and the compiled output;
// the FSM owns the phase sequencing and the `$Failed` sink. Same "Frame owns
// lifecycle, native owns mechanism" split as the kernel's ElfLoader.
//
// It builds the staged `/hello.c` into `/out.elf` and runs it:
//   compile:  tcc -c -B/usr/lib/tcc /hello.c -o /hello.o
//   link:     tcc -B/usr/lib/tcc -static /hello.o -o /out.elf
//   run:      /out.elf   (its exit code is captured + reported)
//
// Syscall ABI: 0=write_char 1=exit 2=fork 4=wait 11=exec_argv.

#![no_std]
#![no_main]

extern crate alloc;

use core::arch::asm;
use core::panic::PanicInfo;

mod build_frame;
mod mem;

use build_frame::BuildDriver;

#[inline(always)]
pub(crate) unsafe fn syscall3(num: u64, a0: u64, a1: u64, a2: u64) -> u64 {
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

pub(crate) fn write_char(b: u8) {
    unsafe { syscall3(0, b as u64, 0, 0) };
}
pub(crate) fn print(s: &[u8]) {
    for &b in s {
        write_char(b);
    }
}
/// Print a signed decimal (small helper — no libc here).
pub(crate) fn print_i32(n: i32) {
    if n < 0 {
        write_char(b'-');
    }
    let mut v = (n as i64).unsigned_abs();
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if v == 0 {
        write_char(b'0');
        return;
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    print(&buf[i..]);
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// The native mechanism the BuildDriver FSM calls (`crate::actions::*`): each
/// phase forks a child that execs `/bin/tcc` (or the output) and waits for it,
/// returning the child's exit status. The FSM decides what each result means.
pub mod actions {
    use super::{print, syscall3, write_char};
    use alloc::vec::Vec;

    fn spawn(args: &[&[u8]]) -> u64 {
        // Pack argv: argc NUL-terminated strings, argv[0] = the program path.
        let mut argv: Vec<u8> = Vec::new();
        for a in args {
            argv.extend_from_slice(a);
            argv.push(0);
        }
        let argc = args.len() as u64;
        unsafe {
            if syscall3(2, 0, 0, 0) == 0 {
                // Child: become the program. exec_argv only returns on failure.
                syscall3(11, argv.as_ptr() as u64, argv.len() as u64, argc);
                print(b"[build] exec failed: ");
                print(args[0]);
                write_char(b'\n');
                syscall3(1, 127, 0, 0);
                loop {
                    core::hint::spin_loop();
                }
            } else {
                // Parent: reap the child, return its exit status.
                syscall3(4, 0, 0, 0)
            }
        }
    }

    pub fn compile() -> bool {
        print(b"[build] compile: tcc -c /hello.c -> /hello.o\n");
        spawn(&[b"/bin/tcc", b"-c", b"-B/usr/lib/tcc", b"/hello.c", b"-o", b"/hello.o"]) == 0
    }
    pub fn link() -> bool {
        print(b"[build] link: tcc -static /hello.o -> /out.elf\n");
        spawn(&[b"/bin/tcc", b"-B/usr/lib/tcc", b"-static", b"/hello.o", b"-o", b"/out.elf"]) == 0
    }
    pub fn run() -> i32 {
        print(b"[build] run: /out.elf\n");
        spawn(&[b"/out.elf"]) as i32
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    mem::init();
    print(b"[build] BuildDriver: compile -> link -> run  (/hello.c -> /out.elf)\n");
    let mut d = BuildDriver::__create();
    d.start();
    if d.is_done() {
        print(b"[build] pipeline ok; /out.elf exited with code ");
        print_i32(d.program_exit());
        write_char(b'\n');
        exit(0);
    } else {
        print(b"[build] pipeline FAILED at phase: ");
        print(d.failed_phase().as_bytes());
        write_char(b'\n');
        exit(1);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

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
// Usage: `buildc [SOURCE.c]` — compiles SOURCE (default `/hello.c`) into
// SOURCE-with-.elf and runs it. A general `cc <file>` rather than a fixed path:
//   compile:  tcc -c -B/usr/lib/tcc <src>   -o <src:.c->.o>
//   link:     tcc -B/usr/lib/tcc -static <obj> -o <src:.c->.elf>
//   run:      <out>   (its exit code is captured + reported)
//
// Syscall ABI: 0=write_char 1=exit 2=fork 4=wait 11=exec_argv.

#![no_std]
#![no_main]

extern crate alloc;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

mod build_frame;
mod mem;

use build_frame::BuildDriver;

// Entry shim: at process start `rsp` points at the SysV initial stack (argc,
// argv[], …). Hand that pointer to `build_main` in rdi, 16-aligned, so we can
// read argv[1] (the source path). Same shim as argtest/crt0.
global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp",
    "  and rsp, -16",
    "  call build_main",
    "  ud2",
);

// The build paths, derived from argv once at startup and read by the FSM's
// `actions::*`. buildc is single-threaded, so plain statics (accessed via raw
// pointers — no references, to satisfy the static-mut lints) are safe.
static mut SRC_BUF: [u8; 256] = [0; 256];
static mut OBJ_BUF: [u8; 256] = [0; 256];
static mut OUT_BUF: [u8; 256] = [0; 256];
static mut SRC_LEN: usize = 0;
static mut OBJ_LEN: usize = 0;
static mut OUT_LEN: usize = 0;

/// Copy the concatenation of `parts` into the static byte buffer at `buf`,
/// recording the length at `lenp` (capped at 256).
unsafe fn fill(buf: *mut u8, lenp: *mut usize, parts: &[&[u8]]) {
    let mut n = 0usize;
    for p in parts {
        for &b in *p {
            if n < 256 {
                *buf.add(n) = b;
            }
            n += 1;
        }
    }
    *lenp = n.min(256);
}

/// Set the source path and derive the object (.o) and output (.elf) paths by
/// replacing a trailing ".c" (or appending if the source has no ".c").
fn set_paths(src: &[u8]) {
    let stem: &[u8] = if src.ends_with(b".c") {
        &src[..src.len() - 2]
    } else {
        src
    };
    unsafe {
        fill((&raw mut SRC_BUF) as *mut u8, &raw mut SRC_LEN, &[src]);
        fill(
            (&raw mut OBJ_BUF) as *mut u8,
            &raw mut OBJ_LEN,
            &[stem, b".o"],
        );
        fill(
            (&raw mut OUT_BUF) as *mut u8,
            &raw mut OUT_LEN,
            &[stem, b".elf"],
        );
    }
}

fn src_path() -> &'static [u8] {
    unsafe {
        core::slice::from_raw_parts(
            (&raw const SRC_BUF) as *const u8,
            (&raw const SRC_LEN).read(),
        )
    }
}
fn obj_path() -> &'static [u8] {
    unsafe {
        core::slice::from_raw_parts(
            (&raw const OBJ_BUF) as *const u8,
            (&raw const OBJ_LEN).read(),
        )
    }
}
fn out_path() -> &'static [u8] {
    unsafe {
        core::slice::from_raw_parts(
            (&raw const OUT_BUF) as *const u8,
            (&raw const OUT_LEN).read(),
        )
    }
}

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
        let (src, obj) = (crate::src_path(), crate::obj_path());
        print(b"[build] compile: tcc -c ");
        print(src);
        print(b" -> ");
        print(obj);
        write_char(b'\n');
        spawn(&[b"/bin/tcc", b"-c", b"-B/usr/lib/tcc", src, b"-o", obj]) == 0
    }
    pub fn link() -> bool {
        let (obj, out) = (crate::obj_path(), crate::out_path());
        print(b"[build] link: tcc -static ");
        print(obj);
        print(b" -> ");
        print(out);
        write_char(b'\n');
        spawn(&[b"/bin/tcc", b"-B/usr/lib/tcc", b"-static", obj, b"-o", out]) == 0
    }
    pub fn run() -> i32 {
        let out = crate::out_path();
        print(b"[build] run: ");
        print(out);
        write_char(b'\n');
        spawn(&[out]) as i32
    }
}

/// Length of the NUL-terminated C string at `p` (an argv entry).
fn cstr_len(p: *const u8) -> usize {
    let mut n = 0;
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
    }
    n
}

#[no_mangle]
extern "C" fn build_main(sp: *const u64) -> ! {
    mem::init();
    // argv[1] (at sp[2]) is the source path; default to /hello.c when omitted.
    let argc = unsafe { *sp };
    if argc >= 2 {
        let p = unsafe { *sp.add(2) } as *const u8;
        let n = cstr_len(p);
        let src = unsafe { core::slice::from_raw_parts(p, n) };
        set_paths(src);
    } else {
        set_paths(b"/hello.c");
    }

    print(b"[build] BuildDriver: compile -> link -> run  (");
    print(src_path());
    print(b" -> ");
    print(out_path());
    print(b")\n");
    let mut d = BuildDriver::__create();
    d.start();
    if d.is_done() {
        print(b"[build] pipeline ok; ");
        print(out_path());
        print(b" exited with code ");
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

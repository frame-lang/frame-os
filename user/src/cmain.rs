// Frame OS user program "cmain" (B10-1).
//
// A C-style program: it has NO `_start` of its own — frame-os-libc's crt0
// provides it, parses the kernel's System V initial stack, and calls `main`.
// This is exactly the entry path a tcc-compiled C program (B11) will take:
// crt0 -> main -> libc calls -> syscalls. `main` here just echoes its argc/argv
// through the libc's `write`, proving the linkage and the argv hand-off end to
// end. Disk-only: the shell runs it as `/bin/cmain`.

#![no_std]
#![no_main]

use core::panic::PanicInfo;
use frame_os_libc::{exit, free, malloc, print_fmt, realloc, strlen, write, Arg};

// `main`, C-style: `int main(int argc, char **argv, char **envp)`. frame-libc's
// crt0 calls this, then `exit`s with the returned code.
#[no_mangle]
extern "C" fn main(argc: i32, argv: *const *const u8, _envp: *const *const u8) -> i32 {
    write(1, b"cmain: hello from frame-libc; argc=");
    write(1, &[b'0' + (argc.clamp(0, 9) as u8)]);
    write(1, b"\n");

    let mut i = 0i32;
    while i < argc {
        let p = unsafe { *argv.add(i as usize) };
        let s = unsafe { core::slice::from_raw_parts(p, strlen(p)) };
        write(1, b"  argv[");
        write(1, &[b'0' + (i.clamp(0, 9) as u8)]);
        write(1, b"]=");
        write(1, s);
        write(1, b"\n");
        i += 1;
    }

    // B10-3a: printf via the format-scanner FSM + native conversions. Covers
    // every supported conversion plus the width/left/zero padding flags.
    print_fmt(
        "cmain: d=%d u=%u x=%x X=%X c=%c s=%s p=%p pad=[%5d][%-5d][%05d] pct=%%\n",
        &[
            Arg::Int(-42),
            Arg::UInt(42),
            Arg::UInt(255),
            Arg::UInt(255),
            Arg::Char(b'Q'),
            Arg::Str(b"world\0".as_ptr()),
            Arg::Ptr(0xdead),
            Arg::Int(7),
            Arg::Int(7),
            Arg::Int(7),
        ],
    );

    // B10-2: exercise the heap. 200 KiB forces the libc to grow the program
    // break past its initial 64 KiB chunk; realloc then grows the block further,
    // and the original bytes must survive the copy.
    const N: usize = 200_000;
    let p = unsafe { malloc(N) };
    if p.is_null() {
        write(2, b"cmain: malloc FAILED\n");
        return 1;
    }
    for i in 0..N {
        unsafe { *p.add(i) = (i as u8) ^ 0x5A };
    }
    let mut ok = true;
    for i in 0..N {
        if unsafe { *p.add(i) } != (i as u8) ^ 0x5A {
            ok = false;
            break;
        }
    }
    let q = unsafe { realloc(p, N + 100_000) };
    if q.is_null() {
        write(2, b"cmain: realloc FAILED\n");
        return 1;
    }
    for i in 0..N {
        if unsafe { *q.add(i) } != (i as u8) ^ 0x5A {
            ok = false;
            break;
        }
    }
    unsafe { free(q) };
    if ok {
        write(1, b"cmain: malloc/realloc/free ok (200 KiB via brk)\n");
    } else {
        write(1, b"cmain: heap VERIFY MISMATCH\n");
    }
    0
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit(127)
}

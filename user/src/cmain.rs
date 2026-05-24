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

use frame_os_libc::{
    fclose, feof, fgets, fopen, fprintf_args, fputs, fread, free, malloc, print_fmt, realloc,
    stdout, strlen, write, Arg, FILE,
};

// The C-ABI variadic printf/fprintf frame-libc provides (B11-1). *Calling* a
// variadic extern is stable Rust, so this exercises the real C ABI path a
// tcc-compiled program takes — the args go through registers/stack exactly as a
// C compiler would emit, and frame-libc's naked trampoline reads them. (FILE is
// an opaque handle passed only by pointer, so the ctypes lint is a non-issue.)
#[allow(improper_ctypes)]
extern "C" {
    fn printf(fmt: *const u8, ...) -> i32;
    fn fprintf(f: *mut FILE, fmt: *const u8, ...) -> i32;
}

// The C `<time.h>` surface frame-libc provides (B11-3 follow-up). `time` reads
// the kernel's CMOS RTC; `localtime` breaks an epoch into this `struct tm`
// (layout must match libc/src/posix.rs::Tm).
#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
}
extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime(t: *const i64) -> *const Tm;
}

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

    // B11-1: the C-ABI variadic printf/fprintf (real varargs through registers).
    unsafe {
        printf(
            b"cmain: va printf d=%d x=%x s=%s c=%c\n\0".as_ptr(),
            -7i32,
            0xbeefu32,
            b"hi\0".as_ptr(),
            b'Z' as i32,
        );
        fprintf(
            stdout(),
            b"cmain: va fprintf %d+%d=%d\n\0".as_ptr(),
            20i32,
            22i32,
            42i32,
        );
    }

    // B10-3b: buffered FILE* streams. fprintf to the console, then write a file
    // with fprintf + fputs, close it, reopen it, read it back, and confirm feof.
    fprintf_args(
        stdout(),
        "cmain: fprintf to stdout: %d+%d=%d\n",
        &[Arg::Int(2), Arg::Int(3), Arg::Int(5)],
    );
    let f = unsafe { fopen(b"/gen.txt\0".as_ptr(), b"w\0".as_ptr()) };
    if f.is_null() {
        write(2, b"cmain: fopen(w) FAILED\n");
        return 1;
    }
    fprintf_args(f, "result=%d\n", &[Arg::Int(42)]);
    unsafe { fputs(b"second line\n\0".as_ptr(), f) };
    unsafe { fclose(f) };

    let g = unsafe { fopen(b"/gen.txt\0".as_ptr(), b"r\0".as_ptr()) };
    if g.is_null() {
        write(2, b"cmain: fopen(r) FAILED\n");
        return 1;
    }
    let mut rb = [0u8; 64];
    let n = unsafe { fread(rb.as_mut_ptr(), 1, rb.len(), g) };
    let expected = b"result=42\nsecond line\n";
    let content_ok = n == expected.len() && &rb[..n] == expected;
    let mut tail = [0u8; 8];
    let n2 = unsafe { fread(tail.as_mut_ptr(), 1, tail.len(), g) };
    let eof_ok = n2 == 0 && unsafe { feof(g) } != 0;
    unsafe { fclose(g) };
    if content_ok && eof_ok {
        write(1, b"cmain: FILE* write/read/feof ok\n");
    } else {
        write(1, b"cmain: FILE* MISMATCH\n");
    }

    // B10-4: line input. Reopen /gen.txt and read it back a line at a time with
    // fgets (which keeps the trailing newline), then NULL at EOF.
    let h = unsafe { fopen(b"/gen.txt\0".as_ptr(), b"r\0".as_ptr()) };
    if h.is_null() {
        write(2, b"cmain: fopen(r) for fgets FAILED\n");
        return 1;
    }
    let mut line = [0u8; 64];
    let l1 = unsafe { fgets(line.as_mut_ptr(), line.len() as i32, h) };
    let line1_ok = !l1.is_null() && unsafe { core::slice::from_raw_parts(l1, strlen(l1)) } == b"result=42\n";
    let l2 = unsafe { fgets(line.as_mut_ptr(), line.len() as i32, h) };
    let line2_ok =
        !l2.is_null() && unsafe { core::slice::from_raw_parts(l2, strlen(l2)) } == b"second line\n";
    let l3 = unsafe { fgets(line.as_mut_ptr(), line.len() as i32, h) };
    let eof_null = l3.is_null();
    unsafe { fclose(h) };
    if line1_ok && line2_ok && eof_null {
        write(1, b"cmain: fgets line-by-line ok\n");
    } else {
        write(1, b"cmain: fgets MISMATCH\n");
    }

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

    // B11-3 follow-up: the real wall clock. time() reads the CMOS RTC via the
    // kernel; localtime() breaks it into a calendar date. Prints
    // "cmain: clock YYYY-MM-DD HH:MM:SS (epoch=...)" — the same call path tcc
    // takes for __DATE__/__TIME__. (QEMU pins the RTC to a fixed base in tests,
    // so the date is deterministic.)
    let now = unsafe { time(core::ptr::null_mut()) };
    let tm = unsafe { &*localtime(&now) };
    print_fmt(
        "cmain: clock %d-%02d-%02d %02d:%02d:%02d (epoch=%d)\n",
        &[
            Arg::Int((tm.tm_year + 1900) as i64),
            Arg::Int((tm.tm_mon + 1) as i64),
            Arg::Int(tm.tm_mday as i64),
            Arg::Int(tm.tm_hour as i64),
            Arg::Int(tm.tm_min as i64),
            Arg::Int(tm.tm_sec as i64),
            Arg::Int(now),
        ],
    );
    0
}
// No #[panic_handler] here: frame-libc provides it (cmain links the libc).

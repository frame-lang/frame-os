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
use frame_os_libc::{exit, strlen, write};

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
    0
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit(127)
}

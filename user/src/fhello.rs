// Frame OS user program "fhello" (V1.0 capstone, Rust half).
//
// Drives the `Hello` Frame system — generated from frame/hello.frs by
// `framec -l rust` — and prints. The C-ABI entry is frame-libc's crt0 (this is
// a `#![no_main]` program like cmain), so the path is crt0 → main → the Frame
// FSM. The *same* hello.frs is transpiled to C and built by the on-device tcc
// (see /fhello.c + `buildc /fhello.c`): one Frame source, both backends, both
// run from the shell.
//
// A real state transition gates the output: greeted() is false in $Ready and
// true only after greet() moves the FSM to $Greeted.

#![no_std]
#![no_main]

extern crate alloc;

use frame_os_libc::write;

mod hello_frame;
use hello_frame::Hello;

#[no_mangle]
extern "C" fn main(_argc: i32, _argv: *const *const u8, _envp: *const *const u8) -> i32 {
    let mut h = Hello::__create();
    if h.greeted() {
        write(1, b"fhello: FAIL greeted before greet\n");
        return 1;
    }
    h.greet();
    if h.greeted() {
        write(
            1,
            b"fhello: hello from a Frame system, transpiled to Rust!\n",
        );
        0
    } else {
        write(1, b"fhello: FAIL not greeted after greet\n");
        1
    }
}
// frame-libc provides the panic handler + crt0 (_start).

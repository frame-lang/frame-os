// Frame OS user program "waiter" (B3 Step 5d).
//
// Demonstrates wait()/reap: the parent forks a child, then blocks in wait()
// until the child exits, reaping it (collecting its status; the kernel frees
// the child's Process slot + address space). The child does a little work and
// exits(7). Unlike the forker/spawner (whose children linger as zombies), this
// child is fully reaped — proving the wait + teardown path.
//
// Syscall ABI: rax = number (0 write_char, 1 exit, 2 fork, 4 wait), args in rdi.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[inline(always)]
unsafe fn syscall1(num: u64, a0: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        inlateout("rax") num => ret,
        in("rdi") a0,
        out("rcx") _,
        out("r11") _,
        options(nostack),
    );
    ret
}

fn write_char(b: u8) {
    unsafe {
        syscall1(0, b as u64);
    }
}

fn fork() -> u64 {
    unsafe { syscall1(2, 0) }
}

fn wait() -> u64 {
    unsafe { syscall1(4, 0) }
}

fn exit(code: u64) -> ! {
    unsafe {
        syscall1(1, code);
    }
    loop {
        core::hint::spin_loop();
    }
}

fn pace() {
    for _ in 0..40_000u64 {
        core::hint::spin_loop();
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    if fork() == 0 {
        // Child: do a little work, then exit with a distinctive status.
        let mut i = 0;
        while i < 4 {
            write_char(b'c');
            pace();
            i += 1;
        }
        exit(7);
    } else {
        // Parent: block until the child exits, reaping it.
        write_char(b'W');
        let _status = wait();
        write_char(b'D'); // done waiting
        write_char(b'\n');
        exit(0);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

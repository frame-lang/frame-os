// Frame OS freestanding user program "shell" (B4 Step 4).
//
// A scripted ring-3 program that exercises the B4 file-I/O syscalls and
// exec-from-disk: it opens `/motd`, streams its bytes to the console, closes
// it, then `exec`s `/bin/hello` *by path* — replacing its own image with a
// program loaded from the on-disk filesystem. No std, no allocator: fixed
// buffers and raw `syscall`s only.
//
// Syscall ABI (B3 + B4): rax = number, args in rdi/rsi/rdx, return in rax.
//   0 = write_char(rdi = byte)              → serial
//   1 = exit(rdi = code)                    → never returns
//   5 = open(rdi = path_ptr, rsi = path_len)→ fd, or u64::MAX
//   6 = read(rdi = fd, rsi = buf, rdx = len)→ bytes read (0 = EOF)
//   7 = close(rdi = fd)
//   8 = exec_path(rdi = path_ptr, rsi = len)→ only returns on failure

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

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

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // `cat /motd`: open by path, stream every byte to the console, close.
    print(b"[shell] cat /motd:\n");
    let fd = open(b"/motd");
    if fd != u64::MAX {
        let mut buf = [0u8; 64];
        loop {
            let n = read(fd, &mut buf);
            if n == 0 {
                break;
            }
            print(&buf[..n as usize]);
        }
        close(fd);
    } else {
        print(b"[shell] open /motd failed\n");
    }

    // `exec /bin/hello`: replace our image with a program loaded from disk.
    // On success this never returns (hello runs and exit(42)s); a return means
    // the path didn't resolve.
    print(b"[shell] exec /bin/hello:\n");
    exec_path(b"/bin/hello");
    print(b"[shell] exec /bin/hello failed\n");
    exit(7);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

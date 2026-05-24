// Frame OS user program "fwtest" (B9-3).
//
// Exercises the file write path the toolchains need: open-for-write
// (create/truncate), write, random-access write via lseek, fstat (size), read
// back, and dup (a shared-offset descriptor). It creates /tmp.txt on the
// (writable) disk, writes "Hello, Frame OS!", overwrites the middle via a seek,
// then reads it all back and verifies — proving the kernel's new fd write/seek/
// stat/dup syscalls round-trip through the on-disk filesystem. Prints
// "fwtest: all ok" on success (a FAIL line otherwise).
//
// Syscall ABI: 1=exit 5=open(path,len,flags) 6=read 7=close 12=write 13=lseek
//              14=fstat 16=dup 17=unlink, plus 0=write_char for printing.

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
    unsafe { syscall3(0, b as u64, 0, 0) };
}
fn print(s: &[u8]) {
    for &b in s {
        write_char(b);
    }
}
fn print_u64(mut n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if n == 0 {
        write_char(b'0');
        return;
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    print(&buf[i..]);
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

const O_READ: u64 = 0;
const O_WRITE: u64 = 1;
const SEEK_SET: u64 = 0;
const SEEK_END: u64 = 2;

fn open(path: &[u8], flags: u64) -> u64 {
    unsafe { syscall3(5, path.as_ptr() as u64, path.len() as u64, flags) }
}
fn read(fd: u64, buf: &mut [u8]) -> u64 {
    unsafe { syscall3(6, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}
fn close(fd: u64) {
    unsafe { syscall3(7, fd, 0, 0) };
}
fn write(fd: u64, buf: &[u8]) -> u64 {
    unsafe { syscall3(12, fd, buf.as_ptr() as u64, buf.len() as u64) }
}
fn lseek(fd: u64, off: u64, whence: u64) -> u64 {
    unsafe { syscall3(13, fd, off, whence) }
}
fn fstat(fd: u64) -> u64 {
    unsafe { syscall3(14, fd, 0, 0) }
}
fn dup(fd: u64) -> u64 {
    unsafe { syscall3(16, fd, 0, 0) }
}
fn unlink(path: &[u8]) -> u64 {
    unsafe { syscall3(17, path.as_ptr() as u64, path.len() as u64, 0) }
}

fn fail(msg: &[u8]) -> ! {
    print(b"FAIL: ");
    print(msg);
    write_char(b'\n');
    exit(1);
}

const PATH: &[u8] = b"/tmp.txt";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // 1. Create + write "Hello, Frame OS!" (16 bytes).
    let fd = open(PATH, O_WRITE);
    if fd == u64::MAX {
        fail(b"open for write");
    }
    if write(fd, b"Hello, Frame OS!") != 16 {
        fail(b"write 16");
    }

    // 2. Random-access write: seek to offset 7 and overwrite 7 bytes.
    //    "Hello, Frame OS!" -> "Hello, Planet!S!"
    if lseek(fd, 7, SEEK_SET) != 7 {
        fail(b"lseek to 7");
    }
    if write(fd, b"Planet!") != 7 {
        fail(b"overwrite at 7");
    }

    // 3. fstat reports the size (still 16 — the overwrite stayed in bounds).
    let sz = fstat(fd);
    if sz != 16 {
        fail(b"fstat size != 16");
    }
    close(fd);

    // 4. Reopen for reading; read the whole file back and verify.
    let rfd = open(PATH, O_READ);
    if rfd == u64::MAX {
        fail(b"open for read");
    }
    let mut buf = [0u8; 32];
    let n = read(rfd, &mut buf);
    if n != 16 || &buf[..16] != b"Hello, Planet!S!" {
        fail(b"read-back mismatch");
    }

    // 5. Seek to the overwritten region and read just it.
    if lseek(rfd, 7, SEEK_SET) != 7 {
        fail(b"re-seek to 7");
    }
    let mut mid = [0u8; 7];
    if read(rfd, &mut mid) != 7 || &mid != b"Planet!" {
        fail(b"mid read");
    }

    // 6. SEEK_END returns the size; dup shares the offset (now 14) so the dup'd
    //    fd reads the final two bytes.
    if lseek(rfd, 0, SEEK_END) != 16 {
        fail(b"seek end != 16");
    }
    if lseek(rfd, 14, SEEK_SET) != 14 {
        fail(b"seek to 14");
    }
    let dfd = dup(rfd);
    if dfd == u64::MAX {
        fail(b"dup");
    }
    let mut tail = [0u8; 2];
    if read(dfd, &mut tail) != 2 || &tail != b"S!" {
        fail(b"dup read");
    }
    close(rfd);
    close(dfd);

    // 7. unlink the file, then confirm it's gone (B11-3 follow-up, syscall #17):
    //    unlink returns 0, and a subsequent open-for-read must fail.
    if unlink(PATH) != 0 {
        fail(b"unlink");
    }
    if open(PATH, O_READ) != u64::MAX {
        fail(b"file still present after unlink");
    }

    print(b"fwtest: wrote 16 bytes, fstat=");
    print_u64(sz);
    print(b", seek+overwrite+dup read-back ok\n");
    print(b"fwtest: unlink removed /tmp.txt (reopen fails): ok\n");
    print(b"fwtest: all ok\n");
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

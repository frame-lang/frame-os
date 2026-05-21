// Frame OS user program "faulter" (B3 Step 4b).
//
// Deliberately reads a kernel-half address from ring 3. The page is mapped but
// supervisor-only, so the CPU raises #PF with the U/S error-code bit set. The
// kernel's PageFaultHandler classifies that as a user fault, kills this
// process, and keeps running — proving hardware isolation: a misbehaving user
// program cannot take down the kernel.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // The kernel is loaded at the canonical -2GB higher-half base, mapped
    // present + supervisor (no USER flag). A ring-3 read of it faults.
    let kernel_addr = 0xffff_ffff_8000_0000usize as *const u8;
    let _v = unsafe { core::ptr::read_volatile(kernel_addr) };

    // Unreachable: the read above faults and the kernel never returns us here.
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

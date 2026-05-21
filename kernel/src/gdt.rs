// kernel/src/gdt.rs
//
// Our own GDT + TSS (B3 Step 1a). Pure native. Limine left us a working
// GDT, but ring 3 + `syscall`/`sysret` require a *specific* selector layout
// and a TSS (for the kernel stack on a ring-3 → ring-0 interrupt), so we
// install our own.
//
// Selector layout is dictated by `syscall`/`sysret`:
//   syscall: CS = STAR[47:32],      SS = STAR[47:32] + 8
//   sysret:  CS = STAR[63:48] + 16, SS = STAR[63:48] + 8   (both RPL 3)
// With STAR[47:32]=0x08 and STAR[63:48]=0x10 that means:
//   0x00 null
//   0x08 kernel code   (syscall CS)
//   0x10 kernel data   (syscall SS = 0x10)
//   0x18 user data     (sysret SS = 0x10+8 = 0x18 | 3)
//   0x20 user code     (sysret CS = 0x10+16 = 0x20 | 3)
//   0x28 TSS           (16-byte system descriptor → 0x28..0x38)

use core::arch::asm;

pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
/// Ring-3 selectors (RPL 3 OR'd in by sysret / the ring-3 entry at Step 1b).
#[allow(dead_code)]
pub const USER_DATA: u16 = 0x18;
#[allow(dead_code)]
pub const USER_CODE: u16 = 0x20;
pub const TSS_SELECTOR: u16 = 0x28;

const KERNEL_STACK_SIZE: usize = 16 * 1024;
static mut KERNEL_STACK: [u8; KERNEL_STACK_SIZE] = [0; KERNEL_STACK_SIZE];

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Tss {
    reserved0: u32,
    rsp: [u64; 3], // rsp0..rsp2
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap_base: u16,
}

impl Tss {
    const fn new() -> Self {
        Tss {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iomap_base: core::mem::size_of::<Tss>() as u16,
        }
    }
}

static mut TSS: Tss = Tss::new();

// 7 u64 slots: null, kcode, kdata, udata, ucode, then the 16-byte TSS
// descriptor (2 slots).
static mut GDT: [u64; 7] = [0; 7];

#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: u64,
}

/// Build + load the GDT and TSS, reload all segment registers, and `ltr`.
pub fn init() {
    unsafe {
        // RSP0: the kernel stack used when an interrupt enters ring 0 from
        // ring 3.
        let kstack_top = (&raw mut KERNEL_STACK).add(1) as u64 & !0xF;
        (&raw mut TSS).cast::<u8>(); // (keep TSS addressable)
        let tss_ptr = &raw mut TSS;
        (*tss_ptr).rsp[0] = kstack_top;

        let gdt = &raw mut GDT;
        (*gdt)[0] = 0; // null
        (*gdt)[1] = 0x00AF_9A00_0000_FFFF; // kernel code (ring0, 64-bit)
        (*gdt)[2] = 0x00CF_9200_0000_FFFF; // kernel data
        (*gdt)[3] = 0x00CF_F200_0000_FFFF; // user data (ring3)
        (*gdt)[4] = 0x00AF_FA00_0000_FFFF; // user code (ring3, 64-bit)

        // TSS system descriptor (16 bytes across slots 5,6).
        let base = tss_ptr as u64;
        let limit = (core::mem::size_of::<Tss>() - 1) as u64;
        let low = (limit & 0xFFFF)
            | ((base & 0xFF_FFFF) << 16)
            | (0x89u64 << 40) // present, available 64-bit TSS
            | (((limit >> 16) & 0xF) << 48)
            | (((base >> 24) & 0xFF) << 56);
        let high = (base >> 32) & 0xFFFF_FFFF;
        (*gdt)[5] = low;
        (*gdt)[6] = high;

        let gdtr = Gdtr {
            limit: (core::mem::size_of::<[u64; 7]>() - 1) as u16,
            base: gdt as u64,
        };

        // Load GDT, reload CS via a far return, reload data segments, ltr.
        asm!(
            "lgdt [{gdtr}]",
            "push {kcode}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {kdata:x}",
            "mov es, {kdata:x}",
            "mov ss, {kdata:x}",
            "mov fs, {kdata:x}",
            "mov gs, {kdata:x}",
            "ltr {tss:x}",
            gdtr = in(reg) &gdtr,
            kcode = in(reg) KERNEL_CODE as u64,
            kdata = in(reg) KERNEL_DATA as u32,
            tss = in(reg) TSS_SELECTOR as u32,
            tmp = out(reg) _,
            options(preserves_flags),
        );
    }
}

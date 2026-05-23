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

use crate::percpu::MAX_CPUS;
use core::arch::asm;

pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
/// Ring-3 selectors (RPL 3 OR'd in by sysret / the ring-3 entry at Step 1b).
#[allow(dead_code)]
pub const USER_DATA: u16 = 0x18;
#[allow(dead_code)]
pub const USER_CODE: u16 = 0x20;
/// The TSS selector for core `cpu`. Per-CPU TSS descriptors start at slot 5
/// (byte 0x28) and run two GDT slots (0x10) each (R5b). Core 0's selector stays
/// 0x28, preserving the B3 syscall/ring-3 layout.
pub const fn tss_selector(cpu: usize) -> u16 {
    (0x28 + 0x10 * cpu) as u16
}
/// Core 0's TSS selector (the BSP, used by the syscall/ring-3 path).
pub const TSS_SELECTOR: u16 = 0x28;

/// Read the current Task Register selector (`str`) — the TSS this core has
/// loaded. Used to verify per-CPU TSS setup (R5b).
pub fn current_tr() -> u16 {
    let sel: u16;
    unsafe { asm!("str {0:x}", out(reg) sel, options(nomem, nostack, preserves_flags)) };
    sel
}

const KERNEL_STACK_SIZE: usize = 16 * 1024;
static mut KERNEL_STACK: [u8; KERNEL_STACK_SIZE] = [0; KERNEL_STACK_SIZE];

/// Per-CPU double-fault (#DF) IST stack (R5b). When a core takes a #DF, the CPU
/// switches to this known-good stack (via TSS.ist[0] + the IDT gate's IST=1), so a
/// fault that corrupted the current stack still lands somewhere sane instead of
/// triple-faulting. One per core.
const DF_STACK_SIZE: usize = 8 * 1024;
static mut DF_STACKS: [[u8; DF_STACK_SIZE]; MAX_CPUS] = [[0; DF_STACK_SIZE]; MAX_CPUS];

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

// One TSS per core (R5b). Core 0 is the BSP's (its RSP0 carries the ring-3 →
// ring-0 kernel stack for B3 user processes); every core's ist[0] points at its
// own #DF stack.
static mut TSS: [Tss; MAX_CPUS] = [const { Tss::new() }; MAX_CPUS];

fn tss_ptr(cpu: usize) -> *mut Tss {
    let base = &raw mut TSS as *mut Tss;
    unsafe { base.add(cpu) }
}

/// Set the calling core's TSS.RSP0 — the kernel stack the CPU loads when an
/// interrupt or trap enters ring 0 from ring 3. The scheduler calls this on every
/// switch *to* a user process, so each process traps onto its own kernel stack
/// (B3 Step 5a). Per-CPU as of R5b (targets this core's TSS).
pub fn set_rsp0(top: u64) {
    let cpu = crate::percpu::this_cpu_index() as usize;
    unsafe { (*tss_ptr(cpu)).rsp[0] = top };
}

// GDT: 5 fixed slots (null, kcode, kdata, udata, ucode) + a 16-byte (2-slot) TSS
// descriptor per core.
const GDT_LEN: usize = 5 + 2 * MAX_CPUS;
static mut GDT: [u64; GDT_LEN] = [0; GDT_LEN];

/// Write the 16-byte 64-bit-TSS system descriptor for `base` into GDT slots
/// `slot`, `slot+1`.
unsafe fn write_tss_descriptor(gdt: *mut u64, slot: usize, base: u64) {
    let limit = (core::mem::size_of::<Tss>() - 1) as u64;
    let low = (limit & 0xFFFF)
        | ((base & 0xFF_FFFF) << 16)
        | (0x89u64 << 40) // present, available 64-bit TSS
        | (((limit >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    let high = (base >> 32) & 0xFFFF_FFFF;
    unsafe {
        *gdt.add(slot) = low;
        *gdt.add(slot + 1) = high;
    }
}

#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: u64,
}

/// Build + load the GDT and per-CPU TSSes, reload all segment registers, and
/// `ltr` core 0's TSS. Called once by the BSP. Sets up *every* core's TSS (RSP0
/// for core 0, and the #DF IST stack for all cores) so an AP only has to `ltr`.
pub fn init() {
    unsafe {
        let gdt = &raw mut GDT;
        (*gdt)[0] = 0; // null
        (*gdt)[1] = 0x00AF_9A00_0000_FFFF; // kernel code (ring0, 64-bit)
        (*gdt)[2] = 0x00CF_9200_0000_FFFF; // kernel data
        (*gdt)[3] = 0x00CF_F200_0000_FFFF; // user data (ring3)
        (*gdt)[4] = 0x00AF_FA00_0000_FFFF; // user code (ring3, 64-bit)

        // Per-core TSS: ist[0] = that core's #DF stack; descriptor into the GDT.
        let df_base = &raw mut DF_STACKS as *mut [u8; DF_STACK_SIZE];
        for cpu in 0..MAX_CPUS {
            let df_top = (df_base.add(cpu).add(1) as u64) & !0xF; // 16-aligned top
            (*tss_ptr(cpu)).ist[0] = df_top;
            write_tss_descriptor(gdt as *mut u64, 5 + 2 * cpu, tss_ptr(cpu) as u64);
        }
        // Core 0's RSP0: the kernel stack used when an interrupt enters ring 0
        // from ring 3 (the BSP runs the B3 user processes).
        let kstack_top = (&raw mut KERNEL_STACK).add(1) as u64 & !0xF;
        (*tss_ptr(0)).rsp[0] = kstack_top;

        let gdtr = Gdtr {
            limit: (core::mem::size_of::<[u64; GDT_LEN]>() - 1) as u16,
            base: gdt as u64,
        };

        // Load GDT, reload CS via a far return, reload data segments, ltr core 0.
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

/// Load the already-built GDT on application processor `cpu` and reload its
/// segment registers, then `ltr` *this core's* TSS (R5b). The BSP's `init()` built
/// the GDT and every core's TSS (including each core's #DF IST stack); an AP needs
/// `lgdt` + a far-return to reload CS to our kernel code selector (so the IDT
/// gates' `CS = 0x08` is valid), and now also `ltr tss_selector(cpu)` so a #DF on
/// this core lands on *its own* IST stack instead of triple-faulting.
///
/// NOTE: this reloads `gs` (zeroing the GS base), so a caller that uses per-CPU
/// GS state must call `percpu::init_this_cpu` *after* this, not before.
pub fn load_on_ap(cpu: usize) {
    unsafe {
        let gdt = &raw const GDT;
        let gdtr = Gdtr {
            limit: (core::mem::size_of::<[u64; GDT_LEN]>() - 1) as u16,
            base: gdt as u64,
        };
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
            tss = in(reg) tss_selector(cpu) as u32,
            tmp = out(reg) _,
            options(preserves_flags),
        );
    }
}

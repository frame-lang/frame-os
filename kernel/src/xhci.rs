// kernel/src/xhci.rs
//
// xHCI (USB 3) host-controller bring-up — the native foundation for B6 (USB).
// Step 1: discover the controller on PCI, map its MMIO register window, reset
// it, stand up the data structures the spec requires before the controller will
// run (DCBAA, command ring, event ring + ERST), set Run/Stop, and detect a
// device connected on a port. The USB *lifecycle* (port reset, enumeration,
// transfers) is driven by Frame systems in later B6 steps; this module owns the
// register choreography and the DMA ring memory.
//
// Register map (all relative to the MMIO BAR base, except where noted):
//   Capability registers  @ base+0          (read-only; CAPLENGTH gives their size)
//   Operational registers @ base+CAPLENGTH
//   Runtime registers     @ base+RTSOFF      (interrupter sets)
//   Doorbell array        @ base+DBOFF
//   Port registers        @ op+0x400, 0x10 bytes each
//
// MMIO is reached through the HHDM (`frames::phys_to_virt`), which maps the full
// physical address space — so the BAR's physical base is directly addressable.
// All register access is volatile.

use crate::{frames, paging, pci, serial};
use core::ptr::{read_volatile, write_volatile};

/// MMIO mapping flags: writable + cache-disable (PCD, bit 4) + write-through
/// (PWT, bit 3). Device registers must not be cached. (`paging::map` adds
/// PRESENT.)
const MMIO_FLAGS: u64 = paging::WRITABLE | (1 << 4) | (1 << 3);
/// Pages to map for the register window. qemu-xhci's BAR is ~16 KiB; 64 KiB is a
/// safe cover for capability + operational + runtime + doorbell + port regs.
const MMIO_PAGES: u64 = 16;

// --- Capability register offsets (from BAR base) ---------------------------
const CAP_CAPLENGTH: usize = 0x00; // u8
const CAP_HCSPARAMS1: usize = 0x04; // u32: MaxPorts[31:24], MaxIntrs[18:8], MaxSlots[7:0]
const CAP_HCSPARAMS2: usize = 0x08; // u32: scratchpad-buffer counts
const CAP_HCCPARAMS1: usize = 0x10; // u32: CSZ[2] = 64-byte contexts, AC64[0]
const CAP_DBOFF: usize = 0x14; // u32: doorbell array offset (dword-aligned)
const CAP_RTSOFF: usize = 0x18; // u32: runtime register space offset (32-byte aligned)

// --- Operational register offsets (from op base = BAR + CAPLENGTH) ---------
const OP_USBCMD: usize = 0x00;
const OP_USBSTS: usize = 0x04;
const OP_CRCR: usize = 0x18; // u64: command ring control
const OP_DCBAAP: usize = 0x30; // u64: device context base address array pointer
const OP_CONFIG: usize = 0x38; // u32: MaxSlotsEn[7:0]
const OP_PORTS_BASE: usize = 0x400; // PORTSC[0] @ op+0x400; each port set is 0x10 bytes

const USBCMD_RS: u32 = 1 << 0; // Run/Stop
const USBCMD_HCRST: u32 = 1 << 1; // Host Controller Reset
const USBSTS_HCH: u32 = 1 << 0; // HC Halted
const USBSTS_CNR: u32 = 1 << 11; // Controller Not Ready

const PORTSC_CCS: u32 = 1 << 0; // Current Connect Status
const PORTSC_PED: u32 = 1 << 1; // Port Enabled/Disabled
const PORTSC_PR: u32 = 1 << 4; // Port Reset

// --- Runtime / interrupter register offsets --------------------------------
const RT_IR0: usize = 0x20; // interrupter 0 register set
const IR_ERSTSZ: usize = 0x08; // u32: event ring segment table size
const IR_ERSTBA: usize = 0x10; // u64: event ring segment table base address
const IR_ERDP: usize = 0x18; // u64: event ring dequeue pointer

const TRB_SIZE: usize = 16;
const RING_TRBS: usize = 256; // TRBs per ring segment (one page: 256 * 16 = 4096)

/// A located + initialized xHCI controller. Holds the MMIO base pointers and the
/// physical addresses of the DMA structures (rings, DCBAA) the later B6 steps
/// build on.
pub struct Xhci {
    #[allow(dead_code)]
    base: *mut u8, // MMIO BAR base (capability registers)
    op: *mut u8,   // operational registers (base + CAPLENGTH)
    #[allow(dead_code)]
    runtime: *mut u8, // runtime registers (base + RTSOFF); used by later B6 steps
    #[allow(dead_code)]
    doorbells: *mut u32, // doorbell array (base + DBOFF)
    max_ports: u8,
    max_slots: u8,
    #[allow(dead_code)]
    ctx_64: bool, // CSZ: contexts are 64 bytes (else 32)
    #[allow(dead_code)]
    dcbaa_phys: u64,
    #[allow(dead_code)]
    cmd_ring_phys: u64,
    #[allow(dead_code)]
    event_ring_phys: u64,
}

static mut XHCI: Option<Xhci> = None;

/// The initialized controller, if `init()` succeeded.
#[allow(dead_code)]
pub fn controller() -> Option<&'static mut Xhci> {
    let p = &raw mut XHCI;
    unsafe { (*p).as_mut() }
}

// --- volatile MMIO helpers --------------------------------------------------

unsafe fn rd32(p: *mut u8, off: usize) -> u32 {
    read_volatile(p.add(off) as *const u32)
}
unsafe fn wr32(p: *mut u8, off: usize, val: u32) {
    write_volatile(p.add(off) as *mut u32, val);
}
unsafe fn wr64(p: *mut u8, off: usize, val: u64) {
    // Two 32-bit writes (low then high) — portable across MMIO widths.
    write_volatile(p.add(off) as *mut u32, val as u32);
    write_volatile(p.add(off + 4) as *mut u32, (val >> 32) as u32);
}

/// Allocate one zeroed physical page; return its physical address.
fn alloc_zeroed_page() -> Option<u64> {
    let phys = frames::alloc_frame()?;
    let virt = frames::phys_to_virt(phys);
    unsafe { core::ptr::write_bytes(virt, 0, frames::FRAME_SIZE as usize) };
    Some(phys)
}

/// Discover and bring up the xHCI controller. Returns false if no controller is
/// present (e.g. QEMU launched without `-device qemu-xhci`).
pub fn init() -> bool {
    // xHCI = PCI class 0x0C (serial bus), subclass 0x03 (USB), prog-if 0x30.
    let Some(dev) = pci::find_by_class(0x0C, 0x03, 0x30) else {
        serial::writeln("[usb] no xHCI controller found");
        return false;
    };
    dev.enable_mem_and_bus_master();

    let bar_phys = dev.bar_mem(0);
    // The xHCI MMIO BAR lives in QEMU's high PCIe window, which Limine's HHDM
    // does not map — so map the register window explicitly (uncached) before
    // touching it. We map it at its HHDM virtual address so `phys_to_virt`-style
    // addressing stays consistent with the (RAM-backed, already-mapped) rings.
    let base = frames::phys_to_virt(bar_phys);
    for i in 0..MMIO_PAGES {
        let off = i * frames::FRAME_SIZE;
        unsafe { paging::map(base as u64 + off, bar_phys + off, MMIO_FLAGS) };
    }

    let caplength = unsafe { read_volatile(base.add(CAP_CAPLENGTH)) } as usize;
    let hcs1 = unsafe { rd32(base, CAP_HCSPARAMS1) };
    let hcs2 = unsafe { rd32(base, CAP_HCSPARAMS2) };
    let hcc1 = unsafe { rd32(base, CAP_HCCPARAMS1) };
    let dboff = (unsafe { rd32(base, CAP_DBOFF) } & !0x3) as usize;
    let rtsoff = (unsafe { rd32(base, CAP_RTSOFF) } & !0x1F) as usize;

    let max_slots = (hcs1 & 0xFF) as u8;
    let max_ports = ((hcs1 >> 24) & 0xFF) as u8;
    let ctx_64 = (hcc1 >> 2) & 1 != 0;

    let op = unsafe { base.add(caplength) };
    let runtime = unsafe { base.add(rtsoff) };
    let doorbells = unsafe { base.add(dboff) as *mut u32 };

    serial::write_str("[usb] xHCI @ ");
    serial::write_hex_u64(bar_phys);
    serial::write_str(" caplen ");
    serial::write_u32_decimal(caplength as u32);
    serial::write_str(" slots ");
    serial::write_u32_decimal(max_slots as u32);
    serial::write_str(" ports ");
    serial::write_u32_decimal(max_ports as u32);
    serial::writeln("");

    // 1. Wait for Controller-Not-Ready to clear, then halt + reset.
    if !wait_clear(op, OP_USBSTS, USBSTS_CNR) {
        serial::writeln("[usb] controller never became ready (CNR stuck)");
        return false;
    }
    unsafe {
        // Halt: clear Run/Stop, wait for HCHalted.
        let cmd = rd32(op, OP_USBCMD);
        wr32(op, OP_USBCMD, cmd & !USBCMD_RS);
    }
    if !wait_set(op, OP_USBSTS, USBSTS_HCH) {
        serial::writeln("[usb] controller did not halt");
        return false;
    }
    // Reset and wait for HCRST to self-clear + CNR to clear.
    unsafe { wr32(op, OP_USBCMD, USBCMD_HCRST) };
    if !wait_clear(op, OP_USBCMD, USBCMD_HCRST) || !wait_clear(op, OP_USBSTS, USBSTS_CNR) {
        serial::writeln("[usb] controller reset did not complete");
        return false;
    }

    // 2. Program MaxSlotsEn in CONFIG.
    unsafe {
        let cfg = rd32(op, OP_CONFIG) & !0xFF;
        wr32(op, OP_CONFIG, cfg | max_slots as u32);
    }

    // 3. Device Context Base Address Array (one page; entry 0 is the scratchpad
    //    array pointer if the controller requested scratchpad buffers).
    let Some(dcbaa_phys) = alloc_zeroed_page() else {
        serial::writeln("[usb] out of memory (DCBAA)");
        return false;
    };
    let max_scratch = (((hcs2 >> 27) & 0x1F) | (((hcs2 >> 21) & 0x1F) << 5)) as usize;
    if max_scratch > 0 && !setup_scratchpad(dcbaa_phys, max_scratch) {
        serial::writeln("[usb] out of memory (scratchpad)");
        return false;
    }
    unsafe { wr64(op, OP_DCBAAP, dcbaa_phys) };

    // 4. Command ring (one page); CRCR points at it with the Ring Cycle State
    //    set. The final TRB is a Link TRB back to the start (toggle cycle), so
    //    the ring is closed for when Step 3 starts enqueuing commands.
    let Some(cmd_ring_phys) = alloc_zeroed_page() else {
        serial::writeln("[usb] out of memory (command ring)");
        return false;
    };
    write_link_trb(cmd_ring_phys, cmd_ring_phys);
    unsafe { wr64(op, OP_CRCR, cmd_ring_phys | 1) }; // RCS = 1

    // 5. Event ring: one segment (a page) + a one-entry segment table (ERST),
    //    wired into interrupter 0. ERDP starts at the segment base.
    let Some(event_ring_phys) = alloc_zeroed_page() else {
        serial::writeln("[usb] out of memory (event ring)");
        return false;
    };
    let Some(erst_phys) = alloc_zeroed_page() else {
        serial::writeln("[usb] out of memory (ERST)");
        return false;
    };
    unsafe {
        // ERST entry 0: { ring segment base (u64), size in TRBs (u32), rsvd }.
        let erst = frames::phys_to_virt(erst_phys);
        write_volatile(erst as *mut u64, event_ring_phys);
        write_volatile(erst.add(8) as *mut u32, RING_TRBS as u32);
        // Interrupter 0.
        wr32(runtime, RT_IR0 + IR_ERSTSZ, 1);
        wr64(runtime, RT_IR0 + IR_ERDP, event_ring_phys);
        wr64(runtime, RT_IR0 + IR_ERSTBA, erst_phys);
    }

    // 6. Run.
    unsafe {
        let cmd = rd32(op, OP_USBCMD);
        wr32(op, OP_USBCMD, cmd | USBCMD_RS);
    }
    if !wait_clear(op, OP_USBSTS, USBSTS_HCH) {
        serial::writeln("[usb] controller did not start running");
        return false;
    }
    serial::writeln("[usb] xHCI running");

    let xhci = Xhci {
        base,
        op,
        runtime,
        doorbells,
        max_ports,
        max_slots,
        ctx_64,
        dcbaa_phys,
        cmd_ring_phys,
        event_ring_phys,
    };

    // 7. Report connected ports (the usb-kbd attaches to one of them).
    let mut connected = 0u32;
    for port in 1..=max_ports {
        let sc = xhci.portsc(port);
        if sc & PORTSC_CCS != 0 {
            connected += 1;
            serial::write_str("[usb] device connected on port ");
            serial::write_u32_decimal(port as u32);
            serial::write_str(" (PORTSC ");
            serial::write_hex_u64(sc as u64);
            serial::writeln(")");
        }
    }
    if connected == 0 {
        serial::writeln("[usb] no devices connected");
    }

    let p = &raw mut XHCI;
    unsafe { (*p).replace(xhci) };
    true
}

/// Allocate the scratchpad buffer array + buffers and point DCBAA[0] at it.
fn setup_scratchpad(dcbaa_phys: u64, count: usize) -> bool {
    let Some(array_phys) = alloc_zeroed_page() else {
        return false;
    };
    let array = frames::phys_to_virt(array_phys) as *mut u64;
    for i in 0..count {
        let Some(buf) = alloc_zeroed_page() else {
            return false;
        };
        unsafe { write_volatile(array.add(i), buf) };
    }
    // DCBAA[0] = scratchpad buffer array base.
    let dcbaa = frames::phys_to_virt(dcbaa_phys) as *mut u64;
    unsafe { write_volatile(dcbaa, array_phys) };
    true
}

/// Write a Link TRB at the last slot of `ring_phys` pointing back to `target`,
/// with the Toggle Cycle bit set (so the consumer flips its cycle on wrap).
fn write_link_trb(ring_phys: u64, target: u64) {
    let ring = frames::phys_to_virt(ring_phys);
    let last = unsafe { ring.add((RING_TRBS - 1) * TRB_SIZE) };
    unsafe {
        write_volatile(last as *mut u64, target); // ring segment pointer
        write_volatile(last.add(8) as *mut u32, 0); // status
        // TRB type 6 (Link) << 10, Toggle Cycle (bit 1), Cycle (bit 0).
        write_volatile(last.add(12) as *mut u32, (6 << 10) | (1 << 1) | 1);
    }
}

impl Xhci {
    #[allow(dead_code)]
    pub fn max_ports(&self) -> u8 {
        self.max_ports
    }
    #[allow(dead_code)]
    pub fn max_slots(&self) -> u8 {
        self.max_slots
    }

    /// Read PORTSC for 1-based `port`.
    pub fn portsc(&self, port: u8) -> u32 {
        let off = OP_PORTS_BASE + (port as usize - 1) * 0x10;
        unsafe { rd32(self.op, off) }
    }

    /// Whether a device is currently connected on 1-based `port` (PORTSC.CCS).
    pub fn port_connected(&self, port: u8) -> bool {
        self.portsc(port) & PORTSC_CCS != 0
    }

    /// Whether 1-based `port` is enabled (PORTSC.PED) — set by the controller
    /// after a successful USB3 connect, or after a USB2 port reset.
    #[allow(dead_code)]
    pub fn port_enabled(&self, port: u8) -> bool {
        self.portsc(port) & PORTSC_PED != 0
    }

    /// Whether 1-based `port` is currently in reset (PORTSC.PR).
    #[allow(dead_code)]
    pub fn port_resetting(&self, port: u8) -> bool {
        self.portsc(port) & PORTSC_PR != 0
    }

    /// Number of connected ports (for the bring-up smoke oracle).
    #[allow(dead_code)]
    pub fn connected_count(&self) -> u32 {
        (1..=self.max_ports).filter(|&p| self.port_connected(p)).count() as u32
    }
}

// --- bounded register polling ----------------------------------------------
//
// Bounded spins (not timer-based): bring-up runs before the demo arms any
// timers, and these transitions complete in microseconds under QEMU. The bound
// keeps a misbehaving/absent controller from hanging the kernel.

const POLL_LIMIT: u32 = 100_000_000;

fn wait_clear(p: *mut u8, off: usize, mask: u32) -> bool {
    let mut spins = 0u32;
    while unsafe { rd32(p, off) } & mask != 0 {
        spins += 1;
        if spins >= POLL_LIMIT {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

fn wait_set(p: *mut u8, off: usize, mask: u32) -> bool {
    let mut spins = 0u32;
    while unsafe { rd32(p, off) } & mask == 0 {
        spins += 1;
        if spins >= POLL_LIMIT {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

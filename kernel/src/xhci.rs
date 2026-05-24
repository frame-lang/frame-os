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

use crate::frame_systems::{HubPort, UsbEnumeration, UsbMsd, UsbTransfer};
use crate::{frames, interrupts, paging, pci, serial};
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
                               // PORTSC change bits (17–23: CSC, PEC, WRC, OCC, PRC, PLC, CEC) are write-1-to-
                               // clear. They must be written as 0 when we want to *preserve* them, and PED
                               // (also write-1-to-disable) written as 0 so a register write doesn't disable the
                               // port. This mask is the set we steer around / clear explicitly.
const PORTSC_CHANGES: u32 = 0x00FE_0000;

/// Reset-settle cap, in PIT ticks (100 Hz). QEMU completes a port reset in a
/// tick or two; this is a generous bound before `HubPort` gives up (→ timeout).
const RESET_SETTLE_TICKS: u64 = 50;

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
    op: *mut u8, // operational registers (base + CAPLENGTH)
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
    cmd_ring_phys: u64,
    event_ring_phys: u64,
    // Ring cursors (B6 Step 3). The command ring is a producer ring (we enqueue,
    // the controller consumes): `cmd_enqueue` is our next slot, `cmd_pcs` the
    // Producer Cycle State we stamp. The event ring is a consumer ring (the
    // controller produces, we dequeue): `event_dequeue` is our next slot,
    // `event_ccs` the Consumer Cycle State a valid event must match.
    cmd_enqueue: usize,
    cmd_pcs: u32,
    event_dequeue: usize,
    event_ccs: u32,
}

static mut XHCI: Option<Xhci> = None;

// --- per-device table (R3a) -------------------------------------------------
//
// B6 was single-flight: one device, one port, a pile of `static mut` globals.
// R3 runs several devices concurrently, so that per-device state moves into a
// table (`DEVICES`), one slot per attached device. A `CUR_DEV` ambient index —
// the `tcp.rs` connection-table pattern — lets the *unchanged* `HubPort` /
// `UsbEnumeration` / `UsbTransfer` FSM actions operate on "the current device":
// the driver loop points `CUR_DEV` at the device an event belongs to (resolved
// by xHCI slot) for the duration of that dispatch. The FSM instances never know
// the table exists; native owns the demux, Frame owns each lifecycle.

/// Max devices we enumerate concurrently (qemu-xhci exposes 8 USB2 + 8 USB3
/// root-hub ports; a handful of attached devices is plenty for the demo).
const MAX_DEVICES: usize = 4;

/// One attached device's full enumeration + transfer state (was the B6 globals).
#[derive(Clone, Copy)]
struct Device {
    in_use: bool,
    port: u8,            // 1-based root-hub port this device sits on
    slot: u8,            // xHCI slot id (0 until Enable Slot completes)
    reset_deadline: u64, // HubPort reset-settle deadline (PIT ticks)
    configured: bool,    // reached UsbEnumeration.$Configured
    // Class identity, parsed from the configuration descriptor (R3b). The first
    // interface's class/subclass/protocol: HID keyboard = 3/1/1, HID mouse =
    // 3/1/2, Mass Storage (Bulk-Only, SCSI) = 8/6/0x50.
    iface_class: u8,
    iface_protocol: u8,
    // Endpoint addresses parsed from the config descriptor (0 if absent). A USB
    // endpoint address has the IN/OUT direction in bit 7 and the number in 3:0.
    bulk_in_ep: u8,
    bulk_out_ep: u8,
    // Enumeration DMA structures.
    device_ctx_phys: u64,
    input_ctx_phys: u64,
    ep0_ring_phys: u64,
    ep0_enqueue: usize,
    ep0_pcs: u32,
    desc_buf_phys: u64,
    // Interrupt-IN transfer (HID) structures.
    ep1_ring_phys: u64,
    ep1_enqueue: usize,
    ep1_pcs: u32,
    report_buf_phys: u64,
    // Mass-storage Bulk-Only Transport structures (R3b): a transfer ring +
    // cursor per bulk endpoint, and DMA buffers for the CBW, data, and CSW.
    bulk_in_ring_phys: u64,
    bulk_in_enq: usize,
    bulk_in_pcs: u32,
    bulk_out_ring_phys: u64,
    bulk_out_enq: usize,
    bulk_out_pcs: u32,
    cbw_buf_phys: u64,
    data_buf_phys: u64,
    csw_buf_phys: u64,
}

const DEVICE_INIT: Device = Device {
    in_use: false,
    port: 0,
    slot: 0,
    reset_deadline: 0,
    configured: false,
    iface_class: 0,
    iface_protocol: 0,
    bulk_in_ep: 0,
    bulk_out_ep: 0,
    device_ctx_phys: 0,
    input_ctx_phys: 0,
    ep0_ring_phys: 0,
    ep0_enqueue: 0,
    ep0_pcs: 1,
    desc_buf_phys: 0,
    ep1_ring_phys: 0,
    ep1_enqueue: 0,
    ep1_pcs: 1,
    report_buf_phys: 0,
    bulk_in_ring_phys: 0,
    bulk_in_enq: 0,
    bulk_in_pcs: 1,
    bulk_out_ring_phys: 0,
    bulk_out_enq: 0,
    bulk_out_pcs: 1,
    cbw_buf_phys: 0,
    data_buf_phys: 0,
    csw_buf_phys: 0,
};

static mut DEVICES: [Device; MAX_DEVICES] = [DEVICE_INIT; MAX_DEVICES];
/// The device the current FSM dispatch operates on (set by the driver loop).
static mut CUR_DEV: usize = 0;

fn cur_dev() -> usize {
    unsafe { (&raw const CUR_DEV).read() }
}
fn set_cur_dev(i: usize) {
    unsafe { (&raw mut CUR_DEV).write(i) };
}
fn dslot(i: usize) -> *mut Device {
    let base = &raw mut DEVICES as *mut Device;
    unsafe { base.add(i) }
}
/// The current device (the one the executing FSM action belongs to).
fn curdev() -> *mut Device {
    dslot(cur_dev())
}
/// Find the device-table index whose assigned xHCI slot is `slot`, if any.
fn dev_by_slot(slot: u8) -> Option<usize> {
    (0..MAX_DEVICES).find(|&i| {
        let d = dslot(i);
        unsafe { (*d).in_use && (*d).slot == slot }
    })
}
/// Number of attached devices seeded into the table by `init` (contiguous 0..n).
fn device_count() -> usize {
    (0..MAX_DEVICES)
        .filter(|&i| unsafe { (*dslot(i)).in_use })
        .count()
}

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
        // Both rings start at slot 0; producer/consumer cycle states start at 1
        // (matching the RCS we set in CRCR and the controller's initial ERDP).
        cmd_enqueue: 0,
        cmd_pcs: 1,
        event_dequeue: 0,
        event_ccs: 1,
    };

    // 7. Report connected ports and seed the device table — one slot per attached
    // device, up to MAX_DEVICES (R3a). Each becomes an independent enumeration
    // lifecycle. (B6 had a single CONNECTED_PORT; the table generalizes it.)
    let mut connected = 0u32;
    for port in 1..=max_ports {
        let sc = xhci.portsc(port);
        if sc & PORTSC_CCS != 0 {
            if (connected as usize) < MAX_DEVICES {
                let d = dslot(connected as usize);
                unsafe {
                    (*d).in_use = true;
                    (*d).port = port;
                }
            }
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

    fn write_portsc(&self, port: u8, val: u32) {
        let off = OP_PORTS_BASE + (port as usize - 1) * 0x10;
        unsafe { wr32(self.op, off, val) };
    }

    /// Begin a port reset on 1-based `port`: set PORTSC.PR while preserving the
    /// RW bits and writing 0 to PED + the write-1-to-clear change bits (so the
    /// write neither disables the port nor clears a pending change).
    pub fn begin_reset(&self, port: u8) {
        let v = self.portsc(port);
        let v = (v & !(PORTSC_PED | PORTSC_CHANGES)) | PORTSC_PR;
        self.write_portsc(port, v);
    }

    /// Acknowledge (clear) the write-1-to-clear change bits on `port` after a
    /// reset completes — write back the currently-set change bits (clearing
    /// them) with PED written 0 so we don't disable the port.
    pub fn clear_port_changes(&self, port: u8) {
        let v = self.portsc(port);
        let v = (v & !PORTSC_PED) | (v & PORTSC_CHANGES);
        self.write_portsc(port, v);
    }

    /// Enqueue a command TRB on the command ring (the low 3 dwords + a control
    /// dword `ctrl` whose cycle bit we stamp), then return the physical address
    /// of the slot it landed in (for matching its Command Completion Event).
    /// Follows the Link TRB at the end of the ring (toggling our cycle state).
    fn enqueue_cmd(&mut self, d0: u32, d1: u32, d2: u32, ctrl: u32) -> u64 {
        let ring = frames::phys_to_virt(self.cmd_ring_phys);
        // If we're at the Link TRB slot, point it at our current cycle so the
        // controller follows it, then wrap to slot 0 and toggle our cycle.
        if self.cmd_enqueue >= RING_TRBS - 1 {
            let link = unsafe { ring.add((RING_TRBS - 1) * TRB_SIZE) };
            unsafe {
                write_volatile(
                    link.add(12) as *mut u32,
                    (6 << 10) | (1 << 1) | self.cmd_pcs,
                )
            };
            self.cmd_enqueue = 0;
            self.cmd_pcs ^= 1;
        }
        let slot = unsafe { ring.add(self.cmd_enqueue * TRB_SIZE) };
        let trb_phys = self.cmd_ring_phys + (self.cmd_enqueue * TRB_SIZE) as u64;
        unsafe {
            write_volatile(slot as *mut u32, d0);
            write_volatile(slot.add(4) as *mut u32, d1);
            write_volatile(slot.add(8) as *mut u32, d2);
            write_volatile(slot.add(12) as *mut u32, ctrl | self.cmd_pcs);
        }
        self.cmd_enqueue += 1;
        trb_phys
    }

    /// Ring the command doorbell (DB[0] = 0) so the controller processes the
    /// command ring up to our enqueue pointer.
    fn ring_command_doorbell(&self) {
        unsafe { write_volatile(self.doorbells, 0) };
    }

    /// Ring an endpoint doorbell: DB[slot] = endpoint DCI (e.g. 1 for EP0).
    fn ring_doorbell(&self, slot: u8, dci: u32) {
        unsafe { write_volatile(self.doorbells.add(slot as usize), dci) };
    }

    /// Dequeue the next event-ring TRB if one has been produced (its cycle bit
    /// matches our Consumer Cycle State). Advances the dequeue pointer + ERDP.
    fn poll_event(&mut self) -> Option<[u32; 4]> {
        let ring = frames::phys_to_virt(self.event_ring_phys);
        let slot = unsafe { ring.add(self.event_dequeue * TRB_SIZE) };
        let d3 = unsafe { read_volatile(slot.add(12) as *const u32) };
        if (d3 & 1) != self.event_ccs {
            return None; // controller hasn't produced this slot yet
        }
        let d0 = unsafe { read_volatile(slot as *const u32) };
        let d1 = unsafe { read_volatile(slot.add(4) as *const u32) };
        let d2 = unsafe { read_volatile(slot.add(8) as *const u32) };

        self.event_dequeue += 1;
        if self.event_dequeue >= RING_TRBS {
            self.event_dequeue = 0;
            self.event_ccs ^= 1;
        }
        // Update ERDP to the new dequeue position + clear the Event Handler Busy
        // bit (bit 3, write-1-to-clear).
        let erdp = self.event_ring_phys + (self.event_dequeue * TRB_SIZE) as u64;
        unsafe { wr64(self.runtime, RT_IR0 + IR_ERDP, erdp | (1 << 3)) };
        Some([d0, d1, d2, d3])
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
        (1..=self.max_ports)
            .filter(|&p| self.port_connected(p))
            .count() as u32
    }
}

// --- HubPort driver (B6 Step 2) --------------------------------------------
//
// These are the native actions the `HubPort` Frame system calls, plus the loop
// that drives one port from connect → reset → enabled. Frame owns the lifecycle
// (which state, the timed reset transition); this owns the PORTSC pokes + the
// settle deadline.

/// `HubPort.$Resetting.$>`: assert Port Reset on `port` and arm the settle
/// deadline (fired as `timeout()` by the driver loop if the controller doesn't
/// report the port enabled in time).
pub fn begin_port_reset(port: u8) {
    if let Some(x) = controller() {
        x.begin_reset(port);
    }
    unsafe { (*curdev()).reset_deadline = interrupts::ticks() + RESET_SETTLE_TICKS };
    serial::write_str("[usb] resetting port ");
    serial::write_u32_decimal(port as u32);
    serial::writeln("");
}

/// `HubPort.$Enabled.$>`: the port reset completed and the controller enabled
/// the port — the device is now in its Default state, ready for enumeration.
pub fn on_port_enabled(port: u8) {
    serial::write_str("[usb] port ");
    serial::write_u32_decimal(port as u32);
    serial::writeln(" enabled");
}

/// Whether the controller reports `port` enabled (reset completed).
fn port_reset_done(port: u8) -> bool {
    controller().map(|x| x.port_enabled(port)).unwrap_or(false)
}

/// Drive the connected port through its `HubPort` lifecycle: connect → reset →
/// (enabled | timeout). Called from kmain after `init()`. Single-flight (the one
/// device detected at bring-up).
pub fn run_port_lifecycle() {
    let n = device_count();
    if n == 0 {
        return; // no devices connected
    }

    interrupts::enable();

    // Bring every attached port up *concurrently*: N HubPort instances coexist,
    // each pinned to its device via CUR_DEV. We assert reset on all ports first
    // (each $Resetting.$> arms that device's own settle deadline), then poll the
    // controller until every port is enabled or its deadline passes. (R3a — the
    // orthogonal-regions question: many concurrent lifecycle FSMs of one type.)
    let mut hp: [Option<HubPort>; MAX_DEVICES] = [None, None, None, None];
    let mut done = [false; MAX_DEVICES];
    for (d, cell) in hp.iter_mut().enumerate().take(n) {
        set_cur_dev(d);
        let port = unsafe { (*dslot(d)).port };
        let mut h = HubPort::__create();
        h.connect(port); // -> $Connected
        h.reset(); // -> $Resetting ($> asserts PR + arms this device's deadline)
        *cell = Some(h);
    }

    loop {
        let mut all_done = true;
        for d in 0..n {
            if done[d] {
                continue;
            }
            set_cur_dev(d);
            let port = unsafe { (*dslot(d)).port };
            let h = hp[d].as_mut().unwrap();
            if port_reset_done(port) {
                h.reset_complete(); // -> $Enabled ($> logs)
                if let Some(x) = controller() {
                    x.clear_port_changes(port); // ack the reset-change bits
                }
                done[d] = true;
            } else if interrupts::ticks() >= unsafe { (*dslot(d)).reset_deadline } {
                h.timeout(); // -> $Connected
                serial::write_str("[usb] port ");
                serial::write_u32_decimal(port as u32);
                serial::writeln(" reset timed out");
                done[d] = true;
            } else {
                all_done = false;
            }
        }
        if all_done {
            break;
        }
        interrupts::wait_for_interrupt();
    }
    interrupts::disable();
}

// --- USB enumeration (B6 Step 3) -------------------------------------------
//
// The native actions the `UsbEnumeration` Frame system calls (each issues one
// xHCI command, non-blocking — never waits inside a Frame handler), plus the
// driver loop that dequeues completion events and dispatches the matching FSM
// event. Frame owns the enumeration *stage*; this owns the TRBs + contexts.

const TRB_NORMAL: u32 = 1;
const TRB_SETUP_STAGE: u32 = 2;
const TRB_DATA_STAGE: u32 = 3;
const TRB_STATUS_STAGE: u32 = 4;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_CMD_COMPLETION: u32 = 33;
const COMPLETION_SUCCESS: u32 = 1;
const COMPLETION_SHORT_PACKET: u32 = 13; // an IN transfer returning < requested — fine

// The HID boot keyboard's interrupt-IN endpoint: USB endpoint 1 IN → xHCI
// Device Context Index 3 (DCI = endpoint*2 + direction; EP1 IN = 1*2+1).
const EP1_IN_DCI: u32 = 3;

// TRB control-dword flags.
const TRB_IDT: u32 = 1 << 6; // Immediate Data (Setup packet rides in the TRB)
const TRB_IOC: u32 = 1 << 5; // Interrupt On Completion
const TRB_DIR_IN: u32 = 1 << 16; // Data/Status Stage direction = device-to-host
const TRT_IN: u32 = 3 << 16; // Setup Stage transfer type = IN data stage

const EP0_DCI: u32 = 1; // Endpoint 0 Device Context Index (the control endpoint)

// Enumeration + transfer state is now per-device in the `DEVICES` table (R3a);
// each action reads/writes `(*curdev())`, the device the current dispatch is for.

fn trb_type(d3: u32) -> u32 {
    (d3 >> 10) & 0x3F
}
fn completion_code(d2: u32) -> u32 {
    (d2 >> 24) & 0xFF
}
fn event_slot(d3: u32) -> u8 {
    ((d3 >> 24) & 0xFF) as u8
}

/// EP0 max packet size by USB speed (PORTSC speed field): LS/FS=8, HS=64, SS=512.
fn ep0_mps(speed: u32) -> u32 {
    match speed {
        3 => 64,  // High-Speed
        4 => 512, // SuperSpeed
        _ => 8,   // Full/Low-Speed (and unknown)
    }
}

/// `UsbEnumeration.$Powered.$>`: issue an Enable Slot command (non-blocking).
pub fn cmd_enable_slot() {
    if let Some(x) = controller() {
        x.enqueue_cmd(0, 0, 0, TRB_ENABLE_SLOT << 10);
        x.ring_command_doorbell();
    }
    serial::writeln("[usb] enable slot issued");
}

/// `UsbEnumeration.$SlotEnabled.$>`: build the input context (slot + EP0) for
/// `slot`, register the output device context in the DCBAA, allocate the EP0
/// transfer ring, and issue an Address Device command (non-blocking).
pub fn address_device(slot: u8) {
    serial::write_str("[usb] slot ");
    serial::write_u32_decimal(slot as u32);
    serial::writeln(" enabled");

    let port = unsafe { (*curdev()).port };
    let Some(x) = controller() else { return };
    let speed = (x.portsc(port) >> 10) & 0xF;
    let mps = ep0_mps(speed);
    let cs = if x.ctx_64 { 64usize } else { 32 };

    // Output device context → DCBAA[slot].
    let Some(devctx) = alloc_zeroed_page() else {
        return;
    };
    let dcbaa = frames::phys_to_virt(x.dcbaa_phys) as *mut u64;
    unsafe { write_volatile(dcbaa.add(slot as usize), devctx) };

    // EP0 transfer ring (with a Link TRB back to its start).
    let Some(ep0) = alloc_zeroed_page() else {
        return;
    };
    write_link_trb(ep0, ep0);

    // Input context: Input Control Context + Slot Context + EP0 Context.
    let Some(ictx) = alloc_zeroed_page() else {
        return;
    };
    let v = frames::phys_to_virt(ictx);
    unsafe {
        // Input Control Context: Add Context flags A0 (slot) | A1 (EP0).
        write_volatile(v.add(4) as *mut u32, 0b11);
        // Slot Context: Context Entries = 1 (bits 31:27), Speed (bits 23:20).
        write_volatile(v.add(cs) as *mut u32, (1 << 27) | (speed << 20));
        // Root Hub Port Number (bits 23:16).
        write_volatile(v.add(cs + 4) as *mut u32, (port as u32) << 16);
        // EP0 Context: EP Type = 4 (Control, bits 5:3), CErr = 3 (bits 2:1),
        // Max Packet Size (bits 31:16).
        let ep = cs * 2;
        write_volatile(v.add(ep + 4) as *mut u32, (4 << 3) | (3 << 1) | (mps << 16));
        // TR Dequeue Pointer = EP0 ring | DCS(1).
        write_volatile(v.add(ep + 8) as *mut u32, (ep0 as u32) | 1);
        write_volatile(v.add(ep + 12) as *mut u32, (ep0 >> 32) as u32);
        // Average TRB Length (control = 8).
        write_volatile(v.add(ep + 16) as *mut u32, 8);
    }

    unsafe {
        let d = curdev();
        (*d).slot = slot;
        (*d).device_ctx_phys = devctx;
        (*d).input_ctx_phys = ictx;
        (*d).ep0_ring_phys = ep0;
        (*d).ep0_enqueue = 0;
        (*d).ep0_pcs = 1;
    }

    // Address Device command: input context pointer + slot id.
    x.enqueue_cmd(
        ictx as u32,
        (ictx >> 32) as u32,
        0,
        (TRB_ADDRESS_DEVICE << 10) | ((slot as u32) << 24),
    );
    x.ring_command_doorbell();
}

/// Enqueue a TRB on the EP0 control transfer ring (cycle bit stamped from our
/// EP0 producer cycle state), following the Link TRB at the end of the ring.
fn enqueue_ep0(d0: u32, d1: u32, d2: u32, ctrl: u32) {
    let d = curdev();
    let ring_phys = unsafe { (*d).ep0_ring_phys };
    let mut enq = unsafe { (*d).ep0_enqueue };
    let mut pcs = unsafe { (*d).ep0_pcs };
    let ring = frames::phys_to_virt(ring_phys);
    if enq >= RING_TRBS - 1 {
        let link = unsafe { ring.add((RING_TRBS - 1) * TRB_SIZE) };
        unsafe { write_volatile(link.add(12) as *mut u32, (6 << 10) | (1 << 1) | pcs) };
        enq = 0;
        pcs ^= 1;
    }
    let slot = unsafe { ring.add(enq * TRB_SIZE) };
    unsafe {
        write_volatile(slot as *mut u32, d0);
        write_volatile(slot.add(4) as *mut u32, d1);
        write_volatile(slot.add(8) as *mut u32, d2);
        write_volatile(slot.add(12) as *mut u32, ctrl | pcs);
    }
    unsafe {
        (*d).ep0_enqueue = enq + 1;
        (*d).ep0_pcs = pcs;
    }
}

/// `UsbEnumeration.$AddressAssigned.$>`: the device has a USB address — issue a
/// GET_DESCRIPTOR (device) control transfer on EP0 (Setup → Data IN → Status),
/// reading the 18-byte device descriptor into the DMA buffer.
pub fn get_device_descriptor(slot: u8) {
    serial::write_str("[usb] device addressed (slot ");
    serial::write_u32_decimal(slot as u32);
    serial::writeln(")");

    // Allocate the descriptor DMA buffer once (per device).
    if unsafe { (*curdev()).desc_buf_phys } == 0 {
        if let Some(b) = alloc_zeroed_page() {
            unsafe { (*curdev()).desc_buf_phys = b };
        } else {
            return;
        }
    }
    let buf = unsafe { (*curdev()).desc_buf_phys };

    // Setup packet: bmRequestType=0x80 (IN, standard, device), bRequest=6
    // (GET_DESCRIPTOR), wValue=0x0100 (Device descriptor, index 0), wLength=18.
    let d0 = 0x80 | (6 << 8) | (0x0100 << 16);
    let d1 = 18 << 16; // wIndex=0, wLength=18
    enqueue_ep0(d0, d1, 8, (TRB_SETUP_STAGE << 10) | TRB_IDT | TRT_IN);
    // Data Stage (IN): the 18-byte descriptor into `buf`.
    enqueue_ep0(
        buf as u32,
        (buf >> 32) as u32,
        18,
        (TRB_DATA_STAGE << 10) | TRB_DIR_IN,
    );
    // Status Stage (OUT for an IN data stage), interrupt on completion.
    enqueue_ep0(0, 0, 0, (TRB_STATUS_STAGE << 10) | TRB_IOC);

    if let Some(x) = controller() {
        x.ring_doorbell(slot, EP0_DCI);
    }
}

/// Parse + log the device descriptor read by the GET_DESCRIPTOR transfer
/// (idVendor @ offset 8, idProduct @ 10 — little-endian).
pub fn read_device_descriptor() {
    let buf = unsafe { (*curdev()).desc_buf_phys };
    if buf == 0 {
        return;
    }
    let v = frames::phys_to_virt(buf);
    let id_vendor = unsafe { read_volatile(v.add(8) as *const u16) };
    let id_product = unsafe { read_volatile(v.add(10) as *const u16) };
    serial::write_str("[usb] device descriptor: idVendor ");
    serial::write_hex_u64(id_vendor as u64);
    serial::write_str(" idProduct ");
    serial::write_hex_u64(id_product as u64);
    serial::writeln("");
}

/// `UsbEnumeration.$DeviceDescribed.$>`: issue a SET_CONFIGURATION(1) control
/// transfer on EP0 (Setup → Status, no data stage).
pub fn set_configuration(slot: u8) {
    // Setup packet: bmRequestType=0x00 (OUT, standard, device), bRequest=9
    // (SET_CONFIGURATION), wValue=1 (configuration value), wLength=0.
    let d0 = (9 << 8) | (1 << 16);
    enqueue_ep0(d0, 0, 8, (TRB_SETUP_STAGE << 10) | TRB_IDT); // TRT = No Data
                                                              // Status Stage (IN when there's no data stage), interrupt on completion.
    enqueue_ep0(0, 0, 0, (TRB_STATUS_STAGE << 10) | TRB_DIR_IN | TRB_IOC);

    if let Some(x) = controller() {
        x.ring_doorbell(slot, EP0_DCI);
    }
}

/// `UsbEnumeration.$Configured.$>`: the device is configured and usable. Latch
/// the slot so the transfer step (B6 Step 4) can address the device.
pub fn on_configured(slot: u8) {
    unsafe { (*curdev()).configured = true };
    serial::write_str("[usb] device configured (slot ");
    serial::write_u32_decimal(slot as u32);
    serial::writeln(")");
}

// --- USB transfer (B6 Step 4) ----------------------------------------------
//
// Configure the keyboard's interrupt-IN endpoint, then read a key report off it.
// `configure_endpoint` is native prep (a Configure Endpoint command); the
// `UsbTransfer` Frame system models the transfer itself (queue → complete).

/// Build an input context that adds EP1-IN (the boot keyboard's interrupt
/// endpoint), allocate its transfer ring, and issue a Configure Endpoint command
/// so the controller will service interrupt transfers on it. Non-blocking.
fn configure_endpoint(slot: u8) {
    let port = unsafe { (*curdev()).port };
    let Some(x) = controller() else { return };
    let speed = (x.portsc(port) >> 10) & 0xF;
    let cs = if x.ctx_64 { 64usize } else { 32 };

    let Some(ep1) = alloc_zeroed_page() else {
        return;
    };
    write_link_trb(ep1, ep1);
    unsafe {
        let d = curdev();
        (*d).ep1_ring_phys = ep1;
        (*d).ep1_enqueue = 0;
        (*d).ep1_pcs = 1;
    }

    let Some(ictx) = alloc_zeroed_page() else {
        return;
    };
    let v = frames::phys_to_virt(ictx);
    unsafe {
        // Input Control Context: Add A0 (slot — to bump Context Entries) + A3
        // (EP1-IN, DCI 3).
        write_volatile(v.add(4) as *mut u32, (1 << 0) | (1 << EP1_IN_DCI));
        // Slot Context: Context Entries = 3 (highest DCI now), Speed, Root port.
        write_volatile(v.add(cs) as *mut u32, (3 << 27) | (speed << 20));
        write_volatile(v.add(cs + 4) as *mut u32, (port as u32) << 16);
        // EP1-IN Context at DCI 3 → offset (1 + 3) * ctx_size.
        let ep = cs * (1 + EP1_IN_DCI as usize);
        // dword0: Interval (bits 23:16) — a polling period the controller accepts.
        write_volatile(v.add(ep) as *mut u32, 8 << 16);
        // dword1: EP Type = 7 (Interrupt IN, bits 5:3), CErr = 3 (bits 2:1),
        // Max Packet Size = 8 (boot report, bits 31:16).
        write_volatile(v.add(ep + 4) as *mut u32, (7 << 3) | (3 << 1) | (8 << 16));
        // TR Dequeue Pointer = EP1 ring | DCS(1).
        write_volatile(v.add(ep + 8) as *mut u32, (ep1 as u32) | 1);
        write_volatile(v.add(ep + 12) as *mut u32, (ep1 >> 32) as u32);
        write_volatile(v.add(ep + 16) as *mut u32, 8); // Average TRB Length
    }

    x.enqueue_cmd(
        ictx as u32,
        (ictx >> 32) as u32,
        0,
        (TRB_CONFIGURE_ENDPOINT << 10) | ((slot as u32) << 24),
    );
    x.ring_command_doorbell();
}

/// `UsbTransfer.$InFlight.$>`: queue an interrupt-IN transfer on EP1 — a Normal
/// TRB pointing at the report buffer (IOC + interrupt-on-short-packet), then ring
/// the EP1 doorbell. The keyboard completes it with a Transfer Event when a key
/// report arrives (driven by the harness's injected keypress).
pub fn queue_interrupt_in() {
    // Report DMA buffer (allocate once, per device).
    if unsafe { (*curdev()).report_buf_phys } == 0 {
        if let Some(b) = alloc_zeroed_page() {
            unsafe { (*curdev()).report_buf_phys = b };
        } else {
            return;
        }
    }
    let buf = unsafe { (*curdev()).report_buf_phys };
    let slot = unsafe { (*curdev()).slot };

    // Normal TRB on the EP1 ring: 8-byte boot report, IOC + ISP.
    let d = curdev();
    let ring_phys = unsafe { (*d).ep1_ring_phys };
    let mut enq = unsafe { (*d).ep1_enqueue };
    let mut pcs = unsafe { (*d).ep1_pcs };
    let ring = frames::phys_to_virt(ring_phys);
    if enq >= RING_TRBS - 1 {
        let link = unsafe { ring.add((RING_TRBS - 1) * TRB_SIZE) };
        unsafe { write_volatile(link.add(12) as *mut u32, (6 << 10) | (1 << 1) | pcs) };
        enq = 0;
        pcs ^= 1;
    }
    let trb = unsafe { ring.add(enq * TRB_SIZE) };
    unsafe {
        write_volatile(trb as *mut u32, buf as u32);
        write_volatile(trb.add(4) as *mut u32, (buf >> 32) as u32);
        write_volatile(trb.add(8) as *mut u32, 8); // TRB Transfer Length
                                                   // Normal, Interrupt-On-Completion (1<<5), Interrupt-on-Short-Packet (1<<2).
        write_volatile(
            trb.add(12) as *mut u32,
            (TRB_NORMAL << 10) | (1 << 5) | (1 << 2) | pcs,
        );
    }
    unsafe {
        (*d).ep1_enqueue = enq + 1;
        (*d).ep1_pcs = pcs;
    }

    if let Some(x) = controller() {
        x.ring_doorbell(slot, EP1_IN_DCI);
    }
    serial::writeln("[usb] waiting for key report");
}

/// `UsbTransfer.$Complete.$>`: the interrupt transfer completed — read + log the
/// 8-byte HID boot keyboard report (byte 0 = modifiers, byte 2 = first keycode).
pub fn on_report() {
    let buf = unsafe { (*curdev()).report_buf_phys };
    if buf == 0 {
        return;
    }
    let v = frames::phys_to_virt(buf);
    let modifiers = unsafe { read_volatile(v) };
    let keycode = unsafe { read_volatile(v.add(2)) };
    serial::write_str("[usb] HID report: modifiers ");
    serial::write_hex_u64(modifiers as u64);
    serial::write_str(" keycode ");
    serial::write_hex_u64(keycode as u64);
    serial::writeln("");
    serial::writeln("[usb] key transfer complete");
}

/// Wait (bounded, in the driver — not in a Frame handler) for the next Command
/// Completion Event, returning whether it was Success.
fn wait_cmd_completion(deadline: u64) -> bool {
    while interrupts::ticks() < deadline {
        if let Some(ev) = controller().and_then(|x| x.poll_event()) {
            if trb_type(ev[3]) == TRB_CMD_COMPLETION {
                return completion_code(ev[2]) == COMPLETION_SUCCESS;
            }
        }
        interrupts::wait_for_interrupt();
    }
    false
}

/// Drive the post-enumeration transfer: configure the keyboard's interrupt
/// endpoint, then complete one interrupt-IN transfer through the `UsbTransfer`
/// Frame system. Called from kmain after enumeration reaches `$Configured`.
pub fn run_transfer() {
    // Route by class, not table index: with a USB3 mass-storage device present,
    // the keyboard is no longer device 0.
    let Some(i) = keyboard_device() else {
        return; // no keyboard attached
    };
    set_cur_dev(i);
    let slot = unsafe { (*dslot(i)).slot };
    if slot == 0 {
        return;
    }
    interrupts::enable();

    // 1. Configure the interrupt endpoint (native prep) + wait for the command.
    configure_endpoint(slot);
    if !wait_cmd_completion(interrupts::ticks() + 100) {
        serial::writeln("[usb] configure endpoint failed");
        interrupts::disable();
        return;
    }
    serial::writeln("[usb] interrupt endpoint configured (EP1 IN)");

    // 2. The transfer itself: queue an interrupt-IN read and wait for the key
    //    report (the harness injects a keypress via the QEMU monitor).
    let mut t = UsbTransfer::__create();
    t.start(); // $InFlight.$> queues the transfer + logs "waiting for key report"
    let deadline = interrupts::ticks() + 600; // ~6s for the harness to send a key
    while interrupts::ticks() < deadline {
        if let Some(ev) = controller().and_then(|x| x.poll_event()) {
            if trb_type(ev[3]) == TRB_TRANSFER_EVENT {
                let code = completion_code(ev[2]);
                if code == COMPLETION_SUCCESS || code == COMPLETION_SHORT_PACKET {
                    t.complete(); // → $Complete ($> reads the report)
                } else {
                    t.fail();
                }
            }
        }
        if t.is_complete() || t.is_failed() {
            break;
        }
        interrupts::wait_for_interrupt();
    }
    if !t.is_complete() {
        serial::writeln("[usb] key report not received (no transfer)");
    }
    interrupts::disable();
}

/// Advance one device's `UsbEnumeration` FSM on a completion event, given the
/// event's TRB type + completion code. `CUR_DEV` must already point at this
/// device (so the FSM's enter-handler actions touch the right device's state).
/// This is the B6 single-device milestone dispatch, now keyed per device.
fn advance_enum(e: &mut UsbEnumeration, ty: u32, code: u32, slot: u8) {
    if ty == TRB_CMD_COMPLETION {
        if code != COMPLETION_SUCCESS {
            e.fail();
            return;
        }
        match e.state().as_str() {
            "Powered" => e.slot_enabled(slot), // → $SlotEnabled (issues Address Device)
            "SlotEnabled" => e.addressed(),    // → $AddressAssigned (issues GET_DESCRIPTOR)
            _ => {}
        }
    } else if ty == TRB_TRANSFER_EVENT {
        // A short packet on an IN transfer (fewer bytes than requested) is a
        // normal completion, not a failure.
        if code != COMPLETION_SUCCESS && code != COMPLETION_SHORT_PACKET {
            e.fail();
            return;
        }
        match e.state().as_str() {
            "AddressAssigned" => e.device_described(), // → $DeviceDescribed (SET_CONFIG)
            "DeviceDescribed" => e.configured(),       // → $Configured
            _ => {}
        }
    }
}

/// Enumerate every attached device **concurrently** (R3a). One `UsbEnumeration`
/// instance per device coexists in the `e` table; the single driver loop demuxes
/// each xHCI completion to the right instance by slot (`dev_by_slot`), pointing
/// `CUR_DEV` at it for the dispatch. This is the connection-table pattern (R2b)
/// applied to USB — but driven by *real asynchronous hardware completions* rather
/// than synthetic events: the "many concurrent lifecycle FSMs of one type" /
/// orthogonal-regions question, on hardware.
///
/// Slot assignment is the one serialized step: Enable Slot's completion carries
/// no port, so only one is outstanding at a time and an *unbound* returned slot
/// is bound to the requesting device. Everything after (Address Device, the EP0
/// descriptor + SET_CONFIG transfers) carries the slot, so it interleaves freely.
pub fn run_enumeration() {
    let n = device_count();
    if n == 0 {
        return;
    }
    interrupts::enable();
    let mut e: [Option<UsbEnumeration>; MAX_DEVICES] = [None, None, None, None];

    // Phase A — assign a slot to each device (one Enable Slot outstanding).
    for d in 0..n {
        set_cur_dev(d);
        e[d] = Some(UsbEnumeration::__create()); // $Powered.$> → Enable Slot
        let deadline = interrupts::ticks() + 200;
        let mut assigned = false;
        while !assigned && interrupts::ticks() < deadline {
            if let Some(ev) = controller().and_then(|x| x.poll_event()) {
                let ty = trb_type(ev[3]);
                let code = completion_code(ev[2]);
                let slot = event_slot(ev[3]);
                if ty == TRB_CMD_COMPLETION
                    && code == COMPLETION_SUCCESS
                    && dev_by_slot(slot).is_none()
                {
                    // An unbound slot → this device's Enable Slot result.
                    set_cur_dev(d);
                    e[d].as_mut().unwrap().slot_enabled(slot); // → Address Device
                    unsafe { (*dslot(d)).slot = slot };
                    assigned = true;
                } else if let Some(other) = dev_by_slot(slot) {
                    // A bound slot → another device's later completion; advance it.
                    set_cur_dev(other);
                    advance_enum(e[other].as_mut().unwrap(), ty, code, slot);
                }
            } else {
                interrupts::wait_for_interrupt();
            }
        }
        if !assigned {
            serial::writeln("[usb] enable slot timed out");
            set_cur_dev(d);
            e[d].as_mut().unwrap().fail();
        }
    }

    // Phase B — interleave the remaining stages until every device is done.
    let deadline = interrupts::ticks() + 400;
    loop {
        let all_done = (0..n).all(|d| {
            let inst = e[d].as_mut().unwrap();
            inst.is_configured() || inst.is_failed()
        });
        if all_done || interrupts::ticks() >= deadline {
            break;
        }
        if let Some(ev) = controller().and_then(|x| x.poll_event()) {
            let ty = trb_type(ev[3]);
            let code = completion_code(ev[2]);
            let slot = event_slot(ev[3]);
            if let Some(d) = dev_by_slot(slot) {
                set_cur_dev(d);
                advance_enum(e[d].as_mut().unwrap(), ty, code, slot);
            }
        } else {
            interrupts::wait_for_interrupt();
        }
    }

    // Report the outcome.
    let configured = (0..n)
        .filter(|&d| e[d].as_mut().unwrap().is_configured())
        .count();
    serial::write_str("[usb] enumerated ");
    serial::write_u32_decimal(configured as u32);
    serial::write_str(" of ");
    serial::write_u32_decimal(n as u32);
    serial::writeln(" devices");
    interrupts::disable();
}

// --- device classification (R3b) -------------------------------------------
//
// With more than one class of device attached (HID + mass storage), index in
// the device table no longer identifies *what* a device is — a USB3 mass-storage
// device sorts onto a lower port than the USB2 HID devices. So after enumeration
// we read each device's *configuration descriptor* and record the first
// interface's class/protocol + its bulk endpoint addresses. Routing (which
// device gets the keypress transfer, which gets SCSI) is then by class, not by
// table position — exactly what a real OS does.

const USB_DT_INTERFACE: u8 = 4;
const USB_DT_ENDPOINT: u8 = 5;
const CONFIG_DESC_LEN: u32 = 64; // enough for config + one interface + its endpoints

const CLASS_HID: u8 = 0x03;
const CLASS_MSD: u8 = 0x08; // Mass Storage
const EP_ATTR_BULK: u8 = 0x02; // bmAttributes transfer-type field == bulk

/// Wait (bounded, in the driver — not a Frame handler) for the next Transfer
/// Event, returning whether it completed OK (success or short packet).
fn wait_transfer_completion(deadline: u64) -> bool {
    while interrupts::ticks() < deadline {
        if let Some(ev) = controller().and_then(|x| x.poll_event()) {
            if trb_type(ev[3]) == TRB_TRANSFER_EVENT {
                let code = completion_code(ev[2]);
                return code == COMPLETION_SUCCESS || code == COMPLETION_SHORT_PACKET;
            }
        }
        interrupts::wait_for_interrupt();
    }
    false
}

/// Read device `i`'s configuration descriptor on EP0 and parse the first
/// interface's class/protocol and its bulk IN/OUT endpoint addresses into the
/// device table. Returns whether the read succeeded.
fn classify_device(i: usize) -> bool {
    set_cur_dev(i);
    let buf = unsafe { (*curdev()).desc_buf_phys };
    if buf == 0 {
        return false;
    }
    // Drain any leftover completion events from enumeration so the wait below
    // blocks for *this* config-descriptor read's completion, not a stale one.
    while let Some(x) = controller() {
        if x.poll_event().is_none() {
            break;
        }
    }
    // GET_DESCRIPTOR(Configuration, index 0): wValue=0x0200, wLength=CONFIG_DESC_LEN.
    let d0 = 0x80 | (6 << 8) | (0x0200 << 16);
    let d1 = CONFIG_DESC_LEN << 16;
    enqueue_ep0(d0, d1, 8, (TRB_SETUP_STAGE << 10) | TRB_IDT | TRT_IN);
    enqueue_ep0(
        buf as u32,
        (buf >> 32) as u32,
        CONFIG_DESC_LEN,
        (TRB_DATA_STAGE << 10) | TRB_DIR_IN,
    );
    enqueue_ep0(0, 0, 0, (TRB_STATUS_STAGE << 10) | TRB_IOC);
    let slot = unsafe { (*curdev()).slot };
    if let Some(x) = controller() {
        x.ring_doorbell(slot, EP0_DCI);
    }
    if !wait_transfer_completion(interrupts::ticks() + 100) {
        return false;
    }

    // Walk the returned descriptor blob by bLength. Record the FIRST interface's
    // class/protocol (class 0 is the "not yet seen" sentinel — never a real
    // interface class) and any bulk endpoint addresses.
    let v = frames::phys_to_virt(buf);
    let total =
        (unsafe { read_volatile(v.add(2) as *const u16) } as usize).min(CONFIG_DESC_LEN as usize);
    let mut off = unsafe { read_volatile(v) } as usize; // skip the config descriptor itself
    while off + 2 <= total {
        let blen = unsafe { read_volatile(v.add(off)) } as usize;
        if blen == 0 {
            break;
        }
        let dtype = unsafe { read_volatile(v.add(off + 1)) };
        if dtype == USB_DT_INTERFACE && blen >= 9 && unsafe { (*curdev()).iface_class } == 0 {
            unsafe {
                (*curdev()).iface_class = read_volatile(v.add(off + 5));
                (*curdev()).iface_protocol = read_volatile(v.add(off + 7));
            }
        } else if dtype == USB_DT_ENDPOINT && blen >= 7 {
            let addr = unsafe { read_volatile(v.add(off + 2)) };
            let attrs = unsafe { read_volatile(v.add(off + 3)) };
            if attrs & 0x3 == EP_ATTR_BULK {
                if addr & 0x80 != 0 {
                    unsafe { (*curdev()).bulk_in_ep = addr };
                } else {
                    unsafe { (*curdev()).bulk_out_ep = addr };
                }
            }
        }
        off += blen;
    }
    true
}

/// Classify every configured device (read its config descriptor) and log what
/// each is. Called from kmain after enumeration, before the class-specific
/// drivers (keypress transfer, SCSI) run.
pub fn classify_devices() {
    interrupts::enable();
    for i in 0..MAX_DEVICES {
        if !unsafe { (*dslot(i)).in_use && (*dslot(i)).configured } {
            continue;
        }
        classify_device(i);
        let (slot, cls, proto) = unsafe {
            let d = dslot(i);
            ((*d).slot, (*d).iface_class, (*d).iface_protocol)
        };
        let label = match (cls, proto) {
            (CLASS_HID, 1) => "HID keyboard",
            (CLASS_HID, 2) => "HID mouse",
            (CLASS_HID, _) => "HID",
            (CLASS_MSD, _) => "mass storage",
            _ => "other",
        };
        serial::write_str("[usb] slot ");
        serial::write_u32_decimal(slot as u32);
        serial::write_str(" is ");
        serial::write_str(label);
        serial::write_str(" (class ");
        serial::write_u32_decimal(cls as u32);
        serial::writeln(")");
    }
    interrupts::disable();
}

/// Find the first configured device of `class` (optionally matching `protocol`).
fn find_device(class: u8, protocol: Option<u8>) -> Option<usize> {
    (0..MAX_DEVICES).find(|&i| unsafe {
        let d = dslot(i);
        (*d).in_use
            && (*d).configured
            && (*d).iface_class == class
            && protocol.map_or(true, |p| (*d).iface_protocol == p)
    })
}
/// The configured HID keyboard, if attached (for the interrupt-IN keypress).
pub fn keyboard_device() -> Option<usize> {
    find_device(CLASS_HID, Some(1))
}
/// The configured mass-storage device, if attached (for SCSI over bulk).
pub fn mass_storage_device() -> Option<usize> {
    find_device(CLASS_MSD, None)
}

// --- USB mass storage: Bulk-Only Transport + SCSI (R3b) --------------------
//
// The native half of the `UsbMsd` Frame system: configure the device's bulk
// endpoints, build the CBW / SCSI CDB and read the data + CSW. Frame owns the
// three-phase BOT lifecycle; this owns the byte layout and the bulk-ring TRBs.

// SCSI operation codes we issue.
const SCSI_INQUIRY: u8 = 0x12;
const SCSI_READ_CAPACITY10: u8 = 0x25;
const SCSI_READ10: u8 = 0x28;

// xHCI endpoint types for the EP context (bits 5:3 of dword1).
const EP_TYPE_BULK_OUT: u32 = 2;
const EP_TYPE_BULK_IN: u32 = 6;

const CBW_SIGNATURE: u32 = 0x4342_5355; // 'USBC' little-endian
const CSW_SIGNATURE: u32 = 0x5342_5355; // 'USBS' little-endian
const CBW_LEN: u32 = 31;
const CSW_LEN: u32 = 13;

/// xHCI Device Context Index for a USB endpoint address (dir in bit 7, number in
/// bits 3:0): DCI = number * 2 + (1 if IN else 0).
fn dci_of(ep_addr: u8) -> u32 {
    let num = (ep_addr & 0x0F) as u32;
    num * 2 + if ep_addr & 0x80 != 0 { 1 } else { 0 }
}

/// Bulk max-packet size by USB speed (the PORTSC speed field): SS=1024, HS=512,
/// otherwise 64.
fn bulk_mps(speed: u32) -> u32 {
    match speed {
        4 => 1024, // SuperSpeed
        3 => 512,  // High-Speed
        _ => 64,
    }
}

/// The SCSI CDB + (cdb_len, data_in_len) for `cmd`. All three commands here read
/// data device-to-host. READ(10) reads LBA 0, one block (512 bytes).
fn scsi_cdb(cmd: u8) -> ([u8; 16], u8, u32) {
    let mut cdb = [0u8; 16];
    cdb[0] = cmd;
    match cmd {
        SCSI_INQUIRY => {
            cdb[4] = 36; // allocation length
            (cdb, 6, 36)
        }
        SCSI_READ_CAPACITY10 => (cdb, 10, 8),
        SCSI_READ10 => {
            // LBA 0 (bytes 2..6 big-endian, all zero), transfer length 1 block
            // (bytes 7..9 big-endian).
            cdb[8] = 1;
            (cdb, 10, 512)
        }
        _ => (cdb, 6, 0),
    }
}

/// Configure the mass-storage device's bulk IN + OUT endpoints (a Configure
/// Endpoint command adding both), allocating their transfer rings. Non-blocking
/// in the sense of the HID path's `configure_endpoint`; the caller waits for the
/// command completion.
fn configure_bulk_endpoints(i: usize) {
    set_cur_dev(i);
    let (port, in_ep, out_ep) = unsafe {
        let d = dslot(i);
        ((*d).port, (*d).bulk_in_ep, (*d).bulk_out_ep)
    };
    let Some(x) = controller() else { return };
    let speed = (x.portsc(port) >> 10) & 0xF;
    let mps = bulk_mps(speed);
    let cs = if x.ctx_64 { 64usize } else { 32 };
    let in_dci = dci_of(in_ep);
    let out_dci = dci_of(out_ep);
    let max_dci = in_dci.max(out_dci);

    let Some(in_ring) = alloc_zeroed_page() else {
        return;
    };
    let Some(out_ring) = alloc_zeroed_page() else {
        return;
    };
    write_link_trb(in_ring, in_ring);
    write_link_trb(out_ring, out_ring);
    unsafe {
        let d = curdev();
        (*d).bulk_in_ring_phys = in_ring;
        (*d).bulk_in_enq = 0;
        (*d).bulk_in_pcs = 1;
        (*d).bulk_out_ring_phys = out_ring;
        (*d).bulk_out_enq = 0;
        (*d).bulk_out_pcs = 1;
    }

    let Some(ictx) = alloc_zeroed_page() else {
        return;
    };
    let v = frames::phys_to_virt(ictx);
    unsafe {
        // Input Control Context: Add A0 (slot) | A(in_dci) | A(out_dci).
        write_volatile(
            v.add(4) as *mut u32,
            (1 << 0) | (1 << in_dci) | (1 << out_dci),
        );
        // Slot Context: Context Entries = highest DCI, speed, root port.
        write_volatile(v.add(cs) as *mut u32, (max_dci << 27) | (speed << 20));
        write_volatile(v.add(cs + 4) as *mut u32, (port as u32) << 16);
        // Bulk IN EP context.
        let ep_in = cs * (1 + in_dci as usize);
        write_volatile(
            v.add(ep_in + 4) as *mut u32,
            (EP_TYPE_BULK_IN << 3) | (3 << 1) | (mps << 16),
        );
        write_volatile(v.add(ep_in + 8) as *mut u32, (in_ring as u32) | 1);
        write_volatile(v.add(ep_in + 12) as *mut u32, (in_ring >> 32) as u32);
        write_volatile(v.add(ep_in + 16) as *mut u32, mps);
        // Bulk OUT EP context.
        let ep_out = cs * (1 + out_dci as usize);
        write_volatile(
            v.add(ep_out + 4) as *mut u32,
            (EP_TYPE_BULK_OUT << 3) | (3 << 1) | (mps << 16),
        );
        write_volatile(v.add(ep_out + 8) as *mut u32, (out_ring as u32) | 1);
        write_volatile(v.add(ep_out + 12) as *mut u32, (out_ring >> 32) as u32);
        write_volatile(v.add(ep_out + 16) as *mut u32, mps);
    }

    let slot = unsafe { (*curdev()).slot };
    x.enqueue_cmd(
        ictx as u32,
        (ictx >> 32) as u32,
        0,
        (TRB_CONFIGURE_ENDPOINT << 10) | ((slot as u32) << 24),
    );
    x.ring_command_doorbell();
}

/// Enqueue a Normal TRB `(buf, len)` on the current device's bulk IN or OUT ring,
/// stamping the producer cycle and following the Link TRB at wrap.
fn enqueue_bulk(is_in: bool, buf: u64, len: u32) {
    let d = curdev();
    let (ring_phys, mut enq, mut pcs) = unsafe {
        if is_in {
            ((*d).bulk_in_ring_phys, (*d).bulk_in_enq, (*d).bulk_in_pcs)
        } else {
            (
                (*d).bulk_out_ring_phys,
                (*d).bulk_out_enq,
                (*d).bulk_out_pcs,
            )
        }
    };
    let ring = frames::phys_to_virt(ring_phys);
    if enq >= RING_TRBS - 1 {
        let link = unsafe { ring.add((RING_TRBS - 1) * TRB_SIZE) };
        unsafe { write_volatile(link.add(12) as *mut u32, (6 << 10) | (1 << 1) | pcs) };
        enq = 0;
        pcs ^= 1;
    }
    let trb = unsafe { ring.add(enq * TRB_SIZE) };
    // Normal TRB, IOC (1<<5); Interrupt-on-Short-Packet (1<<2) for IN reads.
    let isp = if is_in { 1 << 2 } else { 0 };
    unsafe {
        write_volatile(trb as *mut u32, buf as u32);
        write_volatile(trb.add(4) as *mut u32, (buf >> 32) as u32);
        write_volatile(trb.add(8) as *mut u32, len);
        write_volatile(
            trb.add(12) as *mut u32,
            (TRB_NORMAL << 10) | (1 << 5) | isp | pcs,
        );
    }
    unsafe {
        if is_in {
            (*d).bulk_in_enq = enq + 1;
            (*d).bulk_in_pcs = pcs;
        } else {
            (*d).bulk_out_enq = enq + 1;
            (*d).bulk_out_pcs = pcs;
        }
    }
}

/// `UsbMsd.$CommandPhase.$>`: build the CBW for SCSI `cmd` and send it on the
/// bulk OUT endpoint.
pub fn msd_send_cbw(cmd: u8) {
    // Allocate the BOT DMA buffers once (per device).
    unsafe {
        if (*curdev()).cbw_buf_phys == 0 {
            let (Some(c), Some(dbuf), Some(s)) = (
                alloc_zeroed_page(),
                alloc_zeroed_page(),
                alloc_zeroed_page(),
            ) else {
                return;
            };
            (*curdev()).cbw_buf_phys = c;
            (*curdev()).data_buf_phys = dbuf;
            (*curdev()).csw_buf_phys = s;
        }
    }
    let (cdb, cdb_len, data_len) = scsi_cdb(cmd);
    let cbw = unsafe { (*curdev()).cbw_buf_phys };
    let v = frames::phys_to_virt(cbw);
    unsafe {
        write_volatile(v as *mut u32, CBW_SIGNATURE);
        write_volatile(v.add(4) as *mut u32, cmd as u32); // dCBWTag (use the opcode)
        write_volatile(v.add(8) as *mut u32, data_len); // dCBWDataTransferLength
        write_volatile(v.add(12), 0x80); // bmCBWFlags = data IN
        write_volatile(v.add(13), 0); // bCBWLUN
        write_volatile(v.add(14), cdb_len); // bCBWCBLength
        for (j, b) in cdb.iter().enumerate() {
            write_volatile(v.add(15 + j), *b);
        }
    }
    enqueue_bulk(false, cbw, CBW_LEN);
    let slot = unsafe { (*curdev()).slot };
    let out_dci = dci_of(unsafe { (*curdev()).bulk_out_ep });
    if let Some(x) = controller() {
        x.ring_doorbell(slot, out_dci);
    }
}

/// `UsbMsd.$DataPhase.$>`: read the command's data on the bulk IN endpoint.
pub fn msd_recv_data(cmd: u8) {
    let (_, _, data_len) = scsi_cdb(cmd);
    let buf = unsafe { (*curdev()).data_buf_phys };
    enqueue_bulk(true, buf, data_len);
    let slot = unsafe { (*curdev()).slot };
    let in_dci = dci_of(unsafe { (*curdev()).bulk_in_ep });
    if let Some(x) = controller() {
        x.ring_doorbell(slot, in_dci);
    }
}

/// `UsbMsd.$StatusPhase.$>`: read the 13-byte CSW on the bulk IN endpoint.
pub fn msd_recv_csw() {
    let csw = unsafe { (*curdev()).csw_buf_phys };
    enqueue_bulk(true, csw, CSW_LEN);
    let slot = unsafe { (*curdev()).slot };
    let in_dci = dci_of(unsafe { (*curdev()).bulk_in_ep });
    if let Some(x) = controller() {
        x.ring_doorbell(slot, in_dci);
    }
}

/// Whether the current device's CSW is valid: correct signature + status 0
/// (command passed). Read after the status-phase transfer completes.
fn csw_ok() -> bool {
    let v = frames::phys_to_virt(unsafe { (*curdev()).csw_buf_phys });
    let sig = unsafe { read_volatile(v as *const u32) };
    let status = unsafe { read_volatile(v.add(12)) };
    sig == CSW_SIGNATURE && status == 0
}

/// Parse + log the data a completed SCSI command returned.
fn msd_report(cmd: u8) {
    let data = frames::phys_to_virt(unsafe { (*curdev()).data_buf_phys });
    match cmd {
        SCSI_INQUIRY => {
            // Vendor ID @ byte 8 (8 chars), Product ID @ 16 (16 chars).
            serial::write_str("[msd] INQUIRY vendor '");
            for j in 8..16 {
                serial::write_byte(unsafe { read_volatile(data.add(j)) });
            }
            serial::write_str("' product '");
            for j in 16..32 {
                serial::write_byte(unsafe { read_volatile(data.add(j)) });
            }
            serial::writeln("'");
        }
        SCSI_READ_CAPACITY10 => {
            // Last LBA (big-endian @0) + block size (big-endian @4).
            let be = |o: usize| -> u32 {
                let b = |k: usize| unsafe { read_volatile(data.add(k)) } as u32;
                (b(o) << 24) | (b(o + 1) << 16) | (b(o + 2) << 8) | b(o + 3)
            };
            let last_lba = be(0);
            let blk = be(4);
            serial::write_str("[msd] capacity: ");
            serial::write_u32_decimal(last_lba + 1);
            serial::write_str(" blocks of ");
            serial::write_u32_decimal(blk);
            serial::writeln(" bytes");
        }
        SCSI_READ10 => {
            serial::write_str("[msd] block 0 first 8 bytes: ");
            for j in 0..8 {
                serial::write_byte(unsafe { read_volatile(data.add(j)) });
            }
            serial::writeln("");
        }
        _ => {}
    }
}

/// Drive the mass-storage device's SCSI sequence over Bulk-Only Transport: read
/// INQUIRY, READ CAPACITY(10), then READ(10) of block 0. Each command runs one
/// `UsbMsd` Frame instance through its CBW → data → CSW phases; native does the
/// bulk transfers + byte layout. Demonstrates a new device class + transfer type.
pub fn run_msd() {
    let Some(i) = mass_storage_device() else {
        return;
    };
    set_cur_dev(i);
    let slot = unsafe { (*dslot(i)).slot };
    serial::write_str("[usb] mass storage on slot ");
    serial::write_u32_decimal(slot as u32);
    serial::writeln(" — configuring bulk endpoints");

    interrupts::enable();
    configure_bulk_endpoints(i);
    if !wait_cmd_completion(interrupts::ticks() + 100) {
        serial::writeln("[msd] configure bulk endpoints failed");
        interrupts::disable();
        return;
    }
    serial::writeln("[usb] bulk endpoints configured (IN + OUT)");

    for cmd in [SCSI_INQUIRY, SCSI_READ_CAPACITY10, SCSI_READ10] {
        set_cur_dev(i);
        let mut m = UsbMsd::__create(); // $Idle
        m.begin(cmd); // → $CommandPhase ($> sends the CBW)
        let deadline = interrupts::ticks() + 200;
        while !m.is_complete() && !m.is_failed() && interrupts::ticks() < deadline {
            if let Some(ev) = controller().and_then(|x| x.poll_event()) {
                if trb_type(ev[3]) == TRB_TRANSFER_EVENT {
                    let code = completion_code(ev[2]);
                    let ok = code == COMPLETION_SUCCESS || code == COMPLETION_SHORT_PACKET;
                    set_cur_dev(i);
                    if !ok {
                        m.fail();
                    } else {
                        match m.state().as_str() {
                            "CommandPhase" => m.cbw_sent(),   // → $DataPhase
                            "DataPhase" => m.data_received(), // → $StatusPhase
                            // The CSW was just read; verify its signature + status
                            // before declaring success (a bad CSW → $Failed).
                            "StatusPhase" => {
                                if csw_ok() {
                                    m.status_received() // → $Complete
                                } else {
                                    m.fail()
                                }
                            }
                            _ => {}
                        }
                    }
                }
            } else {
                interrupts::wait_for_interrupt();
            }
        }
        set_cur_dev(i);
        if m.is_complete() {
            msd_report(cmd);
        } else {
            serial::writeln("[msd] command did not complete");
        }
    }
    interrupts::disable();
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

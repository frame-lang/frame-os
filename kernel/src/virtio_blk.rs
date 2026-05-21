// kernel/src/virtio_blk.rs
//
// Legacy virtio-blk driver (B4 Step 1). Pure native — the block device half of
// the storage stack. Speaks the legacy virtio PCI interface (an I/O BAR with
// the classic 0.9.5 register layout), which QEMU exposes with
// `-device virtio-blk-pci,disable-modern=on`.
//
// The post/drain split (the B4 framec gate): a request is submitted to the
// single virtqueue and the device raises a completion IRQ. The IRQ handler
// (`on_irq`) only *posts* — it acks the device ISR and sets a flag, touching
// no Frame system. The kernel *drains* from normal context (`read_sector` /
// `write_sector`'s wait loop), reading the used ring and driving the
// `BlockRequest` Frame system to $Complete/$Error.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::frame_systems::BlockRequest;
use crate::{frames, interrupts, io, pci, pic, serial};

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_BLK_DEVICE: u16 = 0x1001; // legacy/transitional virtio-blk
const VIRTIO_BLK_IRQ: u8 = 11;

// Legacy virtio PCI I/O register offsets (from the I/O BAR base).
const R_DEVICE_FEATURES: u16 = 0x00;
const R_DRIVER_FEATURES: u16 = 0x04;
const R_QUEUE_PFN: u16 = 0x08;
const R_QUEUE_SIZE: u16 = 0x0C;
const R_QUEUE_SELECT: u16 = 0x0E;
const R_QUEUE_NOTIFY: u16 = 0x10;
const R_STATUS: u16 = 0x12;
const R_ISR: u16 = 0x13;

// Device status bits.
const S_ACKNOWLEDGE: u8 = 1;
const S_DRIVER: u8 = 2;
const S_DRIVER_OK: u8 = 4;

// Virtqueue descriptor flags.
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;
const VRING_ALIGN: u64 = 4096;

// virtio-blk request types.
const BLK_T_IN: u32 = 0; // read (device → memory)
const BLK_T_OUT: u32 = 1; // write (memory → device)

pub const SECTOR_SIZE: usize = 512;

// Scratch-frame layout (one 4 KiB DMA frame): request header, status byte, and
// the 512-byte data buffer.
const OFF_HEADER: u64 = 0; // 16 bytes
const OFF_STATUS: u64 = 16; // 1 byte
const OFF_DATA: u64 = 512; // 512 bytes

struct Device {
    io_base: u16,
    qsize: u16,
    queue_virt: u64, // HHDM virt of the contiguous virtqueue region
    avail_off: u64,
    used_off: u64,
    avail_idx: u16,    // our running available index
    last_used: u16,    // last used-ring index we've drained
    scratch_phys: u64, // DMA scratch frame (header + status + data)
    scratch_virt: u64,
    present: bool,
}

static mut DEV: Device = Device {
    io_base: 0,
    qsize: 0,
    queue_virt: 0,
    avail_off: 0,
    used_off: 0,
    avail_idx: 0,
    last_used: 0,
    scratch_phys: 0,
    scratch_virt: 0,
    present: false,
};

// Posted by the IRQ handler, drained by the wait loop.
static IRQ_PENDING: AtomicBool = AtomicBool::new(false);

fn dev() -> &'static mut Device {
    let p = &raw mut DEV;
    unsafe { &mut *p }
}

// --- register helpers ------------------------------------------------------

fn status_write(base: u16, val: u8) {
    io::outb(base + R_STATUS, val);
}

// --- queue field accessors (raw, via the HHDM virt base) -------------------

fn desc_ptr(i: u16) -> *mut u8 {
    (dev().queue_virt + (i as u64) * 16) as *mut u8
}

unsafe fn set_desc(i: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let d = desc_ptr(i);
    (d as *mut u64).write(addr);
    (d.add(8) as *mut u32).write(len);
    (d.add(12) as *mut u16).write(flags);
    (d.add(14) as *mut u16).write(next);
}

fn avail_base() -> *mut u8 {
    (dev().queue_virt + dev().avail_off) as *mut u8
}
fn used_base() -> *mut u8 {
    (dev().queue_virt + dev().used_off) as *mut u8
}

// --- init (B4 Step 1b) -----------------------------------------------------

/// Probe + initialize the virtio-blk device: reset, negotiate (no features),
/// set up the single virtqueue in DMA memory, and enable the completion IRQ.
/// Returns false if no device is present.
pub fn init() -> bool {
    let Some(pcidev) = pci::find(VIRTIO_VENDOR, VIRTIO_BLK_DEVICE) else {
        serial::writeln("[blk] virtio-blk NOT found");
        return false;
    };
    pcidev.enable_io_and_bus_master();
    let base = pcidev.bar_io(0);
    let irq = pcidev.interrupt_line();

    // Reset, then ACKNOWLEDGE + DRIVER. Legacy: accept no optional features.
    status_write(base, 0);
    status_write(base, S_ACKNOWLEDGE);
    status_write(base, S_ACKNOWLEDGE | S_DRIVER);
    let _features = io::inl(base + R_DEVICE_FEATURES);
    io::outl(base + R_DRIVER_FEATURES, 0);

    // Select queue 0, read its size, lay out + allocate the virtqueue.
    io::outw(base + R_QUEUE_SELECT, 0);
    let qsize = io::inw(base + R_QUEUE_SIZE);
    let q = qsize as u64;
    let desc_size = 16 * q;
    let avail_off = desc_size;
    let avail_size = 6 + 2 * q;
    let used_off = (avail_off + avail_size + VRING_ALIGN - 1) & !(VRING_ALIGN - 1);
    let used_size = 6 + 8 * q;
    let total = used_off + used_size;
    let pages = total.div_ceil(VRING_ALIGN) as usize;
    let bytes = pages * 4096;

    let Some(queue_phys) = frames::alloc_contiguous(pages) else {
        serial::writeln("[blk] out of contiguous frames for virtqueue");
        return false;
    };
    let queue_virt = frames::phys_to_virt(queue_phys) as u64;
    unsafe {
        core::ptr::write_bytes(queue_virt as *mut u8, 0, bytes);
    }

    // Tell the device where the queue lives (legacy: a page frame number).
    io::outl(base + R_QUEUE_PFN, (queue_phys / VRING_ALIGN) as u32);

    // A DMA scratch frame for the request header + status + data.
    let Some(scratch_phys) = frames::alloc_frame() else {
        serial::writeln("[blk] out of frames for scratch");
        return false;
    };
    let scratch_virt = frames::phys_to_virt(scratch_phys) as u64;

    let d = dev();
    d.io_base = base;
    d.qsize = qsize;
    d.queue_virt = queue_virt;
    d.avail_off = avail_off;
    d.used_off = used_off;
    d.avail_idx = 0;
    d.last_used = 0;
    d.scratch_phys = scratch_phys;
    d.scratch_virt = scratch_virt;
    d.present = true;

    // Route the completion IRQ (QEMU wires virtio-blk to IRQ11, the slave PIC;
    // the IDT vector is fixed accordingly) and let the device run.
    pic::unmask_slave_irq(if (8..16).contains(&irq) {
        irq
    } else {
        VIRTIO_BLK_IRQ
    });
    status_write(base, S_ACKNOWLEDGE | S_DRIVER | S_DRIVER_OK);

    serial::write_str("[blk] virtio-blk ready: io 0x");
    serial::write_hex_u64(base as u64);
    serial::write_str(", irq ");
    serial::write_u32_decimal(irq as u32);
    serial::write_str(", queue size ");
    serial::write_u32_decimal(qsize as u32);
    serial::writeln("");
    true
}

// --- the post/drain I/O path (B4 Step 1c) ----------------------------------

/// IRQ post: ack the device ISR and flag a pending completion. Native and
/// interrupt-safe — no Frame dispatch here (that's `drain`'s job).
pub fn on_irq() {
    let d = dev();
    if d.present {
        let _ = io::inb(d.io_base + R_ISR); // read-to-ack
        IRQ_PENDING.store(true, Ordering::SeqCst);
    }
}

/// Submit a 3-descriptor request (header, data, status) for `sector` and ring
/// the doorbell. `write` selects BLK_T_OUT (memory → device) vs BLK_T_IN.
unsafe fn submit(sector: u64, write: bool) {
    let d = dev();
    // Header.
    let hdr = (d.scratch_virt + OFF_HEADER) as *mut u8;
    (hdr as *mut u32).write(if write { BLK_T_OUT } else { BLK_T_IN });
    (hdr.add(4) as *mut u32).write(0); // reserved
    (hdr.add(8) as *mut u64).write(sector);
    ((d.scratch_virt + OFF_STATUS) as *mut u8).write(0xFF); // sentinel

    // Descriptor chain: header (R) → data (R for write / W for read) → status (W).
    let data_flags = if write {
        VRING_DESC_F_NEXT
    } else {
        VRING_DESC_F_NEXT | VRING_DESC_F_WRITE
    };
    set_desc(0, d.scratch_phys + OFF_HEADER, 16, VRING_DESC_F_NEXT, 1);
    set_desc(
        1,
        d.scratch_phys + OFF_DATA,
        SECTOR_SIZE as u32,
        data_flags,
        2,
    );
    set_desc(2, d.scratch_phys + OFF_STATUS, 1, VRING_DESC_F_WRITE, 0);

    // Publish desc 0 as available, bump the avail idx, notify queue 0.
    let avail = avail_base();
    let ring = avail.add(4) as *mut u16; // ring[] starts after flags(2)+idx(2)
    ring.add((d.avail_idx % d.qsize) as usize).write(0);
    core::sync::atomic::fence(Ordering::SeqCst);
    d.avail_idx = d.avail_idx.wrapping_add(1);
    (avail.add(2) as *mut u16).write(d.avail_idx); // avail.idx
    core::sync::atomic::fence(Ordering::SeqCst);
    io::outw(d.io_base + R_QUEUE_NOTIFY, 0);
}

/// Wait (interrupts enabled) for the completion IRQ to post, then drain the
/// used ring and read the device status byte (0 = OK).
fn wait_and_drain() -> u8 {
    IRQ_PENDING.store(false, Ordering::SeqCst);
    interrupts::enable();
    while !IRQ_PENDING.load(Ordering::SeqCst) {
        interrupts::wait_for_interrupt();
    }
    interrupts::disable();
    let d = dev();
    // Advance our used cursor past the completion(s).
    let used_idx = unsafe { (used_base().add(2) as *const u16).read() };
    d.last_used = used_idx;
    unsafe { ((d.scratch_virt + OFF_STATUS) as *const u8).read() }
}

/// Read one 512-byte sector into `out`. Returns true on success. Drives a
/// `BlockRequest` through its lifecycle from the drained completion.
pub fn read_sector(sector: u64, out: &mut [u8; SECTOR_SIZE]) -> bool {
    if !dev().present {
        return false;
    }
    let mut br = BlockRequest::__create();
    br.submit(); // $Queued → $InFlight
    unsafe { submit(sector, false) };
    let status = wait_and_drain();
    if status == 0 {
        br.complete();
    } else {
        br.fail();
    }
    if br.is_complete() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                (dev().scratch_virt + OFF_DATA) as *const u8,
                out.as_mut_ptr(),
                SECTOR_SIZE,
            );
        }
        true
    } else {
        false
    }
}

/// Write one 512-byte sector from `data`. Returns true on success.
pub fn write_sector(sector: u64, data: &[u8; SECTOR_SIZE]) -> bool {
    if !dev().present {
        return false;
    }
    let mut br = BlockRequest::__create();
    br.submit();
    unsafe {
        core::ptr::copy_nonoverlapping(
            data.as_ptr(),
            (dev().scratch_virt + OFF_DATA) as *mut u8,
            SECTOR_SIZE,
        );
        submit(sector, true);
    }
    let status = wait_and_drain();
    if status == 0 {
        br.complete();
    } else {
        br.fail();
    }
    br.is_complete()
}

/// B4 Step 1 demo: init the device, write a known pattern to a sector, read it
/// back, and verify — exercising the full submit → IRQ → post → drain →
/// BlockRequest path.
pub fn run_demo() {
    if !init() {
        return;
    }
    // Use a high sector well clear of the filesystem's metadata + likely data
    // (the FS lives near the start of the disk); a raw write here can't corrupt
    // it, and the FS zeroes any block it later allocates.
    const SCRATCH_SECTOR: u64 = 1000;
    let mut wbuf = [0u8; SECTOR_SIZE];
    for (i, b) in wbuf.iter_mut().enumerate() {
        *b = (i as u8) ^ 0xA5;
    }
    if !write_sector(SCRATCH_SECTOR, &wbuf) {
        serial::writeln("[blk] write failed");
        return;
    }
    let mut rbuf = [0u8; SECTOR_SIZE];
    if !read_sector(SCRATCH_SECTOR, &mut rbuf) {
        serial::writeln("[blk] read failed");
        return;
    }
    if rbuf == wbuf {
        serial::writeln("[blk] sector write/read round-trip: ok");
    } else {
        serial::writeln("[blk] sector round-trip MISMATCH");
    }
}

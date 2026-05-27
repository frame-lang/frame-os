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

use core::sync::atomic::{AtomicU32, Ordering};

use crate::frame_systems::BlockRequest;
use crate::{frames, interrupts, io, pci, pic, sched, serial};

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

// Request-slot pool (multi-flight Step 1). Replaces the single shared scratch
// frame with N independent slots, each its own 4 KiB DMA frame and its own fixed
// descriptor triple — groundwork for overlapping requests. Submission is still
// serialized by the IoScheduler (one slot in flight at a time), so this step is
// a pure structural refactor with no behavior change; later steps make it
// concurrent (used-ring-element drain, then concurrent submit).
//
// Slot `i` owns:
//   - DMA frame at `slot_base + i*4096`, laid out header(16) + status(1) + data(512);
//   - descriptor triple [3i, 3i+1, 3i+2] (chain head 3i). qsize (128/256) ≫ 3N,
//     so static assignment never collides and the reverse map is `id / 3`.
const N_SLOTS: usize = 8;
const SLOT_FRAME: u64 = 4096; // one DMA frame per slot

// Per-slot DMA frame layout: request header, status byte, 512-byte data buffer.
const OFF_HEADER: u64 = 0; // 16 bytes
const OFF_STATUS: u64 = 16; // 1 byte
const OFF_DATA: u64 = 512; // 512 bytes

struct Device {
    io_base: u16,
    qsize: u16,
    queue_virt: u64, // HHDM virt of the contiguous virtqueue region
    avail_off: u64,
    used_off: u64,
    avail_idx: u16, // our running available index
    last_used: u16, // last used-ring index we've drained
    // Request-slot pool (multi-flight Step 1): N contiguous DMA frames, slot `i`
    // at `slot_base_* + i*SLOT_FRAME` (header/status/data). `slot_in_use[i]`
    // tracks allocation; submission is serialized so at most one is in flight.
    slot_base_phys: u64,
    slot_base_virt: u64,
    slot_in_use: [bool; N_SLOTS],
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
    slot_base_phys: 0,
    slot_base_virt: 0,
    slot_in_use: [false; N_SLOTS],
    present: false,
};

// (The completion signal is the `used.idx` advance, polled by `wait_and_drain`,
// which then reads the slot's status byte; the IRQ's only job is to wake a
// blocked waiter promptly via `DISK_WAITER`.)

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

// --- request-slot pool (multi-flight Step 1) -------------------------------

/// HHDM virt / phys base of slot `i`'s DMA frame.
fn slot_virt(i: usize) -> u64 {
    dev().slot_base_virt + i as u64 * SLOT_FRAME
}
fn slot_phys(i: usize) -> u64 {
    dev().slot_base_phys + i as u64 * SLOT_FRAME
}

/// Claim a free slot, marking it in-use. None if the pool is exhausted.
/// Submission is serialized by the IoScheduler today, so this never actually
/// contends — it's the pool API later steps lean on for concurrent in-flight
/// requests.
fn acquire_slot() -> Option<usize> {
    let d = dev();
    for i in 0..N_SLOTS {
        if !d.slot_in_use[i] {
            d.slot_in_use[i] = true;
            return Some(i);
        }
    }
    None
}

/// Return slot `i` to the pool.
fn release_slot(i: usize) {
    dev().slot_in_use[i] = false;
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

    // The request-slot pool: N contiguous DMA frames (one per slot), zeroed.
    let Some(slot_base_phys) = frames::alloc_contiguous(N_SLOTS) else {
        serial::writeln("[blk] out of frames for request slots");
        return false;
    };
    let slot_base_virt = frames::phys_to_virt(slot_base_phys) as u64;
    unsafe {
        core::ptr::write_bytes(slot_base_virt as *mut u8, 0, N_SLOTS * SLOT_FRAME as usize);
    }

    let d = dev();
    d.io_base = base;
    d.qsize = qsize;
    d.queue_virt = queue_virt;
    d.avail_off = avail_off;
    d.used_off = used_off;
    d.avail_idx = 0;
    d.last_used = 0;
    d.slot_base_phys = slot_base_phys;
    d.slot_base_virt = slot_base_virt;
    d.slot_in_use = [false; N_SLOTS];
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

/// The process pid blocked on a disk completion (0 = none / busy-wait path). The
/// IRQ handler wakes it. Single outstanding request (single-flight I/O).
static DISK_WAITER: AtomicU32 = AtomicU32::new(0);

/// IRQ post: ack the device ISR, flag the pending completion, and — if a process
/// is blocked on this read/write — wake it (B8 blocking I/O). Native and
/// interrupt-safe: no Frame dispatch here (that's `drain`'s job); `wake_pid` only
/// flips a TCB state.
pub fn on_irq() {
    let d = dev();
    if d.present {
        let _ = io::inb(d.io_base + R_ISR); // read-to-ack
        let waiter = DISK_WAITER.swap(0, Ordering::SeqCst);
        if waiter != 0 {
            sched::wake_pid(waiter);
        }
    }
}

// Disk transaction serialization (S6): the driver is still single-flight — a
// single completion `DISK_WAITER` and a `used.idx`-only completion test that
// assumes one request in flight. The request-slot pool (multi-flight Step 1)
// gives each request its own buffers + descriptor triple, removing the
// shared-buffer clobber that originally forced serialization — but submit stays
// serialized until the later steps (used-ring-element drain, then concurrent
// submit) make completion per-request. So `read_sector`/`write_sector` hold the
// disk engine for the whole transaction via the `IoScheduler` supervisor
// (`sched::acquire_disk`/`release_disk`): a process that finds it busy is queued
// and blocks, and the holder hands off to the next on release. The sequencing
// (owner, queue, hand-off) lives in that FSM; here we just bracket the txn.

/// Submit slot `slot`'s 3-descriptor request (header, data, status) for `sector`
/// and ring the doorbell. `write` selects BLK_T_OUT (memory → device) vs BLK_T_IN.
/// Slot `i` uses its own buffers (`slot_*(i)`) and descriptor triple `[3i,3i+1,3i+2]`
/// with chain head `3i`, so distinct slots never alias (groundwork for overlap).
unsafe fn submit(slot: usize, sector: u64, write: bool) {
    let d = dev();
    let sv = slot_virt(slot);
    let sp = slot_phys(slot);
    // Header.
    let hdr = (sv + OFF_HEADER) as *mut u8;
    (hdr as *mut u32).write(if write { BLK_T_OUT } else { BLK_T_IN });
    (hdr.add(4) as *mut u32).write(0); // reserved
    (hdr.add(8) as *mut u64).write(sector);
    ((sv + OFF_STATUS) as *mut u8).write(0xFF); // sentinel

    // Descriptor chain: header (R) → data (R for write / W for read) → status (W),
    // at this slot's triple [head, head+1, head+2].
    let head = (3 * slot) as u16;
    let data_flags = if write {
        VRING_DESC_F_NEXT
    } else {
        VRING_DESC_F_NEXT | VRING_DESC_F_WRITE
    };
    set_desc(head, sp + OFF_HEADER, 16, VRING_DESC_F_NEXT, head + 1);
    set_desc(
        head + 1,
        sp + OFF_DATA,
        SECTOR_SIZE as u32,
        data_flags,
        head + 2,
    );
    set_desc(head + 2, sp + OFF_STATUS, 1, VRING_DESC_F_WRITE, 0);

    // Publish the chain head in the avail ring, bump the avail idx, notify queue 0.
    let avail = avail_base();
    let ring = avail.add(4) as *mut u16; // ring[] starts after flags(2)+idx(2)
    ring.add((d.avail_idx % d.qsize) as usize).write(head);
    core::sync::atomic::fence(Ordering::SeqCst);
    d.avail_idx = d.avail_idx.wrapping_add(1);
    (avail.add(2) as *mut u16).write(d.avail_idx); // avail.idx
    core::sync::atomic::fence(Ordering::SeqCst);
    io::outw(d.io_base + R_QUEUE_NOTIFY, 0);
}

/// Wait for the in-flight request to complete, then return the device status
/// byte (0 = OK).
///
/// The completion signal is the **used-ring index** advancing — NOT the status
/// byte. Virtio only guarantees that the device has written *all* of a request's
/// buffers (data + status) once it bumps `used.idx`; the order in which the
/// individual buffers are written is unspecified. Polling the status byte alone
/// can therefore observe the status before the data DMA has landed — harmless for
/// a one-sector read, but tcc's heavy multi-sector reads occasionally got stale
/// data, producing a corrupt/missing `/out.elf`. Waiting on `used.idx` (then
/// reading status) is the spec-correct, race-free completion test, and — polled
/// via `block_current_until`, re-checked after every wake — it's also immune to a
/// lost/early wakeup (the bug that intermittently dropped the S5 redirect's
/// directory-entry write). Single-flight (the IoScheduler serializes whole
/// transactions), so `used.idx` advances exactly one per completed request.
fn wait_and_drain(slot: usize) -> u8 {
    let d = dev();
    let status_ptr = (slot_virt(slot) + OFF_STATUS) as *const u8;
    let used_idx_ptr = unsafe { (used_base() as *const u8).add(2) as *const u16 };
    let start = d.last_used; // device's used.idx before this request
    let done = move || unsafe { core::ptr::read_volatile(used_idx_ptr) } != start;
    let pid = sched::current_pid();
    if sched::is_preemption_active() && pid != 0 {
        // Blocking I/O: yield until the DMA completion. `on_irq` wakes us promptly
        // via DISK_WAITER, but correctness comes from `block_current_until`
        // re-checking `used.idx` — a wake can't make us return before completion.
        DISK_WAITER.store(pid, Ordering::SeqCst);
        sched::block_current_until(done);
        DISK_WAITER.store(0, Ordering::SeqCst);
    } else {
        // Boot / non-process context (no scheduler yet): busy-wait, interrupts on.
        interrupts::enable();
        while !done() {
            interrupts::wait_for_interrupt();
        }
        interrupts::disable();
    }
    // The request is fully written (used.idx advanced); record the new cursor and
    // read the now-valid status byte.
    d.last_used = unsafe { core::ptr::read_volatile(used_idx_ptr) };
    unsafe { core::ptr::read_volatile(status_ptr) }
}

/// Read one 512-byte sector into `out`. Returns true on success. Drives a
/// `BlockRequest` through its lifecycle from the drained completion.
pub fn read_sector(sector: u64, out: &mut [u8; SECTOR_SIZE]) -> bool {
    if !dev().present {
        return false;
    }
    sched::acquire_disk(); // serialize: hold the single-flight engine for this whole txn
    let Some(slot) = acquire_slot() else {
        sched::release_disk();
        return false;
    };
    let mut br = BlockRequest::__create();
    br.submit(); // $Queued → $InFlight
    unsafe { submit(slot, sector, false) };
    let status = wait_and_drain(slot);
    if status == 0 {
        br.complete();
    } else {
        br.fail();
    }
    let ok = if br.is_complete() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                (slot_virt(slot) + OFF_DATA) as *const u8,
                out.as_mut_ptr(),
                SECTOR_SIZE,
            );
        }
        true
    } else {
        false
    };
    release_slot(slot);
    sched::release_disk();
    ok
}

/// Write one 512-byte sector from `data`. Returns true on success.
pub fn write_sector(sector: u64, data: &[u8; SECTOR_SIZE]) -> bool {
    if !dev().present {
        return false;
    }
    sched::acquire_disk(); // serialize: hold the single-flight engine for this whole txn
    let Some(slot) = acquire_slot() else {
        sched::release_disk();
        return false;
    };
    let mut br = BlockRequest::__create();
    br.submit();
    unsafe {
        core::ptr::copy_nonoverlapping(
            data.as_ptr(),
            (slot_virt(slot) + OFF_DATA) as *mut u8,
            SECTOR_SIZE,
        );
        submit(slot, sector, true);
    }
    let status = wait_and_drain(slot);
    if status == 0 {
        br.complete();
    } else {
        br.fail();
    }
    let ok = br.is_complete();
    release_slot(slot);
    sched::release_disk();
    ok
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

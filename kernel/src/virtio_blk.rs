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
    // Per-slot completion state (multi-flight Step 2): `drain_used()` consumes
    // used-ring elements, maps each `id/3` back to its slot, records the status
    // byte, and sets `slot_done`. A waiter's completion predicate is its own
    // `slot_done[slot]` — so completion is per-request, not the old global
    // `used.idx`-advanced test.
    slot_done: [bool; N_SLOTS],
    slot_status: [u8; N_SLOTS],
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
    slot_done: [false; N_SLOTS],
    slot_status: [0; N_SLOTS],
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
    d.slot_done = [false; N_SLOTS];
    d.slot_status = [0; N_SLOTS];
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

/// Consume completed used-ring *elements* (multi-flight Step 2). For each new
/// entry from `last_used` to the device's `used.idx`, map its descriptor id back
/// to a slot (`id / 3` — slot `i` always uses chain head `3i`), read that slot's
/// status byte, and mark `slot_done[slot]`. Each waiter's completion predicate is
/// its own `slot_done` — so completion is per-request, replacing the old global
/// `used.idx`-advanced test. Native + interrupt-safe (reads + flag writes, no
/// Frame dispatch). The leading fence is the virtio read barrier: observe the
/// device's buffer + `used.idx` writes before consuming them. Still serialized
/// today (one in flight), so this drains exactly one element per request; Step 3
/// will also call it from `on_irq` to wake concurrent waiters by id.
fn drain_used() {
    let d = dev();
    let ub = used_base() as *const u8;
    core::sync::atomic::fence(Ordering::SeqCst);
    let used_idx = unsafe { core::ptr::read_volatile(ub.add(2) as *const u16) };
    while d.last_used != used_idx {
        let ring_i = (d.last_used % d.qsize) as usize;
        // used ring layout: flags(2) + idx(2) + ring[{id: u32, len: u32}; qsize].
        let id = unsafe { core::ptr::read_volatile(ub.add(4 + ring_i * 8) as *const u32) };
        let slot = (id / 3) as usize;
        if slot < N_SLOTS {
            let status =
                unsafe { core::ptr::read_volatile((slot_virt(slot) + OFF_STATUS) as *const u8) };
            d.slot_status[slot] = status;
            d.slot_done[slot] = true;
        }
        d.last_used = d.last_used.wrapping_add(1);
    }
}

/// Wait for slot `slot`'s request to complete, then return the device status
/// byte (0 = OK).
///
/// Completion is detected by `drain_used()` consuming the used-ring *element* for
/// this request and setting `slot_done[slot]` — NOT by polling the status byte
/// (the device may write status before the data DMA lands; the used-ring entry is
/// the spec-correct "all buffers written" signal). The predicate drains then
/// checks this slot's own flag, so it's per-request and — polled via
/// `block_current_until`, re-checked after every wake — immune to a lost/early
/// wakeup. Still serialized (the IoScheduler holds the engine for the whole txn),
/// so exactly one element drains per request here.
fn wait_and_drain(slot: usize) -> u8 {
    dev().slot_done[slot] = false; // fresh request; drain_used sets it on completion
    let done = move || {
        drain_used();
        dev().slot_done[slot]
    };
    let pid = sched::current_pid();
    if sched::is_preemption_active() && pid != 0 {
        // Blocking I/O: yield until the DMA completion. `on_irq` wakes us promptly
        // via DISK_WAITER, but correctness comes from `block_current_until`
        // re-checking the predicate — a wake can't make us return before this
        // slot's used-ring element has been drained.
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
    dev().slot_status[slot]
}

// --- block backend (transfer mechanism) ------------------------------------
//
// `read_sector`/`write_sector` keep the **Frame-system wrapper** — IoScheduler
// serialization (`acquire_disk`/`release_disk`) + the `BlockRequest` lifecycle —
// shared across BOTH builds. Only the transfer *mechanism* is swapped here: the
// default build drives the virtqueue (slot + `submit` + `wait_and_drain`); the
// interactive build copies to/from the in-kernel RAM disk (the #110 mitigation).
// So the Frame systems sit on a *critical* path in both configs — exercised over
// the real device in the smoke suite, and over RAM in the interactive shell —
// rather than being bypassed. The RAM backend also isolates #110: it keeps the
// Frame systems + `acquire_disk` and removes only the virtqueue/`wait_and_drain`/
// QEMU completion, so a green interactive run is evidence the hang lived in that
// transfer, not in the Frame systems.

/// Whether the block backend is ready (device present, or RAM disk loaded).
#[cfg(not(feature = "interactive"))]
fn backend_present() -> bool {
    dev().present
}
#[cfg(feature = "interactive")]
fn backend_present() -> bool {
    crate::ramdisk::is_loaded()
}

/// Transfer one sector device→`out`. Returns true on success.
#[cfg(not(feature = "interactive"))]
fn backend_read(sector: u64, out: &mut [u8; SECTOR_SIZE]) -> bool {
    let Some(slot) = acquire_slot() else {
        return false;
    };
    unsafe { submit(slot, sector, false) };
    let ok = wait_and_drain(slot) == 0;
    if ok {
        unsafe {
            core::ptr::copy_nonoverlapping(
                (slot_virt(slot) + OFF_DATA) as *const u8,
                out.as_mut_ptr(),
                SECTOR_SIZE,
            );
        }
    }
    release_slot(slot);
    ok
}
#[cfg(feature = "interactive")]
fn backend_read(sector: u64, out: &mut [u8; SECTOR_SIZE]) -> bool {
    crate::ramdisk::read_sector(sector, out)
}

/// Transfer one sector `data`→device. Returns true on success.
#[cfg(not(feature = "interactive"))]
fn backend_write(sector: u64, data: &[u8; SECTOR_SIZE]) -> bool {
    let Some(slot) = acquire_slot() else {
        return false;
    };
    unsafe {
        core::ptr::copy_nonoverlapping(
            data.as_ptr(),
            (slot_virt(slot) + OFF_DATA) as *mut u8,
            SECTOR_SIZE,
        );
        submit(slot, sector, true);
    }
    let ok = wait_and_drain(slot) == 0;
    release_slot(slot);
    ok
}
#[cfg(feature = "interactive")]
fn backend_write(sector: u64, data: &[u8; SECTOR_SIZE]) -> bool {
    crate::ramdisk::write_sector(sector, data)
}

/// Read one 512-byte sector into `out`. Returns true on success. The transaction
/// is serialized by the `IoScheduler` (`acquire_disk`) and tracked by a
/// `BlockRequest` FSM (both builds); the backend does the actual transfer.
pub fn read_sector(sector: u64, out: &mut [u8; SECTOR_SIZE]) -> bool {
    if !backend_present() {
        return false;
    }
    sched::acquire_disk(); // IoScheduler: serialize the whole transaction
    let mut br = BlockRequest::__create();
    br.submit(); // $Queued → $InFlight
    if backend_read(sector, out) {
        br.complete();
    } else {
        br.fail();
    }
    let ok = br.is_complete();
    sched::release_disk();
    ok
}

/// Write one 512-byte sector from `data`. Returns true on success.
pub fn write_sector(sector: u64, data: &[u8; SECTOR_SIZE]) -> bool {
    if !backend_present() {
        return false;
    }
    sched::acquire_disk();
    let mut br = BlockRequest::__create();
    br.submit();
    if backend_write(sector, data) {
        br.complete();
    } else {
        br.fail();
    }
    let ok = br.is_complete();
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

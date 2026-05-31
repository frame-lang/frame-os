// kernel/src/arch/aarch64/virtio_mmio.rs
//
// Minimal virtio-mmio (legacy/v1) block driver for QEMU's `virt` machine
// (B-HAL.5.3). The QEMU virt board exposes virtio devices over MMIO, not PCI:
// 32 fixed slots at PA 0x0a000000 + i*0x200, each a 0x200-byte register file.
// Slot probing reads MAGIC + VERSION + DEVICE_ID; on the first DEVICE_ID = 2
// (block) we run the legacy v1 init handshake and the queue 0 setup.
//
// What this driver does today (the smoke proof): probe, init, read sector 0,
// print the first ~48 bytes from it. It does NOT do interrupts (polled
// completion), does NOT integrate with the kernel's Frame `BlockRequest` /
// `IoScheduler` (that's the x86 transport's work — same Frame systems will
// drive this when virtio-mmio grows the IRQ + multi-slot path in B-HAL.5.4+).
//
// References: virtio 1.0 spec §4.2 (MMIO transport), §5.2 (block device).

use crate::frames;
use crate::serial;
use core::ptr::{read_volatile, write_volatile};

const VIRTIO_MMIO_BASE: u64 = 0x0a00_0000;
const VIRTIO_MMIO_STRIDE: u64 = 0x200;
const VIRTIO_MMIO_SLOTS: u64 = 32;

const VIRTIO_MAGIC: u32 = 0x7472_6976; // "virt"
const VIRTIO_VERSION_LEGACY: u32 = 1;
const VIRTIO_DEVICE_BLOCK: u32 = 2;

// MMIO register offsets (legacy + common).
const R_MAGIC: u64 = 0x000;
const R_VERSION: u64 = 0x004;
const R_DEVICE_ID: u64 = 0x008;
const R_DEVICE_FEATURES_SEL: u64 = 0x014;
const R_DRIVER_FEATURES: u64 = 0x020;
const R_DRIVER_FEATURES_SEL: u64 = 0x024;
const R_GUEST_PAGE_SIZE: u64 = 0x028; // legacy
const R_QUEUE_SEL: u64 = 0x030;
const R_QUEUE_NUM_MAX: u64 = 0x034;
const R_QUEUE_NUM: u64 = 0x038;
const R_QUEUE_ALIGN: u64 = 0x03c; // legacy
const R_QUEUE_PFN: u64 = 0x040; // legacy
const R_QUEUE_NOTIFY: u64 = 0x050;
const R_INTERRUPT_ACK: u64 = 0x064;
const R_STATUS: u64 = 0x070;

// STATUS register bits.
const STAT_ACK: u32 = 1;
const STAT_DRIVER: u32 = 2;
const STAT_DRIVER_OK: u32 = 4;

// VRing descriptor flags + alignment.
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;
const VRING_ALIGN: u64 = 4096;

// virtio-blk request header (16 B) — type + reserved + sector.
#[repr(C)]
struct BlkReq {
    type_: u32,
    reserved: u32,
    sector: u64,
}
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

const SECTOR_SIZE: usize = 512;

// ---------------------------------------------------------------------------
// MMIO accessors. Volatile because the device's view changes independently.
// ---------------------------------------------------------------------------

#[inline]
unsafe fn mmio_r32(base: u64, off: u64) -> u32 {
    unsafe { read_volatile((base + off) as *const u32) }
}
#[inline]
unsafe fn mmio_w32(base: u64, off: u64, v: u32) {
    unsafe { write_volatile((base + off) as *mut u32, v) };
}

// ---------------------------------------------------------------------------
// Probe — find the first virtio-mmio block device.
// ---------------------------------------------------------------------------

/// Walk the 32 fixed virtio-mmio slots; return the MMIO base of the first
/// slot whose device-id is 2 (block) — or None if the QEMU build didn't
/// expose one. Verifies MAGIC + VERSION too so we never act on a slot whose
/// device tree we haven't matched.
fn probe_block_device() -> Option<u64> {
    for i in 0..VIRTIO_MMIO_SLOTS {
        let base = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;
        let magic = unsafe { mmio_r32(base, R_MAGIC) };
        if magic != VIRTIO_MAGIC {
            continue;
        }
        let version = unsafe { mmio_r32(base, R_VERSION) };
        let device_id = unsafe { mmio_r32(base, R_DEVICE_ID) };
        if version == VIRTIO_VERSION_LEGACY && device_id == VIRTIO_DEVICE_BLOCK {
            return Some(base);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Init handshake (legacy v1, §3.1.1 of the virtio spec).
// ---------------------------------------------------------------------------

unsafe fn handshake(base: u64) -> u32 {
    // 1. Reset status.
    unsafe { mmio_w32(base, R_STATUS, 0) };
    // 2. ACK — we recognize the device.
    unsafe { mmio_w32(base, R_STATUS, STAT_ACK) };
    // 3. DRIVER — we know how to drive it.
    unsafe { mmio_w32(base, R_STATUS, STAT_ACK | STAT_DRIVER) };
    // 4. Negotiate features. For a smoke read we accept *no* features —
    //    write 0 to both 32-bit halves of DRIVER_FEATURES. The device will
    //    operate with its built-in defaults (legacy v1 doesn't require
    //    FEATURES_OK; modern would).
    unsafe { mmio_w32(base, R_DEVICE_FEATURES_SEL, 0) };
    unsafe { mmio_w32(base, R_DRIVER_FEATURES_SEL, 0) };
    unsafe { mmio_w32(base, R_DRIVER_FEATURES, 0) };
    unsafe { mmio_w32(base, R_DEVICE_FEATURES_SEL, 1) };
    unsafe { mmio_w32(base, R_DRIVER_FEATURES_SEL, 1) };
    unsafe { mmio_w32(base, R_DRIVER_FEATURES, 0) };
    // 5. Set GUEST_PAGE_SIZE = 4096 (legacy uses guest pages for queue PFN).
    unsafe { mmio_w32(base, R_GUEST_PAGE_SIZE, 4096) };
    // 6. Select queue 0, read its max size, program it.
    unsafe { mmio_w32(base, R_QUEUE_SEL, 0) };
    let qnum_max = unsafe { mmio_r32(base, R_QUEUE_NUM_MAX) };
    // We use a tiny queue (8 entries) — plenty for the demo. Cap to whatever
    // the device permits.
    let qnum = if qnum_max < 8 { qnum_max } else { 8 };
    unsafe { mmio_w32(base, R_QUEUE_NUM, qnum) };
    unsafe { mmio_w32(base, R_QUEUE_ALIGN, VRING_ALIGN as u32) };
    qnum
}

// ---------------------------------------------------------------------------
// Virtqueue layout (legacy, single descriptor table + avail ring + used ring
// in one contiguous chunk).
// ---------------------------------------------------------------------------

/// One vring descriptor (16 B, §2.4.5).
#[repr(C)]
struct VRingDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Vring avail header (4 B) — followed by `qnum` u16 ring entries + a u16
/// used_event for VIRTIO_F_EVENT_IDX (we don't enable that).
#[repr(C)]
struct VRingAvail {
    flags: u16,
    idx: u16,
    // ring: [u16; qnum] follows
}

/// Vring used entry (8 B each, §2.4.7).
#[repr(C)]
struct VRingUsedElem {
    id: u32,
    len: u32,
}

/// Vring used header.
#[repr(C)]
struct VRingUsed {
    flags: u16,
    idx: u16,
    // ring: [VRingUsedElem; qnum] follows
}

fn vring_total_bytes(qnum: u32) -> u64 {
    // §2.4.2: layout. Descriptor table + avail + (padding to align) + used.
    let desc = 16 * qnum as u64;
    let avail = 6 + 2 * qnum as u64; // flags + idx + ring + used_event
    let after = desc + avail;
    let used_off = (after + VRING_ALIGN - 1) & !(VRING_ALIGN - 1);
    let used = 6 + 8 * qnum as u64;
    used_off + used
}

// Holds the kernel's view of one virtqueue.
struct VQueue {
    base: u64,     // device MMIO base
    qnum: u32,     // queue size in entries
    desc_pa: u64,  // PA of descriptor table
    desc_va: u64,  // VA of descriptor table (identity-mapped, so == PA)
    avail_va: u64, // VA of avail ring header
    used_va: u64,  // VA of used ring header
}

unsafe fn setup_vqueue(base: u64, qnum: u32) -> VQueue {
    let total = vring_total_bytes(qnum);
    let pages = total.div_ceil(VRING_ALIGN) as usize;
    let queue_pa = frames::alloc_contiguous(pages).expect("virtqueue alloc");
    // Identity-mapped: PA == VA on aarch64.
    let queue_va = queue_pa;
    // Zero the whole region.
    unsafe { core::ptr::write_bytes(queue_va as *mut u8, 0, pages * VRING_ALIGN as usize) };

    let desc_va = queue_va;
    let avail_va = desc_va + 16 * qnum as u64;
    let used_off = (16 * qnum as u64 + 6 + 2 * qnum as u64 + VRING_ALIGN - 1) & !(VRING_ALIGN - 1);
    let used_va = desc_va + used_off;

    // Program the device: legacy uses QUEUE_PFN = phys / GUEST_PAGE_SIZE.
    unsafe { mmio_w32(base, R_QUEUE_PFN, (queue_pa / VRING_ALIGN) as u32) };

    VQueue {
        base,
        qnum,
        desc_pa: queue_pa,
        desc_va,
        avail_va,
        used_va,
    }
}

unsafe fn set_desc(q: &VQueue, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let d = (q.desc_va + i as u64 * 16) as *mut VRingDesc;
    unsafe {
        write_volatile(
            d,
            VRingDesc {
                addr,
                len,
                flags,
                next,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// I/O. Single in-flight request, polled completion. One pre-allocated buffer
// (16 B header + SECTOR_SIZE data + 1 B status) used by every transfer — the
// driver is single-threaded right now, so reuse is safe.
// ---------------------------------------------------------------------------

const REQ_OFFSET: usize = 0;
const DATA_OFFSET: usize = 16;
const STATUS_OFFSET: usize = 16 + SECTOR_SIZE;

/// The shared request region. Allocated lazily on the first transfer and
/// reused for the rest of this single-threaded driver's lifetime.
static mut REQ_BUF_PA: u64 = 0;

/// Direction of a virtio-blk request.
pub enum BlkDir {
    Read,
    Write,
}

fn req_buf() -> u64 {
    unsafe {
        let p = &raw mut REQ_BUF_PA;
        if p.read() == 0 {
            let pa = frames::alloc_contiguous(1).expect("blk req buf alloc");
            core::ptr::write_bytes(pa as *mut u8, 0, 4096);
            p.write(pa);
        }
        p.read()
    }
}

/// Submit one transfer at `sector`, hand-off `data` per direction, poll the
/// used ring until the device acks, return true iff the device reported
/// status 0. Pure native I/O — no FSM, no IRQ. The Frame `BlockRequest`
/// system wraps this with its lifecycle ($Submitted → $Complete/$Failed).
unsafe fn submit_and_wait(q: &VQueue, sector: u64, dir: BlkDir, data: &mut [u8]) -> bool {
    assert!(
        data.len() == SECTOR_SIZE,
        "virtio-mmio: data buf must be 512 B"
    );
    let buf_pa = req_buf();
    let buf = buf_pa as *mut u8;

    // Header.
    let hdr = (buf as u64 + REQ_OFFSET as u64) as *mut BlkReq;
    let type_ = match dir {
        BlkDir::Read => VIRTIO_BLK_T_IN,
        BlkDir::Write => VIRTIO_BLK_T_OUT,
    };
    unsafe {
        write_volatile(
            hdr,
            BlkReq {
                type_,
                reserved: 0,
                sector,
            },
        );
    }

    // For a write, copy the caller's data into the shared region before
    // handing it to the device. For a read, the device will fill it.
    if matches!(dir, BlkDir::Write) {
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                (buf as u64 + DATA_OFFSET as u64) as *mut u8,
                SECTOR_SIZE,
            );
        }
    }

    // Pre-poison the status byte so we can tell whether the device wrote it.
    let status = (buf as u64 + STATUS_OFFSET as u64) as *mut u8;
    unsafe { write_volatile(status, 0xFF) };

    // 3 chained descriptors. Device always reads the header. For IN, it
    // writes the data + status. For OUT, the data is also device-readable
    // (no WRITE flag), only the status is device-written.
    let data_flags = match dir {
        BlkDir::Read => VRING_DESC_F_NEXT | VRING_DESC_F_WRITE,
        BlkDir::Write => VRING_DESC_F_NEXT,
    };
    unsafe {
        set_desc(q, 0, buf_pa + REQ_OFFSET as u64, 16, VRING_DESC_F_NEXT, 1);
        set_desc(
            q,
            1,
            buf_pa + DATA_OFFSET as u64,
            SECTOR_SIZE as u32,
            data_flags,
            2,
        );
        set_desc(
            q,
            2,
            buf_pa + STATUS_OFFSET as u64,
            1,
            VRING_DESC_F_WRITE,
            0,
        );
    }

    // Push descriptor index 0 onto the avail ring and bump idx.
    let avail = q.avail_va as *mut VRingAvail;
    let avail_ring = (q.avail_va + 4) as *mut u16; // ring[] starts after flags+idx
    unsafe {
        let idx = read_volatile(&(*avail).idx);
        write_volatile(avail_ring.add((idx as usize) % q.qnum as usize), 0);
        // Ensure the descriptor + ring writes are visible before idx bump.
        core::arch::asm!("dsb st", options(nomem, nostack));
        write_volatile(&mut (*avail).idx, idx.wrapping_add(1));
    }

    // Notify the device that queue 0 has new work.
    unsafe { mmio_w32(q.base, R_QUEUE_NOTIFY, 0) };

    // Poll the used ring until idx advances.
    let used = q.used_va as *const VRingUsed;
    let initial_used = unsafe { read_volatile(&(*used).idx) };
    let mut spins = 0u64;
    while unsafe { read_volatile(&(*used).idx) } == initial_used {
        if spins > 200_000_000 {
            serial::writeln("[vio-mmio] I/O timeout (no used-ring advance)");
            return false;
        }
        spins += 1;
        core::hint::spin_loop();
    }
    // ACK any interrupt (we polled, but the device may have asserted it).
    let isr_mask = 0x3u32; // both ring + config change
    unsafe { mmio_w32(q.base, R_INTERRUPT_ACK, isr_mask) };

    // Check the status byte.
    let st = unsafe { read_volatile(status) };
    if st != 0 {
        serial::write_str("[vio-mmio] device status=0x");
        serial::write_hex_u64(st as u64);
        serial::writeln(" (expected 0)");
        return false;
    }

    // For a read, copy the device-written data back to the caller.
    if matches!(dir, BlkDir::Read) {
        unsafe {
            core::ptr::copy_nonoverlapping(
                (buf as u64 + DATA_OFFSET as u64) as *const u8,
                data.as_mut_ptr(),
                SECTOR_SIZE,
            );
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Public entry — the smoke demo. Read sector 0 (the marker the harness put
// there), then drive a write+read+verify round-trip on sector 1 through the
// `BlockRequest` Frame system — its $Submitted → $Complete lifecycle wraps
// each native transfer, the same pattern the x86 virtio-blk path uses. The
// BlockRequest FSM is `pure` (compiles for both ISAs since B-HAL.4.4), so
// the Frame layer comes for free on aarch64 now that there's a storage
// backend to drive it (B-HAL.5.4a).
// ---------------------------------------------------------------------------

use crate::frame_systems::BlockRequest;

/// Run one block-I/O transfer wrapped in a fresh `BlockRequest` FSM, return
/// true iff both the native I/O succeeded *and* the FSM reached $Complete.
unsafe fn run_request(q: &VQueue, sector: u64, dir: BlkDir, data: &mut [u8]) -> bool {
    let mut req = BlockRequest::__create();
    req.submit(); // $Idle → $Submitted
    let io_ok = unsafe { submit_and_wait(q, sector, dir, data) };
    if io_ok {
        req.complete(); // $Submitted → $Complete
    } else {
        req.fail(); // $Submitted → $Error
    }
    io_ok && req.is_complete()
}

/// Probe for a virtio-mmio block device, init it, read sector 0, then do a
/// write+read+verify round-trip on sector 1 driven through `BlockRequest`.
pub fn run_demo() {
    serial::writeln("[vio-mmio] probing for virtio-mmio block device...");
    let base = match probe_block_device() {
        Some(b) => b,
        None => {
            serial::writeln("[vio-mmio] no virtio-mmio block device found");
            return;
        }
    };
    serial::write_str("[vio-mmio] found block device at MMIO 0x");
    serial::write_hex_u64(base);
    serial::writeln("");

    let qnum = unsafe { handshake(base) };
    serial::write_str("[vio-mmio] handshake ok; queue 0 size = ");
    serial::write_u32_decimal(qnum);
    serial::writeln("");

    let q = unsafe { setup_vqueue(base, qnum) };

    // Mark device live.
    unsafe { mmio_w32(base, R_STATUS, STAT_ACK | STAT_DRIVER | STAT_DRIVER_OK) };

    // --- Read sector 0 through a BlockRequest FSM, print marker -----------
    let mut sec0 = [0u8; SECTOR_SIZE];
    if unsafe { run_request(&q, 0, BlkDir::Read, &mut sec0) } {
        serial::write_str("[vio-mmio] sector 0: \"");
        for &b in sec0.iter().take(48) {
            if b == 0 || b == b'\n' {
                break;
            }
            serial::write_byte(b);
        }
        serial::writeln("\"");
        serial::writeln("[vio-mmio] sector 0 read: ok");
    }

    // --- Round-trip on sector 1: write known pattern, read back, verify ---
    let mut wbuf = [0u8; SECTOR_SIZE];
    let pattern = b"FrameOS-aarch64-RW-rountrip-OK";
    wbuf[..pattern.len()].copy_from_slice(pattern);
    let write_ok = unsafe { run_request(&q, 1, BlkDir::Write, &mut wbuf) };

    let mut rbuf = [0u8; SECTOR_SIZE];
    let read_ok = unsafe { run_request(&q, 1, BlkDir::Read, &mut rbuf) };

    let bytes_match = rbuf[..pattern.len()] == *pattern;
    if write_ok && read_ok && bytes_match {
        serial::writeln("[vio-mmio] sector 1 write+read+verify via BlockRequest FSM: ok");
    } else {
        serial::write_str("[vio-mmio] sector 1 round-trip FAILED: write=");
        serial::write_str(if write_ok { "ok" } else { "fail" });
        serial::write_str(" read=");
        serial::write_str(if read_ok { "ok" } else { "fail" });
        serial::write_str(" match=");
        serial::writeln(if bytes_match { "ok" } else { "fail" });
    }
}

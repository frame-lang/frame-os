// kernel/src/ramdisk.rs
//
// RAM-backed block device for the `interactive` build (the #110 mitigation).
//
// The whole filesystem image is baked into the kernel at build time
// (`include_bytes!(env!("FRAMEOS_BAKED_FS"))`, the image xtask assembles) and
// copied into a writable RAM buffer at boot. Every `read_sector`/`write_sector`
// is then a plain in-memory `memcpy` — no virtqueue, no DMA, no completion IRQ,
// nothing for QEMU's emulated-disk completion path to lose or starve. That path
// is exactly where the #110 "mv hang" lives, so serving the disk from RAM makes
// the interactive shell flake-proof regardless of host load.
//
// EPHEMERAL: writes live only in RAM and reset to the pristine baked image on
// each boot — which matches how the disk is already used (every test run starts
// from a fresh image; nothing relies on cross-boot persistence). The durable
// path remains virtio-blk → a real disk file, still used + validated by the
// (non-interactive) smoke suite and by real hardware. A write-back variant
// (flush dirty blocks to the device at shutdown) is a documented future upgrade.
//
// Single-core / BSP-only, like the fs + buffer cache above it, so no locking.

use crate::serial;
use crate::virtio_blk::SECTOR_SIZE;

/// The filesystem image baked into the kernel by xtask (see `build.rs`). Its
/// length is the disk size (BLK_DISK_BLOCKS × 512).
static BAKED_FS: &[u8] = include_bytes!(env!("FRAMEOS_BAKED_FS"));

/// Disk size in bytes. Must match xtask's `BLK_DISK_BLOCKS` (16384 × 512 = 8 MiB);
/// `init` asserts the baked image matches, failing loudly on any drift.
const DISK_BYTES: usize = 16384 * SECTOR_SIZE;

/// The writable in-RAM disk, seeded from `BAKED_FS` at boot.
static mut DISK: [u8; DISK_BYTES] = [0; DISK_BYTES];

fn disk() -> &'static mut [u8; DISK_BYTES] {
    let p = &raw mut DISK;
    unsafe { &mut *p }
}

/// Copy the baked image into the writable RAM disk. Called once at boot before
/// `fs::mount()`. Panics (loudly, early) if the baked image size doesn't match
/// the compiled disk size — a build-config mismatch we never want to limp past.
pub fn init() {
    assert!(
        BAKED_FS.len() == DISK_BYTES,
        "baked fs image size mismatch: image is not the expected disk size"
    );
    disk().copy_from_slice(BAKED_FS);
    serial::write_str("[ramdisk] ");
    serial::write_u32_decimal((DISK_BYTES / 1024) as u32);
    serial::writeln(" KiB RAM disk loaded from baked image");
}

/// Read one 512-byte sector into `out`. Returns false if `block` is out of range.
pub fn read_sector(block: u64, out: &mut [u8; SECTOR_SIZE]) -> bool {
    let start = block as usize * SECTOR_SIZE;
    let Some(end) = start.checked_add(SECTOR_SIZE) else {
        return false;
    };
    if end > DISK_BYTES {
        return false;
    }
    out.copy_from_slice(&disk()[start..end]);
    true
}

/// Write one 512-byte sector from `data` (RAM-only). Returns false if `block` is
/// out of range.
pub fn write_sector(block: u64, data: &[u8; SECTOR_SIZE]) -> bool {
    let start = block as usize * SECTOR_SIZE;
    let Some(end) = start.checked_add(SECTOR_SIZE) else {
        return false;
    };
    if end > DISK_BYTES {
        return false;
    }
    disk()[start..end].copy_from_slice(data);
    true
}

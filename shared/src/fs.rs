// shared/src/fs.rs
//
// On-disk format for the Frame OS filesystem (B4) — a minimal xv6-style inode
// FS. Shared (no_std, no alloc) between the kernel driver (`kernel/src/fs.rs`)
// and the host `mkfs` tool (`xtask`) so the byte layout is defined exactly
// once. Pure layout + (de)serialization; no I/O.
//
// Disk layout (512-byte blocks):
//   block 0           superblock (magic + total block count)
//   block 1           free-block bitmap (1 bit per block; 1 block ⇒ ≤4096 blocks)
//   blocks 2..6       inode table (4 blocks × 8 inodes = 32 inodes)
//   blocks 6..        data blocks
//
// Inode 0 is reserved/unused; inode 1 is the root directory. A directory's
// data blocks hold 32-byte dirents (name[28] + inode u32).

pub const BLOCK_SIZE: usize = 512;
pub const MAGIC: u32 = 0xF5A5_0F50; // "frame fs"

// Block layout. The superblock is fixed at block 0 and the free-block bitmap
// starts at block 1; everything after the bitmap (inode table, then data) shifts
// with the disk size, because the bitmap grows to cover the whole disk. Use
// `Layout::for_total(total_blocks)` to compute the offsets — `BITMAP_BLOCK` /
// `INODE_START` / `DATA_START` are no longer fixed (B9.5: scalable FS).
pub const SB_BLOCK: u32 = 0;
pub const BITMAP_START: u32 = 1;

/// Bits in one bitmap block — one bit per disk block (`512 * 8 = 4096`).
pub const BITS_PER_BLOCK: u32 = (BLOCK_SIZE * 8) as u32;
/// Block pointers in one indirect block (`512 / 4 = 128`).
pub const PTRS_PER_BLOCK: usize = BLOCK_SIZE / 4;

pub const INODE_SIZE: usize = 128;
pub const INODES_PER_BLOCK: usize = BLOCK_SIZE / INODE_SIZE; // 4
pub const INODE_BLOCKS: u32 = 16;
pub const INODE_COUNT: u32 = INODE_BLOCKS * INODES_PER_BLOCK as u32; // 64

// Block map: 28 direct + single-indirect + double-indirect. Max file =
// 28*512 + 128*512 + 128*128*512 ≈ 8.07 MiB. The inode is exactly 128 bytes:
// kind(2)+nlink(2)+size(4)+direct[28]*4(112)+indirect(4)+double_indirect(4).
pub const NDIRECT: usize = 28;
pub const NAME_LEN: usize = 28;
pub const DIRENT_SIZE: usize = 32; // name[28] + inode(u32)
pub const DIRENTS_PER_BLOCK: usize = BLOCK_SIZE / DIRENT_SIZE; // 16

pub const ROOT_INODE: u32 = 1;

// Inode types.
pub const T_FREE: u16 = 0;
pub const T_FILE: u16 = 1;
pub const T_DIR: u16 = 2;

/// The on-disk layout for a given disk size (B9.5). The bitmap covers every
/// block on the disk (so it scales with the disk), and the inode table + data
/// region follow it. Both the kernel (cached at mount, from the superblock's
/// `total_blocks`) and the host `mkfs` derive offsets through this, so they
/// always agree.
#[derive(Clone, Copy)]
pub struct Layout {
    pub total_blocks: u32,
    pub bitmap_blocks: u32,
    pub inode_start: u32,
    pub data_start: u32,
}

impl Layout {
    /// Compute the layout for a disk of `total_blocks` blocks.
    pub fn for_total(total_blocks: u32) -> Layout {
        let bitmap_blocks = total_blocks.div_ceil(BITS_PER_BLOCK);
        let inode_start = BITMAP_START + bitmap_blocks;
        let data_start = inode_start + INODE_BLOCKS;
        Layout {
            total_blocks,
            bitmap_blocks,
            inode_start,
            data_start,
        }
    }

    /// Block holding inode `ino`, and the byte offset within it.
    pub fn inode_loc(&self, ino: u32) -> (u32, usize) {
        (
            self.inode_start + ino / INODES_PER_BLOCK as u32,
            (ino as usize % INODES_PER_BLOCK) * INODE_SIZE,
        )
    }

    /// For disk block `block`, the bitmap block that tracks it plus the byte +
    /// bit within that block. The bitmap has one bit per block, blocks
    /// `BITMAP_START..` holding bits `0..` in order.
    pub fn bitmap_loc(&self, block: u32) -> (u32, usize, u32) {
        let bm_block = BITMAP_START + block / BITS_PER_BLOCK;
        let within = block % BITS_PER_BLOCK;
        (bm_block, (within / 8) as usize, within % 8)
    }
}

// --- little-endian field helpers -------------------------------------------

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn wr_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// The superblock (block 0): magic + total block count.
#[derive(Clone, Copy)]
pub struct Superblock {
    pub magic: u32,
    pub total_blocks: u32,
}

impl Superblock {
    pub fn parse(block: &[u8]) -> Superblock {
        Superblock {
            magic: rd_u32(block, 0),
            total_blocks: rd_u32(block, 4),
        }
    }
    pub fn write(&self, block: &mut [u8]) {
        wr_u32(block, 0, self.magic);
        wr_u32(block, 4, self.total_blocks);
    }
}

/// An on-disk inode (128 bytes): type, link count, size, 28 direct block
/// pointers, plus single- and double-indirect pointers (B9.5).
#[derive(Clone, Copy)]
pub struct Inode {
    pub kind: u16,
    pub nlink: u16,
    pub size: u32,
    pub direct: [u32; NDIRECT],
    pub indirect: u32,
    pub double_indirect: u32,
}

impl Inode {
    pub const fn empty() -> Inode {
        Inode {
            kind: T_FREE,
            nlink: 0,
            size: 0,
            direct: [0; NDIRECT],
            indirect: 0,
            double_indirect: 0,
        }
    }

    /// Parse the inode at byte offset `off` within an inode-table block.
    pub fn parse(block: &[u8], off: usize) -> Inode {
        let mut direct = [0u32; NDIRECT];
        for (i, d) in direct.iter_mut().enumerate() {
            *d = rd_u32(block, off + 8 + i * 4);
        }
        Inode {
            kind: rd_u16(block, off),
            nlink: rd_u16(block, off + 2),
            size: rd_u32(block, off + 4),
            direct,
            indirect: rd_u32(block, off + 8 + NDIRECT * 4),
            double_indirect: rd_u32(block, off + 12 + NDIRECT * 4),
        }
    }

    /// Write this inode at byte offset `off` within an inode-table block.
    pub fn write(&self, block: &mut [u8], off: usize) {
        wr_u16(block, off, self.kind);
        wr_u16(block, off + 2, self.nlink);
        wr_u32(block, off + 4, self.size);
        for (i, d) in self.direct.iter().enumerate() {
            wr_u32(block, off + 8 + i * 4, *d);
        }
        wr_u32(block, off + 8 + NDIRECT * 4, self.indirect);
        wr_u32(block, off + 12 + NDIRECT * 4, self.double_indirect);
    }

    /// Number of data blocks this inode's `size` spans.
    pub fn nblocks(&self) -> usize {
        (self.size as usize).div_ceil(BLOCK_SIZE)
    }
}

/// Read a dirent (name, inode) at byte offset `off` within a directory block.
/// Returns the name length (bytes up to the first NUL).
pub fn read_dirent(block: &[u8], off: usize) -> ([u8; NAME_LEN], u32) {
    let mut name = [0u8; NAME_LEN];
    name.copy_from_slice(&block[off..off + NAME_LEN]);
    (name, rd_u32(block, off + NAME_LEN))
}

/// Write a dirent (name, inode) at byte offset `off` within a directory block.
pub fn write_dirent(block: &mut [u8], off: usize, name: &[u8], ino: u32) {
    let mut buf = [0u8; NAME_LEN];
    let n = core::cmp::min(name.len(), NAME_LEN);
    buf[..n].copy_from_slice(&name[..n]);
    block[off..off + NAME_LEN].copy_from_slice(&buf);
    wr_u32(block, off + NAME_LEN, ino);
}

/// Compare a dirent name (NUL-padded) against `name`.
pub fn name_eq(dirent_name: &[u8; NAME_LEN], name: &[u8]) -> bool {
    let n = core::cmp::min(name.len(), NAME_LEN);
    if n < NAME_LEN && dirent_name[n] != 0 {
        return false; // dirent name is longer
    }
    &dirent_name[..n] == name && (n == NAME_LEN || dirent_name[n] == 0)
}

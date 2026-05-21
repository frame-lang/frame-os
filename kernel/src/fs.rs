// kernel/src/fs.rs
//
// The Frame OS filesystem driver (B4 Step 2). Pure native — the FS mechanism
// over the virtio-blk driver. A minimal xv6-style inode FS (format in
// `frame_os_shared::fs`): superblock, free-block bitmap, inode table, and a
// single-level root directory of dirents.
//
// A small write-through buffer cache sits between the FS and the block driver
// so repeated metadata reads (superblock, bitmap, inode table) don't re-hit the
// disk. The `Mount` Frame system owns the mount lifecycle; this module owns the
// inode/dirent/bitmap mechanics.

use frame_os_shared::fs;

use crate::{serial, virtio_blk};

type Block = [u8; fs::BLOCK_SIZE];

// --- buffer cache (write-through, direct-mapped) ---------------------------

const CACHE_SLOTS: usize = 16;

#[derive(Clone, Copy)]
struct Slot {
    block: u32,
    valid: bool,
    data: Block,
}

static mut CACHE: [Slot; CACHE_SLOTS] = [Slot {
    block: 0,
    valid: false,
    data: [0; fs::BLOCK_SIZE],
}; CACHE_SLOTS];

fn cache() -> &'static mut [Slot; CACHE_SLOTS] {
    let p = &raw mut CACHE;
    unsafe { &mut *p }
}

/// Read block `b` (via the cache).
fn read_block(b: u32) -> Block {
    let slot = (b as usize) % CACHE_SLOTS;
    let c = &mut cache()[slot];
    if c.valid && c.block == b {
        return c.data;
    }
    let mut buf = [0u8; fs::BLOCK_SIZE];
    virtio_blk::read_sector(b as u64, &mut buf);
    c.block = b;
    c.valid = true;
    c.data = buf;
    buf
}

/// Write block `b` (write-through: disk + cache).
fn write_block(b: u32, data: &Block) {
    virtio_blk::write_sector(b as u64, data);
    let slot = (b as usize) % CACHE_SLOTS;
    let c = &mut cache()[slot];
    c.block = b;
    c.valid = true;
    c.data = *data;
}

// --- inode / bitmap / dirent helpers ---------------------------------------

fn read_inode(ino: u32) -> fs::Inode {
    let (blk, off) = fs::inode_loc(ino);
    fs::Inode::parse(&read_block(blk), off)
}

fn write_inode(ino: u32, node: &fs::Inode) {
    let (blk, off) = fs::inode_loc(ino);
    let mut b = read_block(blk);
    node.write(&mut b, off);
    write_block(blk, &b);
}

/// Allocate a free data block (≥ DATA_START), marking it used in the bitmap.
fn alloc_block() -> Option<u32> {
    let mut bm = read_block(fs::BITMAP_BLOCK);
    let total = fs::Superblock::parse(&read_block(fs::SB_BLOCK)).total_blocks;
    for b in fs::DATA_START..total {
        let (byte, bit) = ((b / 8) as usize, b % 8);
        if bm[byte] & (1 << bit) == 0 {
            bm[byte] |= 1 << bit;
            write_block(fs::BITMAP_BLOCK, &bm);
            // Zero the freshly allocated block.
            write_block(b, &[0u8; fs::BLOCK_SIZE]);
            return Some(b);
        }
    }
    None
}

fn free_block(b: u32) {
    let mut bm = read_block(fs::BITMAP_BLOCK);
    let (byte, bit) = ((b / 8) as usize, b % 8);
    bm[byte] &= !(1 << bit);
    write_block(fs::BITMAP_BLOCK, &bm);
}

/// Allocate a free inode number (≥ 2).
fn alloc_inode() -> Option<u32> {
    (2..fs::INODE_COUNT).find(|&ino| read_inode(ino).kind == fs::T_FREE)
}

// --- public FS API ---------------------------------------------------------

/// Validate the on-disk superblock. The `Mount` Frame system gates on this.
pub fn check_superblock() -> bool {
    fs::Superblock::parse(&read_block(fs::SB_BLOCK)).magic == fs::MAGIC
}

/// Look up `name` in the root directory; returns its inode number.
pub fn lookup(name: &[u8]) -> Option<u32> {
    let root = read_inode(fs::ROOT_INODE);
    let entries = root.size as usize / fs::DIRENT_SIZE;
    let mut seen = 0usize;
    for &blk in root.direct.iter() {
        if blk == 0 {
            continue;
        }
        let data = read_block(blk);
        let mut off = 0;
        while off + fs::DIRENT_SIZE <= fs::BLOCK_SIZE && seen < entries {
            let (dname, ino) = fs::read_dirent(&data, off);
            seen += 1;
            if ino != 0 && fs::name_eq(&dname, name) {
                return Some(ino);
            }
            off += fs::DIRENT_SIZE;
        }
    }
    None
}

/// Read up to `out.len()` bytes of file `ino` into `out`. Returns bytes read.
pub fn read_file(ino: u32, out: &mut [u8]) -> usize {
    let node = read_inode(ino);
    let size = node.size as usize;
    let want = size.min(out.len());
    let mut done = 0;
    for &blk in node.direct.iter() {
        if done >= want {
            break;
        }
        if blk == 0 {
            break;
        }
        let data = read_block(blk);
        let n = (want - done).min(fs::BLOCK_SIZE);
        out[done..done + n].copy_from_slice(&data[..n]);
        done += n;
    }
    done
}

/// Create an empty file `name` in the root directory; returns its inode.
pub fn create(name: &[u8]) -> Option<u32> {
    if lookup(name).is_some() {
        return None;
    }
    let ino = alloc_inode()?;
    let mut node = fs::Inode::empty();
    node.kind = fs::T_FILE;
    node.nlink = 1;
    write_inode(ino, &node);
    // Add a dirent to the root directory's first data block.
    let mut root = read_inode(fs::ROOT_INODE);
    let dblk = root.direct[0];
    let mut data = read_block(dblk);
    let off = root.size as usize;
    if off + fs::DIRENT_SIZE > fs::BLOCK_SIZE {
        return None; // root dir full (single block at Step 2)
    }
    fs::write_dirent(&mut data, off, name, ino);
    write_block(dblk, &data);
    root.size += fs::DIRENT_SIZE as u32;
    write_inode(fs::ROOT_INODE, &root);
    Some(ino)
}

/// Overwrite file `ino` with `data` (allocating data blocks as needed).
pub fn write_file(ino: u32, data: &[u8]) -> bool {
    let mut node = read_inode(ino);
    let nb = data.len().div_ceil(fs::BLOCK_SIZE);
    if nb > fs::NDIRECT {
        return false;
    }
    for (i, slot) in node.direct.iter_mut().enumerate().take(nb) {
        if *slot == 0 {
            match alloc_block() {
                Some(b) => *slot = b,
                None => return false,
            }
        }
        let lo = i * fs::BLOCK_SIZE;
        let hi = ((i + 1) * fs::BLOCK_SIZE).min(data.len());
        let mut buf = [0u8; fs::BLOCK_SIZE];
        buf[..hi - lo].copy_from_slice(&data[lo..hi]);
        write_block(*slot, &buf);
    }
    node.size = data.len() as u32;
    write_inode(ino, &node);
    true
}

/// Delete `name`: free its blocks + inode and clear its dirent.
pub fn delete(name: &[u8]) -> bool {
    let Some(ino) = lookup(name) else {
        return false;
    };
    let mut node = read_inode(ino);
    for &blk in node.direct.iter() {
        if blk != 0 {
            free_block(blk);
        }
    }
    node = fs::Inode::empty(); // mark $free
    write_inode(ino, &node);

    // Clear the dirent (set its inode to 0; lookup skips inode 0).
    let root = read_inode(fs::ROOT_INODE);
    let dblk = root.direct[0];
    let mut data = read_block(dblk);
    let entries = root.size as usize / fs::DIRENT_SIZE;
    for i in 0..entries {
        let off = i * fs::DIRENT_SIZE;
        let (dname, dino) = fs::read_dirent(&data, off);
        if dino == ino && fs::name_eq(&dname, name) {
            fs::write_dirent(&mut data, off, b"", 0);
            write_block(dblk, &data);
            return true;
        }
    }
    true
}

// --- B4 Step 2 demo --------------------------------------------------------

/// Mount the FS (via the `Mount` HSM), read a pre-populated file, then run a
/// create → write → read → delete round-trip.
pub fn run_demo() {
    let mut mount = crate::frame_systems::Mount::__create();
    mount.begin_mount(); // $Unmounted → $Mounting
    if check_superblock() {
        mount.mounted_ok(); // → $Mounted
    } else {
        mount.mount_failed();
        serial::writeln("[fs] mount failed: bad superblock");
        return;
    }
    if !mount.is_mounted() {
        return;
    }
    serial::writeln("[fs] mounted");

    // Read a file the host mkfs put on the disk.
    if let Some(ino) = lookup(b"motd") {
        let mut buf = [0u8; 64];
        let n = read_file(ino, &mut buf);
        serial::write_str("[fs] /motd: ");
        for &c in &buf[..n] {
            serial::write_byte(c);
        }
    } else {
        serial::writeln("[fs] /motd not found");
    }

    // create → write → read → delete round-trip.
    let payload = b"frame-os scratch data";
    let Some(ino) = create(b"scratch") else {
        serial::writeln("[fs] create failed");
        return;
    };
    if !write_file(ino, payload) {
        serial::writeln("[fs] write failed");
        return;
    }
    let mut rbuf = [0u8; 64];
    let n = read_file(lookup(b"scratch").unwrap(), &mut rbuf);
    if &rbuf[..n] == payload {
        serial::writeln("[fs] create/write/read round-trip: ok");
    } else {
        serial::writeln("[fs] round-trip MISMATCH");
    }
    delete(b"scratch");
    if lookup(b"scratch").is_none() {
        serial::writeln("[fs] delete: ok");
    } else {
        serial::writeln("[fs] delete FAILED");
    }

    // Persistence across reboot: the first boot creates a marker file; a later
    // boot of the *same* disk reads it back. (The reboot smoke test boots twice
    // on one disk without re-mkfs'ing between.)
    const MARKER: &[u8] = b"frame-os-persists";
    match lookup(b"persist") {
        Some(ino) => {
            let mut b = [0u8; 32];
            let n = read_file(ino, &mut b);
            if &b[..n] == MARKER {
                serial::writeln("[fs] persistence verified across reboot");
            } else {
                serial::writeln("[fs] persistence marker CORRUPT");
            }
        }
        None => {
            if let Some(ino) = create(b"persist") {
                write_file(ino, MARKER);
                serial::writeln("[fs] persist marker created");
            }
        }
    }

    mount.begin_unmount();
    mount.unmounted();
}

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

// --- on-disk layout (cached at mount) --------------------------------------

// The disk layout (bitmap/inode/data offsets) is derived from the superblock's
// `total_blocks` and depends on disk size (the bitmap scales). Cache it at mount
// so every helper agrees without re-deriving. Set by `check_superblock`.
static mut LAYOUT: fs::Layout = fs::Layout {
    total_blocks: 0,
    bitmap_blocks: 0,
    inode_start: 0,
    data_start: 0,
};

fn layout() -> fs::Layout {
    unsafe { (&raw const LAYOUT).read() }
}

// --- inode / bitmap / dirent helpers ---------------------------------------

fn read_inode(ino: u32) -> fs::Inode {
    let (blk, off) = layout().inode_loc(ino);
    fs::Inode::parse(&read_block(blk), off)
}

fn write_inode(ino: u32, node: &fs::Inode) {
    let (blk, off) = layout().inode_loc(ino);
    let mut b = read_block(blk);
    node.write(&mut b, off);
    write_block(blk, &b);
}

/// Allocate a free data block (≥ data_start), marking it used in the bitmap.
/// The bitmap spans multiple blocks (B9.5); consecutive candidates share a
/// bitmap block, so the buffer cache keeps this cheap.
fn alloc_block() -> Option<u32> {
    let l = layout();
    for b in l.data_start..l.total_blocks {
        let (bm_block, byte, bit) = l.bitmap_loc(b);
        let mut bm = read_block(bm_block);
        if bm[byte] & (1 << bit) == 0 {
            bm[byte] |= 1 << bit;
            write_block(bm_block, &bm);
            // Zero the freshly allocated block.
            write_block(b, &[0u8; fs::BLOCK_SIZE]);
            return Some(b);
        }
    }
    None
}

fn free_block(b: u32) {
    let (bm_block, byte, bit) = layout().bitmap_loc(b);
    let mut bm = read_block(bm_block);
    bm[byte] &= !(1 << bit);
    write_block(bm_block, &bm);
}

/// Allocate a free inode number (≥ 2).
fn alloc_inode() -> Option<u32> {
    (2..fs::INODE_COUNT).find(|&ino| read_inode(ino).kind == fs::T_FREE)
}

// --- block map: direct + single/double indirect (B9.5) ---------------------

/// Read (or, with `alloc`, allocate) the `slot`-th pointer inside indirect block
/// `iblk`, returning the block it points at. Without `alloc`, returns None for an
/// empty slot. Writes the indirect block back when it allocates a pointer.
fn indirect_slot(iblk: u32, slot: usize, alloc: bool) -> Option<u32> {
    let mut blk = read_block(iblk);
    let off = slot * 4;
    let cur = u32::from_le_bytes([blk[off], blk[off + 1], blk[off + 2], blk[off + 3]]);
    if cur != 0 {
        return Some(cur);
    }
    if !alloc {
        return None;
    }
    let new = alloc_block()?;
    blk[off..off + 4].copy_from_slice(&new.to_le_bytes());
    write_block(iblk, &blk);
    Some(new)
}

/// Map file block index `bi` to its physical disk block. With `alloc`, lazily
/// allocates data + the single/double-indirect index blocks as needed (mutating
/// `node` — the caller must `write_inode` afterward); without, returns None for a
/// hole / past the allocated extent. Tiers: 28 direct, then one indirect block
/// (128 ptrs), then a double-indirect block (128×128 ptrs).
fn block_for(node: &mut fs::Inode, bi: usize, alloc: bool) -> Option<u32> {
    let ptrs = fs::PTRS_PER_BLOCK;
    if bi < fs::NDIRECT {
        if node.direct[bi] == 0 {
            if !alloc {
                return None;
            }
            node.direct[bi] = alloc_block()?;
        }
        return Some(node.direct[bi]);
    }
    let bi = bi - fs::NDIRECT;
    if bi < ptrs {
        if node.indirect == 0 {
            if !alloc {
                return None;
            }
            node.indirect = alloc_block()?;
        }
        return indirect_slot(node.indirect, bi, alloc);
    }
    let bi = bi - ptrs;
    if bi >= ptrs * ptrs {
        return None; // beyond the double-indirect extent (max file)
    }
    if node.double_indirect == 0 {
        if !alloc {
            return None;
        }
        node.double_indirect = alloc_block()?;
    }
    // First level selects a single-indirect block; second level the data block.
    let mid = indirect_slot(node.double_indirect, bi / ptrs, alloc)?;
    indirect_slot(mid, bi % ptrs, alloc)
}

// --- public FS API ---------------------------------------------------------

/// Validate the on-disk superblock and cache the disk layout (B9.5). The `Mount`
/// Frame system gates on this; it runs before any inode/block access, so the
/// cached `LAYOUT` is set before anything reads it.
pub fn check_superblock() -> bool {
    let sb = fs::Superblock::parse(&read_block(fs::SB_BLOCK));
    if sb.magic != fs::MAGIC {
        return false;
    }
    unsafe { (&raw mut LAYOUT).write(fs::Layout::for_total(sb.total_blocks)) };
    true
}

/// Look up `name` in directory inode `dir_ino`; returns its inode number.
pub fn dir_lookup(dir_ino: u32, name: &[u8]) -> Option<u32> {
    let dir = read_inode(dir_ino);
    if dir.kind != fs::T_DIR {
        return None;
    }
    let entries = dir.size as usize / fs::DIRENT_SIZE;
    let mut seen = 0usize;
    for &blk in dir.direct.iter() {
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

/// Look up `name` in the root directory.
pub fn lookup(name: &[u8]) -> Option<u32> {
    dir_lookup(fs::ROOT_INODE, name)
}

/// List the entries of directory `path` into `out` as NUL-separated names,
/// returning the bytes written (truncated if `out` fills). `None` if `path`
/// doesn't resolve to a directory. Backs the `readdir` syscall (#21) / `ls`.
pub fn list_dir(path: &[u8], out: &mut [u8]) -> Option<usize> {
    let ino = namei(path)?;
    let dir = read_inode(ino);
    if dir.kind != fs::T_DIR {
        return None;
    }
    let entries = dir.size as usize / fs::DIRENT_SIZE;
    let mut seen = 0usize;
    let mut w = 0usize;
    for &blk in dir.direct.iter() {
        if blk == 0 {
            continue;
        }
        let data = read_block(blk);
        let mut off = 0;
        while off + fs::DIRENT_SIZE <= fs::BLOCK_SIZE && seen < entries {
            let (dname, dino) = fs::read_dirent(&data, off);
            seen += 1;
            off += fs::DIRENT_SIZE;
            if dino == 0 {
                continue; // free/cleared slot (e.g. after unlink)
            }
            // The name field is NUL-padded within NAME_LEN; take up to the NUL.
            let nlen = dname.iter().position(|&c| c == 0).unwrap_or(dname.len());
            if w + nlen + 1 > out.len() {
                return Some(w); // out of space — return what fit
            }
            out[w..w + nlen].copy_from_slice(&dname[..nlen]);
            w += nlen;
            out[w] = 0; // NUL separator
            w += 1;
        }
    }
    Some(w)
}

/// Resolve an absolute path (`/a/b/c`) to an inode by walking directories from
/// the root. Empty components (leading/trailing/double slashes) are skipped.
pub fn namei(path: &[u8]) -> Option<u32> {
    let mut ino = fs::ROOT_INODE;
    for comp in path.split(|&c| c == b'/') {
        if comp.is_empty() {
            continue;
        }
        ino = dir_lookup(ino, comp)?;
    }
    Some(ino)
}

/// Read up to `out.len()` bytes of file `ino` starting at byte `offset`.
/// Returns bytes read (0 at or past EOF).
pub fn read_at(ino: u32, offset: usize, out: &mut [u8]) -> usize {
    let mut node = read_inode(ino);
    let size = node.size as usize;
    if offset >= size {
        return 0;
    }
    let want = (size - offset).min(out.len());
    let mut done = 0;
    while done < want {
        let pos = offset + done;
        let bi = pos / fs::BLOCK_SIZE;
        let Some(blk) = block_for(&mut node, bi, false) else {
            break;
        };
        let within = pos % fs::BLOCK_SIZE;
        let block = read_block(blk);
        let n = (want - done).min(fs::BLOCK_SIZE - within);
        out[done..done + n].copy_from_slice(&block[within..within + n]);
        done += n;
    }
    done
}

/// Whether `ino` is a regular file.
pub fn is_file(ino: u32) -> bool {
    read_inode(ino).kind == fs::T_FILE
}

/// Whether `ino` is a directory.
pub fn is_dir(ino: u32) -> bool {
    read_inode(ino).kind == fs::T_DIR
}

/// Resolve a possibly-relative `path` against the absolute `cwd` into a single
/// canonical absolute path written to `out`; returns the byte length, or `None`
/// if it doesn't fit. Pure string canonicalization (B11-3 follow-up): collapses
/// "." / "" components and pops on ".." (a ".." at the root stays at root).
/// Used by the path syscalls so relative paths honor the caller's cwd.
pub fn resolve(cwd: &[u8], path: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut len = 0usize; // canonical bytes in `out` (no trailing '/'; 0 == root)
    let mut ends = [0usize; 64]; // end offset of each pushed component
    let mut depth = 0usize;

    // An absolute path ignores cwd; a relative one is rooted at cwd.
    let absolute = matches!(path.first(), Some(&b'/'));
    let sources: [&[u8]; 2] = if absolute { [b"", path] } else { [cwd, path] };

    for src in sources {
        for comp in src.split(|&c| c == b'/') {
            if comp.is_empty() || comp == b"." {
                continue;
            }
            if comp == b".." {
                if depth > 0 {
                    depth -= 1;
                    len = if depth > 0 { ends[depth - 1] } else { 0 };
                }
                continue;
            }
            if depth >= ends.len() || len + 1 + comp.len() > out.len() {
                return None;
            }
            out[len] = b'/';
            len += 1;
            out[len..len + comp.len()].copy_from_slice(comp);
            len += comp.len();
            ends[depth] = len;
            depth += 1;
        }
    }
    if len == 0 {
        // Root: emit "/".
        if out.is_empty() {
            return None;
        }
        out[0] = b'/';
        return Some(1);
    }
    Some(len)
}

/// Read up to `out.len()` bytes of file `ino` into `out`. Returns bytes read.
pub fn read_file(ino: u32, out: &mut [u8]) -> usize {
    read_at(ino, 0, out)
}

/// Append a dirent (`name` → `ino`) at the end of directory `pino`, growing it
/// into a new data block when the current one fills. DIRENT_SIZE (32) evenly
/// divides BLOCK_SIZE (512 = 16 dirents), so entries never straddle a block;
/// the new entry goes at logical offset `dir.size`, whose block is allocated on
/// demand via `block_for`. Directories use **direct blocks only** (the read side,
/// `dir_lookup`/`list_dir`, iterates only `dir.direct`), so this caps a directory
/// at NDIRECT×16 = 448 entries and returns false past that or on out-of-space.
fn dir_link(pino: u32, name: &[u8], ino: u32) -> bool {
    let mut dir = read_inode(pino);
    let off = dir.size as usize;
    let bi = off / fs::BLOCK_SIZE;
    if bi >= fs::NDIRECT {
        return false; // directories are direct-only
    }
    let within = off % fs::BLOCK_SIZE;
    let Some(dblk) = block_for(&mut dir, bi, true) else {
        return false;
    };
    let mut data = read_block(dblk);
    fs::write_dirent(&mut data, within, name, ino);
    write_block(dblk, &data);
    dir.size += fs::DIRENT_SIZE as u32;
    write_inode(pino, &dir);
    true
}

/// Split an absolute path into (parent directory inode, final component name).
/// `None` if the parent doesn't resolve to a directory.
fn split_parent(path: &[u8]) -> Option<(u32, &[u8])> {
    let (parent_path, name): (&[u8], &[u8]) = match path.iter().rposition(|&c| c == b'/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (b"", path),
    };
    let pino = if parent_path.is_empty() {
        fs::ROOT_INODE
    } else {
        namei(parent_path)?
    };
    if !is_dir(pino) {
        return None;
    }
    Some((pino, name))
}

/// Clear the dirent for (`ino`, `name`) in directory `pino` (set inode 0; lookups
/// skip it). Returns true if it was found + cleared.
fn clear_dirent(pino: u32, ino: u32, name: &[u8]) -> bool {
    let parent = read_inode(pino);
    let entries = parent.size as usize / fs::DIRENT_SIZE;
    let mut seen = 0usize;
    for &blk in parent.direct.iter() {
        if blk == 0 {
            continue;
        }
        let mut data = read_block(blk);
        let mut off = 0;
        while off + fs::DIRENT_SIZE <= fs::BLOCK_SIZE && seen < entries {
            let (dname, dino) = fs::read_dirent(&data, off);
            seen += 1;
            if dino == ino && fs::name_eq(&dname, name) {
                fs::write_dirent(&mut data, off, b"", 0);
                write_block(blk, &data);
                return true;
            }
            off += fs::DIRENT_SIZE;
        }
    }
    false
}

/// Whether directory `ino` has no live entries (this FS has no `.`/`..`, so an
/// empty directory has zero non-zero dirents).
fn dir_is_empty(ino: u32) -> bool {
    let dir = read_inode(ino);
    let entries = dir.size as usize / fs::DIRENT_SIZE;
    let mut seen = 0usize;
    for &blk in dir.direct.iter() {
        if blk == 0 {
            continue;
        }
        let data = read_block(blk);
        let mut off = 0;
        while off + fs::DIRENT_SIZE <= fs::BLOCK_SIZE && seen < entries {
            let (_, dino) = fs::read_dirent(&data, off);
            seen += 1;
            if dino != 0 {
                return false;
            }
            off += fs::DIRENT_SIZE;
        }
    }
    true
}

/// Allocate a fresh inode of `kind`, returning its number. (nlink = 1, size 0,
/// no blocks.) None if the inode table is full.
fn alloc_node(kind: u16) -> Option<u32> {
    let ino = alloc_inode()?;
    let mut node = fs::Inode::empty();
    node.kind = kind;
    node.nlink = 1;
    write_inode(ino, &node);
    Some(ino)
}

/// Create an empty file `name` in the root directory; returns its inode.
pub fn create(name: &[u8]) -> Option<u32> {
    if lookup(name).is_some() {
        return None;
    }
    let ino = alloc_node(fs::T_FILE)?;
    if !dir_link(fs::ROOT_INODE, name, ino) {
        write_inode(ino, &fs::Inode::empty()); // release on out-of-space
        return None;
    }
    Some(ino)
}

/// Create directory `path` (S7). The parent must exist and be a directory; the
/// final component must not already exist. Returns false on any of those, on a
/// full inode table, or out-of-space.
pub fn mkdir(path: &[u8]) -> bool {
    let Some((pino, name)) = split_parent(path) else {
        return false;
    };
    if name.is_empty() || dir_lookup(pino, name).is_some() {
        return false;
    }
    let Some(ino) = alloc_node(fs::T_DIR) else {
        return false;
    };
    if !dir_link(pino, name, ino) {
        write_inode(ino, &fs::Inode::empty()); // release
        return false;
    }
    true
}

/// Remove directory `path` (S7) — must be an existing, empty directory (and not
/// the root). Frees its blocks + inode and clears its dirent in the parent.
pub fn rmdir(path: &[u8]) -> bool {
    let Some(ino) = namei(path) else {
        return false;
    };
    if ino == fs::ROOT_INODE || !is_dir(ino) || !dir_is_empty(ino) {
        return false;
    }
    let Some((pino, name)) = split_parent(path) else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    let node = read_inode(ino);
    free_all_blocks(&node);
    write_inode(ino, &fs::Inode::empty());
    clear_dirent(pino, ino, name)
}

/// Overwrite file `ino` with `data` (allocating blocks as needed).
pub fn write_file(ino: u32, data: &[u8]) -> bool {
    truncate(ino); // reset size; write_at reuses any blocks already linked
    write_at(ino, 0, data) == data.len()
}

/// Write `data` into file `ino` starting at byte `offset` (B9-3), allocating
/// data + indirect index blocks as needed (B9.5) and growing the file's size if
/// the write extends past the current end. Returns the number of bytes written
/// (short only on out-of-space or past the ~8 MiB max file). The random-access
/// write the `lseek`+`write` path and the toolchains (which seek around their
/// output) need.
pub fn write_at(ino: u32, offset: usize, data: &[u8]) -> usize {
    let mut node = read_inode(ino);
    let mut done = 0;
    while done < data.len() {
        let pos = offset + done;
        let bi = pos / fs::BLOCK_SIZE;
        let Some(blk) = block_for(&mut node, bi, true) else {
            break; // out of space, or past the max file size
        };
        let within = pos % fs::BLOCK_SIZE;
        let mut block = read_block(blk);
        let n = (data.len() - done).min(fs::BLOCK_SIZE - within);
        block[within..within + n].copy_from_slice(&data[done..done + n]);
        write_block(blk, &block);
        done += n;
    }
    let new_end = offset + done;
    if new_end > node.size as usize {
        node.size = new_end as u32;
    }
    write_inode(ino, &node); // persists size + any newly-allocated indirect ptrs
    done
}

/// The size in bytes of file `ino` (B9-3) — backs `stat`/`fstat`.
pub fn size_of(ino: u32) -> usize {
    read_inode(ino).size as usize
}

/// Truncate file `ino` to zero length (B9-3, the open-for-write case). Leaves
/// the data blocks linked in the inode so a subsequent `write_at` reuses them;
/// only the size is reset, so reads see an empty file.
pub fn truncate(ino: u32) {
    let mut node = read_inode(ino);
    node.size = 0;
    write_inode(ino, &node);
}

/// Free a single-indirect block: all the data blocks it points at, then itself.
fn free_indirect(iblk: u32) {
    let blk = read_block(iblk);
    for slot in 0..fs::PTRS_PER_BLOCK {
        let off = slot * 4;
        let d = u32::from_le_bytes([blk[off], blk[off + 1], blk[off + 2], blk[off + 3]]);
        if d != 0 {
            free_block(d);
        }
    }
    free_block(iblk);
}

/// Free every block a file owns: direct, single-indirect (+ its index block),
/// and double-indirect (+ both index levels). (B9.5)
fn free_all_blocks(node: &fs::Inode) {
    for &b in node.direct.iter() {
        if b != 0 {
            free_block(b);
        }
    }
    if node.indirect != 0 {
        free_indirect(node.indirect);
    }
    if node.double_indirect != 0 {
        let blk = read_block(node.double_indirect);
        for slot in 0..fs::PTRS_PER_BLOCK {
            let off = slot * 4;
            let mid = u32::from_le_bytes([blk[off], blk[off + 1], blk[off + 2], blk[off + 3]]);
            if mid != 0 {
                free_indirect(mid);
            }
        }
        free_block(node.double_indirect);
    }
}

/// Delete `name`: free its blocks + inode and clear its dirent.
pub fn delete(name: &[u8]) -> bool {
    let Some(ino) = lookup(name) else {
        return false;
    };
    let node = read_inode(ino);
    free_all_blocks(&node);
    write_inode(ino, &fs::Inode::empty()); // mark $free

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

/// Delete the file at absolute `path` (B11-3-followup). Generalizes `delete`
/// (which is root-only): resolves the parent directory by path, frees the
/// file's inode + data blocks, and clears its dirent in the parent. Returns
/// false if the path doesn't resolve to a regular file. Directories are not
/// removed (only `T_FILE`). The syscall layer (`unlink`) calls this.
pub fn unlink(path: &[u8]) -> bool {
    let Some((parent_ino, name)) = split_parent(path) else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    let Some(ino) = dir_lookup(parent_ino, name) else {
        return false;
    };
    let node = read_inode(ino);
    if node.kind != fs::T_FILE {
        return false; // don't unlink directories (use rmdir)
    }
    free_all_blocks(&node);
    write_inode(ino, &fs::Inode::empty()); // mark $free
    clear_dirent(parent_ino, ino, name)
}

// --- mount -----------------------------------------------------------------

/// Mount the FS by driving the `Mount` HSM ($Unmounted → $Mounting → $Mounted)
/// past a superblock check. Returns whether the mount succeeded. Shared by the
/// B4 demo and the interactive build (which mounts without running the demo).
pub fn mount() -> bool {
    let mut mount = crate::frame_systems::Mount::__create();
    mount.begin_mount(); // $Unmounted → $Mounting
    if check_superblock() {
        mount.mounted_ok(); // → $Mounted
    } else {
        mount.mount_failed();
        return false;
    }
    mount.is_mounted()
}

// --- B4 Step 2 demo --------------------------------------------------------

/// Mount the FS (via the `Mount` HSM), read a pre-populated file, then run a
/// create → write → read → delete round-trip.
pub fn run_demo() {
    if !mount() {
        serial::writeln("[fs] mount failed: bad superblock");
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

    // B9.5: a large-file round-trip that spans the double-indirect tier. 128 KiB
    // = 256 blocks, past 28 direct + 128 single-indirect (~78 KiB), so the tail
    // exercises double-indirect. Written + verified in 512-byte chunks (no big
    // kernel buffer); `delete` then frees all three tiers.
    {
        const BIG: usize = 128 * 1024;
        if let Some(bino) = create(b"big") {
            let mut ok = true;
            let mut chunk = [0u8; fs::BLOCK_SIZE];
            let mut pos = 0;
            while pos < BIG {
                for (i, b) in chunk.iter_mut().enumerate() {
                    *b = ((pos + i) as u8) ^ 0x3C;
                }
                if write_at(bino, pos, &chunk) != chunk.len() {
                    ok = false;
                    break;
                }
                pos += chunk.len();
            }
            let mut rpos = 0;
            while ok && rpos < BIG {
                let mut rb = [0u8; fs::BLOCK_SIZE];
                if read_at(bino, rpos, &mut rb) != rb.len() {
                    ok = false;
                    break;
                }
                for (i, &b) in rb.iter().enumerate() {
                    if b != (((rpos + i) as u8) ^ 0x3C) {
                        ok = false;
                        break;
                    }
                }
                rpos += rb.len();
            }
            if ok && size_of(bino) == BIG {
                serial::writeln("[fs] big file (128 KiB, double-indirect) round-trip: ok");
            } else {
                serial::writeln("[fs] big file round-trip FAILED");
            }
            delete(b"big");
        }
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
}

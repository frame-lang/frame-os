// kernel/src/vfs.rs
//
// The VFS layer (B4 Step 3): an open-file table over the filesystem. It
// resolves paths (fs::namei), tracks per-fd state (inode + byte offset), and
// drives an `OpenFile` Frame system per descriptor so the access mode is
// enforced — a read-only fd's writes are dropped, a closed fd is inert.
//
// With one filesystem the "dispatch" is trivial, but the fd-table + OpenFile
// structure is the seam a real VFS needs (multiple mounts/FS types at B5+).
// The on-disk mechanics live in fs.rs; this owns "which fds are open and how".

use crate::frame_systems::OpenFile;
use crate::{fs, serial};

const MAX_OPEN: usize = 16;

struct Slot {
    inode: u32,
    offset: usize,
    writable: bool,
    file: OpenFile,
}

static mut OPEN: [Option<Slot>; MAX_OPEN] = [const { None }; MAX_OPEN];

fn table() -> &'static mut [Option<Slot>; MAX_OPEN] {
    let p = &raw mut OPEN;
    unsafe { &mut *p }
}

/// Open the file at absolute `path` for reading. Returns a file descriptor, or
/// None if the path doesn't resolve to a regular file or the table is full.
pub fn open_read(path: &[u8]) -> Option<usize> {
    let ino = fs::namei(path)?;
    if !fs::is_file(ino) {
        return None;
    }
    for (fd, slot) in table().iter_mut().enumerate() {
        if slot.is_none() {
            let mut file = OpenFile::__create();
            file.open_read(); // $Open → $Reading
            *slot = Some(Slot {
                inode: ino,
                offset: 0,
                writable: false,
                file,
            });
            return Some(fd);
        }
    }
    None
}

/// Open the file at absolute `path` for writing (B9-3): truncate it if it exists,
/// otherwise create it (single root-level component, e.g. `/out.o`). Returns a
/// write fd, or None on a bad path / a full table. Drives `OpenFile` → $Writing,
/// so this fd's reads are gated off and a read-only fd's writes are dropped.
pub fn open_write(path: &[u8]) -> Option<usize> {
    let ino = match fs::namei(path) {
        Some(i) => {
            if !fs::is_file(i) {
                return None; // a directory
            }
            fs::truncate(i);
            i
        }
        None => {
            // Create at the root directory: the path must be "/<name>" (the FS
            // supports one directory level; nested create lands with B10+).
            if path.first() != Some(&b'/') {
                return None;
            }
            let name = &path[1..];
            if name.is_empty() || name.contains(&b'/') {
                return None;
            }
            fs::create(name)?
        }
    };
    for (fd, slot) in table().iter_mut().enumerate() {
        if slot.is_none() {
            let mut file = OpenFile::__create();
            file.open_write(); // $Open → $Writing
            *slot = Some(Slot {
                inode: ino,
                offset: 0,
                writable: true,
                file,
            });
            return Some(fd);
        }
    }
    None
}

/// Write `buf` to `fd` at its current offset, advancing it (B9-3). Returns the
/// number of bytes written (0 if the fd isn't open for writing — the OpenFile
/// access-mode gate drops a non-$Writing write).
pub fn write(fd: usize, buf: &[u8]) -> usize {
    let Some(slot) = table().get_mut(fd).and_then(|s| s.as_mut()) else {
        return 0;
    };
    slot.file.write(); // gated: a no-op unless the fd is $Writing
    if !slot.file.is_writing() {
        return 0;
    }
    let n = fs::write_at(slot.inode, slot.offset, buf);
    slot.offset += n;
    n
}

/// Reposition `fd`'s offset (B9-3). `whence`: 0 = SET (absolute), 1 = CUR
/// (relative), 2 = END (from file size). Returns the new offset, or None for a
/// bad fd / whence, or a resulting negative offset.
pub fn seek(fd: usize, offset: i64, whence: u32) -> Option<usize> {
    let slot = table().get_mut(fd).and_then(|s| s.as_mut())?;
    let base = match whence {
        0 => 0i64,
        1 => slot.offset as i64,
        2 => fs::size_of(slot.inode) as i64,
        _ => return None,
    };
    let new = base.checked_add(offset)?;
    if new < 0 {
        return None;
    }
    slot.offset = new as usize;
    Some(slot.offset)
}

/// The size in bytes of the file behind `fd` (B9-3) — backs `fstat`.
pub fn fstat_size(fd: usize) -> Option<usize> {
    let slot = table().get_mut(fd).and_then(|s| s.as_mut())?;
    Some(fs::size_of(slot.inode))
}

/// Duplicate `fd` onto the lowest free descriptor (B9-3), sharing the same inode
/// + offset + access mode. Returns the new fd, or None for a bad fd / full table.
pub fn dup(fd: usize) -> Option<usize> {
    let (inode, offset, writable) = {
        let slot = table().get_mut(fd).and_then(|s| s.as_mut())?;
        (slot.inode, slot.offset, slot.writable)
    };
    for (nfd, slot) in table().iter_mut().enumerate() {
        if slot.is_none() {
            let mut file = OpenFile::__create();
            if writable {
                file.open_write();
            } else {
                file.open_read();
            }
            *slot = Some(Slot {
                inode,
                offset,
                writable,
                file,
            });
            return Some(nfd);
        }
    }
    None
}

/// Read up to `buf.len()` bytes from `fd`, advancing its offset. Returns the
/// number of bytes read (0 at EOF, or if the fd isn't open for reading).
pub fn read(fd: usize, buf: &mut [u8]) -> usize {
    let Some(slot) = table().get_mut(fd).and_then(|s| s.as_mut()) else {
        return 0;
    };
    slot.file.read(); // gated: a no-op unless the fd is $Reading
    if !slot.file.is_reading() {
        return 0;
    }
    let n = fs::read_at(slot.inode, slot.offset, buf);
    slot.offset += n;
    n
}

/// Close `fd` (OpenFile → $Closed) and free its table slot.
pub fn close(fd: usize) {
    if let Some(slot) = table().get_mut(fd).and_then(|s| s.as_mut()) {
        slot.file.close();
    }
    if let Some(s) = table().get_mut(fd) {
        *s = None;
    }
}

/// Whether `fd` refers to an open file.
pub fn is_open(fd: usize) -> bool {
    match table().get_mut(fd).and_then(|s| s.as_mut()) {
        Some(slot) => slot.file.is_open(),
        None => false,
    }
}

/// B4 Step 3 demo: open files by path through the VFS (including a nested
/// directory), read them, and show that a closed fd is inert.
pub fn run_demo() {
    // A top-level file.
    if let Some(fd) = open_read(b"/motd") {
        let mut buf = [0u8; 64];
        let n = read(fd, &mut buf);
        serial::write_str("[vfs] read /motd via fd: ");
        for &c in &buf[..n] {
            serial::write_byte(c);
        }
        close(fd);
        if !is_open(fd) {
            serial::writeln("[vfs] /motd closed");
        }
    } else {
        serial::writeln("[vfs] open /motd failed");
    }

    // A file in a nested directory — proves path walking through /bin.
    match open_read(b"/bin/info") {
        Some(fd) => {
            let mut buf = [0u8; 64];
            let n = read(fd, &mut buf);
            serial::write_str("[vfs] read /bin/info via fd: ");
            for &c in &buf[..n] {
                serial::write_byte(c);
            }
            // A closed fd reads nothing.
            close(fd);
            if read(fd, &mut buf) == 0 {
                serial::writeln("[vfs] read after close returns 0: ok");
            }
        }
        None => serial::writeln("[vfs] open /bin/info failed"),
    }
}

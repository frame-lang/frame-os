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
                file,
            });
            return Some(fd);
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

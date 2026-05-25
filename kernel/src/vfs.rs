// kernel/src/vfs.rs
//
// The VFS layer (B4 Step 3): a *per-process* open-file table over the
// filesystem. It resolves paths (fs::namei), tracks per-fd state (inode + byte
// offset), and drives an `OpenFile` Frame system per file descriptor so the
// access mode is enforced — a read-only fd's writes are dropped, a closed fd is
// inert.
//
// S5 (I/O redirection) made the table per-process and gave it real stdio:
//   - There is one `[Option<Slot>; MAX_OPEN]` table per scheduler slot, indexed
//     by `sched::current_slot()`. `fork` copies the parent's table
//     (`clone_fds`), `exec` keeps it (same slot), a fresh process gets the
//     standard console descriptors (`init_console_fds`), and a reaped slot is
//     cleared (`clear_fds`).
//   - fd 0/1/2 are console descriptors by convention (`Slot::ConsoleIn` /
//     `Slot::ConsoleOut`); `write` to a ConsoleOut emits to the serial console,
//     so a program's `write(1, …)` reaches the screen. `open` allocates the
//     lowest *free* fd, which is ≥ 3 once the console fds are installed.
//   - `dup2(old, new)` repoints `new` at `old`, which is how the shell wires
//     redirection: in the forked child it opens the target file, `dup2`s it onto
//     fd 1 (`>`/`>>`) or fd 0 (`<`), then execs. exec preserves the table, so
//     the program inherits the redirected descriptor transparently.
//
// The on-disk mechanics live in fs.rs; this owns "which fds are open and how".

use crate::frame_systems::OpenFile;
use crate::{fs, sched, serial};

// Per-process descriptor capacity. fd 0/1/2 are the console (S5), so this many
// minus 3 are available for files/pipes. Bumped to 32 when the console fds moved
// *into* the table: before S5 the console was a magic fd outside the table, so
// all 16 slots were files; reserving 3 would have cut tcc (which opens many
// files: source, headers, archives, output) below its old headroom.
const MAX_OPEN: usize = 32;
/// One fd table per scheduler slot. Mirrors `sched`'s `MAX_THREADS`.
const MAX_PROCS: usize = 8;

/// One open descriptor: a console stream or a regular file.
enum Slot {
    /// Console input (stdin, fd 0 by convention). `read` yields EOF — interactive
    /// input comes through the blocking `read_line` syscall (#9), not `read`
    /// (#6). A program reads real bytes from fd 0 only once it's been redirected
    /// to a `File` (e.g. `cmd < file`).
    ConsoleIn,
    /// Console output (stdout/stderr, fd 1/2 by convention). `write` emits the
    /// bytes to the serial console.
    ConsoleOut,
    /// A regular file opened through the FS, with its own byte offset + mode.
    File {
        inode: u32,
        offset: usize,
        writable: bool,
        file: OpenFile,
    },
    /// One end of an anonymous pipe (S6). `write_end` selects which end: the
    /// write end accepts `write`, the read end yields bytes via `read` (blocking
    /// in the syscall layer until data arrives or every writer closes). `id`
    /// indexes the kernel pipe pool (`crate::pipe`).
    Pipe { id: usize, write_end: bool },
}

static mut TABLES: [[Option<Slot>; MAX_OPEN]; MAX_PROCS] =
    [const { [const { None }; MAX_OPEN] }; MAX_PROCS];

/// The fd table for scheduler slot `slot`.
fn table_for(slot: usize) -> &'static mut [Option<Slot>; MAX_OPEN] {
    let p = &raw mut TABLES;
    unsafe { &mut (*p)[slot] }
}

/// The current process's fd table.
fn table() -> &'static mut [Option<Slot>; MAX_OPEN] {
    table_for(sched::current_slot())
}

/// Rebuild a fresh copy of `s` (used by `fork`/`dup`/`dup2`). A `File` gets a
/// new `OpenFile` driven to the same access mode, sharing inode + offset.
fn clone_slot(s: &Slot) -> Slot {
    match s {
        Slot::ConsoleIn => Slot::ConsoleIn,
        Slot::ConsoleOut => Slot::ConsoleOut,
        Slot::File {
            inode,
            offset,
            writable,
            ..
        } => {
            let mut file = OpenFile::__create();
            if *writable {
                file.open_write();
            } else {
                file.open_read();
            }
            Slot::File {
                inode: *inode,
                offset: *offset,
                writable: *writable,
                file,
            }
        }
        Slot::Pipe { id, write_end } => {
            // A new descriptor onto the same pipe end → bump that end's ref count.
            if *write_end {
                crate::pipe::inc_writer(*id);
            } else {
                crate::pipe::inc_reader(*id);
            }
            Slot::Pipe {
                id: *id,
                write_end: *write_end,
            }
        }
    }
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
            *slot = Some(Slot::File {
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

/// Open the file at absolute `path` for writing (B9-3). With `append` false this
/// truncates an existing file (or creates it); with `append` true it leaves the
/// contents and positions the offset at end-of-file (`>>` redirection). Creating
/// is limited to a single root-level component (e.g. `/out.o`). Returns a write
/// fd, or None on a bad path / a full table. Drives `OpenFile` → $Writing, so
/// this fd's reads are gated off.
pub fn open_write(path: &[u8], append: bool) -> Option<usize> {
    let ino = match fs::namei(path) {
        Some(i) => {
            if !fs::is_file(i) {
                return None; // a directory
            }
            if !append {
                fs::truncate(i);
            }
            i
        }
        None => {
            // Create at the root directory: the path must be "/<name>" (the FS
            // supports one directory level for create; nested create lands later).
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
    let offset = if append { fs::size_of(ino) } else { 0 };
    for (fd, slot) in table().iter_mut().enumerate() {
        if slot.is_none() {
            let mut file = OpenFile::__create();
            file.open_write(); // $Open → $Writing
            *slot = Some(Slot::File {
                inode: ino,
                offset,
                writable: true,
                file,
            });
            return Some(fd);
        }
    }
    None
}

/// Write `buf` to `fd` (B9-3). A `ConsoleOut` fd emits to the serial console; a
/// `File` fd writes at its current offset, advancing it. Returns the number of
/// bytes written (0 if the fd can't be written — `ConsoleIn`, a read-only file
/// whose `OpenFile` gate drops the write, or a closed fd).
pub fn write(fd: usize, buf: &[u8]) -> usize {
    match table().get_mut(fd).and_then(|s| s.as_mut()) {
        Some(Slot::ConsoleOut) => {
            // Syscalls run with IF=0 on a single core, so the whole buffer goes
            // out without a preemption point — a process's line can't be split
            // mid-way by a concurrent process (or a kernel print).
            for &b in buf {
                serial::write_byte(b);
            }
            buf.len()
        }
        Some(Slot::File {
            inode,
            offset,
            file,
            ..
        }) => {
            file.write(); // gated: a no-op unless the fd is $Writing
            if !file.is_writing() {
                return 0;
            }
            let n = fs::write_at(*inode, *offset, buf);
            *offset += n;
            n
        }
        Some(Slot::Pipe {
            id,
            write_end: true,
        }) => crate::pipe::write(*id, buf),
        _ => 0, // ConsoleIn, a pipe read end, or closed → can't write
    }
}

/// Reposition a `File` fd's offset (B9-3). `whence`: 0 = SET (absolute), 1 = CUR
/// (relative), 2 = END (from file size). Returns the new offset, or None for a
/// bad/console fd, a bad whence, or a resulting negative offset.
pub fn seek(fd: usize, offset: i64, whence: u32) -> Option<usize> {
    let s = table().get_mut(fd).and_then(|x| x.as_mut())?;
    if let Slot::File {
        inode, offset: off, ..
    } = s
    {
        let base = match whence {
            0 => 0i64,
            1 => *off as i64,
            2 => fs::size_of(*inode) as i64,
            _ => return None,
        };
        let new = base.checked_add(offset)?;
        if new < 0 {
            return None;
        }
        *off = new as usize;
        Some(*off)
    } else {
        None
    }
}

/// The size in bytes of the file behind `fd` (B9-3) — backs `fstat`. None for a
/// console or closed fd.
pub fn fstat_size(fd: usize) -> Option<usize> {
    match table().get_mut(fd).and_then(|s| s.as_mut())? {
        Slot::File { inode, .. } => Some(fs::size_of(*inode)),
        _ => None,
    }
}

/// Duplicate `fd` onto the lowest free descriptor (B9-3), sharing the same
/// backing (inode + offset + mode for a file). Returns the new fd, or None for a
/// bad fd / full table.
pub fn dup(fd: usize) -> Option<usize> {
    let cloned = clone_slot(table().get(fd).and_then(|s| s.as_ref())?);
    for (nfd, slot) in table().iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(cloned);
            return Some(nfd);
        }
    }
    None
}

/// Repoint `newfd` at `oldfd` (POSIX `dup2`). Closes `newfd` first if it's open,
/// then makes it a copy of `oldfd`'s backing. Returns `newfd`, or None for a bad
/// `oldfd` / out-of-range `newfd`. This is the primitive the shell uses to wire
/// redirection (`dup2(file, 1)` for `>`, `dup2(file, 0)` for `<`).
pub fn dup2(oldfd: usize, newfd: usize) -> Option<usize> {
    if newfd >= MAX_OPEN {
        return None;
    }
    if oldfd == newfd {
        // Still must be a valid open fd.
        return table().get(oldfd).and_then(|s| s.as_ref()).map(|_| newfd);
    }
    let cloned = clone_slot(table().get(oldfd).and_then(|s| s.as_ref())?);
    close(newfd);
    table()[newfd] = Some(cloned);
    Some(newfd)
}

/// Read up to `buf.len()` bytes from `fd`, advancing a file's offset. Returns the
/// number of bytes read (0 at EOF, on a console fd, or if the fd isn't open for
/// reading).
pub fn read(fd: usize, buf: &mut [u8]) -> usize {
    match table().get_mut(fd).and_then(|s| s.as_mut()) {
        Some(Slot::File {
            inode,
            offset,
            file,
            ..
        }) => {
            file.read(); // gated: a no-op unless the fd is $Reading
            if !file.is_reading() {
                return 0;
            }
            let n = fs::read_at(*inode, *offset, buf);
            *offset += n;
            n
        }
        Some(Slot::Pipe {
            id,
            write_end: false,
        }) => crate::pipe::read(*id, buf),
        _ => 0, // ConsoleIn (EOF here), ConsoleOut, a pipe write end, or closed
    }
}

/// Close `fd` and free its table slot: a `File`'s `OpenFile` goes to $Closed; a
/// `Pipe` end drops its ref count (freeing the pipe when both ends are gone).
pub fn close(fd: usize) {
    match table().get_mut(fd).and_then(|s| s.as_mut()) {
        Some(Slot::File { file, .. }) => file.close(),
        Some(Slot::Pipe { id, write_end }) => {
            if *write_end {
                crate::pipe::dec_writer(*id);
            } else {
                crate::pipe::dec_reader(*id);
            }
        }
        _ => {}
    }
    if let Some(s) = table().get_mut(fd) {
        *s = None;
    }
}

/// Whether `fd` refers to an open descriptor (file or console).
pub fn is_open(fd: usize) -> bool {
    table().get(fd).map(|s| s.is_some()).unwrap_or(false)
}

/// Create an anonymous pipe in the current process, returning (read_fd,
/// write_fd) installed at the two lowest free descriptors. None if the pool or
/// the fd table is exhausted (S6). The shell uses this to wire `cmd1 | cmd2`.
pub fn make_pipe() -> Option<(usize, usize)> {
    let id = crate::pipe::alloc()?;
    let t = table();
    let mut ends: [Option<usize>; 2] = [None, None];
    let mut k = 0usize;
    for (fd, s) in t.iter_mut().enumerate() {
        if s.is_none() {
            *s = Some(Slot::Pipe {
                id,
                write_end: k == 1, // ends[0] = read end, ends[1] = write end
            });
            ends[k] = Some(fd);
            k += 1;
            if k == 2 {
                break;
            }
        }
    }
    match (ends[0], ends[1]) {
        (Some(r), Some(w)) => Some((r, w)),
        _ => {
            // Not enough free fds: undo the partial install and free the pipe.
            if let Some(r) = ends[0] {
                t[r] = None;
            }
            crate::pipe::dec_reader(id);
            crate::pipe::dec_writer(id);
            None
        }
    }
}

/// Whether `fd` is the read end of a pipe (so `read` may need to block until
/// data arrives or every writer closes — handled by the syscall layer).
pub fn is_pipe_read(fd: usize) -> bool {
    matches!(
        table().get(fd).and_then(|s| s.as_ref()),
        Some(Slot::Pipe {
            write_end: false,
            ..
        })
    )
}

/// Whether the pipe behind read-end `fd` still has an open writer. False (→
/// end-of-file) once `fd` isn't a pipe read end or all writers have closed.
pub fn pipe_writers_open(fd: usize) -> bool {
    match table().get(fd).and_then(|s| s.as_ref()) {
        Some(Slot::Pipe {
            id,
            write_end: false,
        }) => crate::pipe::has_writers(*id),
        _ => false,
    }
}

// --- per-process lifecycle (driven by `sched`) -----------------------------

/// Close every descriptor in `t`, leaving it empty.
fn clear_table(t: &mut [Option<Slot>; MAX_OPEN]) {
    for s in t.iter_mut() {
        match s {
            Some(Slot::File { file, .. }) => file.close(),
            Some(Slot::Pipe { id, write_end }) => {
                if *write_end {
                    crate::pipe::dec_writer(*id);
                } else {
                    crate::pipe::dec_reader(*id);
                }
            }
            _ => {}
        }
        *s = None;
    }
}

/// Install the standard console descriptors into process `slot`'s table (fd 0 =
/// stdin, 1 = stdout, 2 = stderr), clearing any stale entries first. Called when
/// a fresh process is admitted (`sched::spawn_user`).
pub fn init_console_fds(slot: usize) {
    let t = table_for(slot);
    clear_table(t);
    t[0] = Some(Slot::ConsoleIn);
    t[1] = Some(Slot::ConsoleOut);
    t[2] = Some(Slot::ConsoleOut);
}

/// Copy process `src`'s fd table into `dst` (a `fork`ed child inherits every
/// descriptor). Clears `dst` first in case it holds a reaped process's leftovers.
pub fn clone_fds(dst: usize, src: usize) {
    clear_table(table_for(dst));
    for fd in 0..MAX_OPEN {
        let cloned = table_for(src)[fd].as_ref().map(clone_slot);
        table_for(dst)[fd] = cloned;
    }
}

/// Close every descriptor of process `slot` (called when its scheduler slot is
/// reaped/freed).
pub fn clear_fds(slot: usize) {
    clear_table(table_for(slot));
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

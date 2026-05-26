// kernel/src/pipe.rs
//
// Anonymous pipes (S6) — the in-kernel byte buffers behind the shell's `|`.
//
// A pipe is a bounded FIFO ring buffer with two ends. The *lifecycle* — how many
// readers/writers are open, and therefore whether a read on an empty pipe should
// block (a writer may still write) or take end-of-file (all writers closed) — is
// modeled by the `Pipe` Frame system (`frame/pipe.frs`): writer presence is the
// state, the reader/writer counts live in its domain. This module owns only the
// *mechanism*: the ring buffer and the byte copies, plus a small pool. That
// mirrors `OpenFile`, where the Frame FSM owns the access-mode lifecycle and the
// VFS owns the inode/offset.
//
// `fork` (vfs::clone_fds) fires `*_opened` on the FSM as it copies a pipe fd into
// the child; `close` / process reap fire `*_closed`. When the FSM reports
// `is_free()` (both ends gone) the slot's buffer is released. Blocking is handled
// in the syscall layer (usermode `do_pipe_read_loop`), which consults
// `has_writers` to choose block vs EOF and yields (sti+hlt) so the writer process
// gets the CPU — exactly like the `read_line` console path. Writes are
// non-blocking and bounded by the buffer; the shell's pipelines never fill 64 KiB.
//
// Single core, syscalls run with interrupts off, so no locking is needed.

use crate::frame_systems::Pipe;

const PIPE_CAP: usize = 64 * 1024;
const MAX_PIPES: usize = 8;

struct PipeBuf {
    buf: [u8; PIPE_CAP],
    head: usize, // next write index
    tail: usize, // next read index
    len: usize,  // bytes currently buffered
    /// Lifecycle FSM ($Writable/$Drained + reader/writer counts). `None` ⇒ the
    /// slot is free.
    fsm: Option<Pipe>,
}

impl PipeBuf {
    const fn empty() -> PipeBuf {
        PipeBuf {
            buf: [0; PIPE_CAP],
            head: 0,
            tail: 0,
            len: 0,
            fsm: None,
        }
    }
}

static mut POOL: [PipeBuf; MAX_PIPES] = [const { PipeBuf::empty() }; MAX_PIPES];

fn pool() -> &'static mut [PipeBuf; MAX_PIPES] {
    let p = &raw mut POOL;
    unsafe { &mut *p }
}

/// Allocate a fresh pipe, returning its id. Its FSM starts in `$Writable` with
/// one reader and one writer (the two descriptors the caller is about to
/// install). None if the pool is exhausted.
pub fn alloc() -> Option<usize> {
    for (id, p) in pool().iter_mut().enumerate() {
        if p.fsm.is_none() {
            p.head = 0;
            p.tail = 0;
            p.len = 0;
            p.fsm = Some(Pipe::__create());
            return Some(id);
        }
    }
    None
}

/// Copy up to `dst.len()` buffered bytes out of pipe `id`, advancing the read
/// cursor. Returns the number of bytes read (0 if empty).
pub fn read(id: usize, dst: &mut [u8]) -> usize {
    let p = &mut pool()[id];
    let n = dst.len().min(p.len);
    for b in dst.iter_mut().take(n) {
        *b = p.buf[p.tail];
        p.tail = (p.tail + 1) % PIPE_CAP;
    }
    p.len -= n;
    n
}

/// Copy up to `src.len()` bytes into pipe `id` (bounded by free space),
/// advancing the write cursor. Returns the number of bytes written.
pub fn write(id: usize, src: &[u8]) -> usize {
    let p = &mut pool()[id];
    let free = PIPE_CAP - p.len;
    let n = src.len().min(free);
    for &b in src.iter().take(n) {
        p.buf[p.head] = b;
        p.head = (p.head + 1) % PIPE_CAP;
    }
    p.len += n;
    n
}

/// Whether pipe `id` still has at least one open write end (FSM query). A reader
/// treats an empty pipe with no writers as end-of-file.
pub fn has_writers(id: usize) -> bool {
    pool()[id].fsm.as_mut().is_some_and(|f| f.has_writers())
}

pub fn inc_reader(id: usize) {
    if let Some(f) = pool()[id].fsm.as_mut() {
        f.reader_opened();
    }
}
pub fn inc_writer(id: usize) {
    if let Some(f) = pool()[id].fsm.as_mut() {
        f.writer_opened();
    }
}

/// Drop one read-end reference; free the pipe if both ends are now closed.
pub fn dec_reader(id: usize) {
    if let Some(f) = pool()[id].fsm.as_mut() {
        f.reader_closed();
    }
    free_if_idle(id);
}

/// Drop one write-end reference; free the pipe if both ends are now closed.
pub fn dec_writer(id: usize) {
    if let Some(f) = pool()[id].fsm.as_mut() {
        f.writer_closed();
    }
    free_if_idle(id);
}

fn free_if_idle(id: usize) {
    let p = &mut pool()[id];
    // `map_or(true, …)` rather than `is_none_or` (stable only since 1.82) to
    // honor the crate's declared MSRV (1.75).
    let free = p.fsm.as_mut().map_or(true, |f| f.is_free());
    if free {
        p.fsm = None;
        p.len = 0;
        p.head = 0;
        p.tail = 0;
    }
}

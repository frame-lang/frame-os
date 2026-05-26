// kernel/src/console.rs
//
// The interactive console's line discipline (B8). The serial RX interrupt
// (IRQ4) *posts* received bytes here via `feed`; the `read_line` syscall
// *drains* a completed line via `take_line`. This is the post/drain shape again:
// the ISR (a context that must stay short and can't block) only accumulates +
// echoes bytes, and the consumer (a process in a syscall) picks up whole lines.
//
// Line discipline (cooked mode, minimal):
//   - printable bytes are echoed and appended to the current line;
//   - backspace / DEL erases the last char (and rubs it out on screen);
//   - CR or LF terminates the line: it's echoed as a newline and the completed
//     line (without the terminator) becomes available to `take_line`.
//
// State is guarded by an IRQ-safe `SpinLock` because `feed` runs in interrupt
// context and `take_line` in syscall context on the same core.

use crate::serial;
use crate::spin::SpinLock;

const LINE_CAP: usize = 256;
const RING_CAP: usize = 1024;

struct Console {
    /// The line currently being edited (backspace acts here; not yet committed).
    cur: [u8; LINE_CAP],
    cur_len: usize,
    /// A FIFO of *committed* input bytes, including the `\n` terminators. A whole
    /// line (with its `\n`) is appended on Enter, so several lines can queue —
    /// batched/scripted input is preserved, not coalesced (the bug a single-line
    /// "last writer wins" buffer would have).
    ring: [u8; RING_CAP],
    head: usize,
    tail: usize,
    len: usize,
}

impl Console {
    const fn new() -> Self {
        Self {
            cur: [0; LINE_CAP],
            cur_len: 0,
            ring: [0; RING_CAP],
            head: 0,
            tail: 0,
            len: 0,
        }
    }
    /// Append one byte to the committed FIFO (dropped if full — rare; the shell
    /// drains far faster than a human or a test script types).
    fn ring_push(&mut self, b: u8) {
        if self.len == RING_CAP {
            return;
        }
        self.ring[self.tail] = b;
        self.tail = (self.tail + 1) % RING_CAP;
        self.len += 1;
    }
}

static CONSOLE: SpinLock<Console> = SpinLock::new(Console::new());

/// Feed one received byte through the line discipline (called by the RX ISR).
/// Echoes to the terminal and, on a terminator, commits the edited line (plus a
/// `\n`) to the FIFO for `take_line`. Pure native — never dispatches a Frame
/// system.
pub fn feed(b: u8) {
    let mut c = CONSOLE.lock();
    match b {
        b'\r' | b'\n' => {
            serial::write_byte(b'\r');
            serial::write_byte(b'\n');
            // Commit the edited line + its newline to the FIFO.
            let n = c.cur_len;
            for i in 0..n {
                let byte = c.cur[i];
                c.ring_push(byte);
            }
            c.ring_push(b'\n');
            c.cur_len = 0;
        }
        // Ctrl-C (0x03) / Ctrl-Z (0x1A): terminal job-control signals (S10 2c).
        // Send SIGINT / SIGTSTP to the current foreground process (the job the
        // shell is waiting on). No foreground (fg == 0, i.e. the shell is idle at
        // its prompt) ⇒ ignore. Echo the conventional ^C/^Z. The edit line is
        // dropped (matches a terminal interrupting the current input).
        0x03 | 0x1A => {
            let fg = crate::usermode::foreground_pid();
            let (sig, label): (u32, &str) = if b == 0x03 {
                (crate::usermode::SIGINT, "^C\r\n")
            } else {
                (crate::usermode::SIGTSTP, "^Z\r\n")
            };
            serial::write_str(label);
            c.cur_len = 0;
            if fg != 0 {
                crate::sched::send_signal(fg, sig);
            }
        }
        // Backspace / DEL: rub out the last char on screen and in the edit buffer.
        0x08 | 0x7F if c.cur_len > 0 => {
            c.cur_len -= 1;
            serial::write_byte(0x08);
            serial::write_byte(b' ');
            serial::write_byte(0x08);
        }
        // Printable ASCII (and tab) with room; ignore other control bytes / overflow.
        0x20..=0x7E | b'\t' if c.cur_len < LINE_CAP => {
            let i = c.cur_len;
            c.cur[i] = b;
            c.cur_len += 1;
            serial::write_byte(b);
        }
        _ => {}
    }
}

/// Take the next committed line (up to the next `\n`), if one is available, into
/// `dst` (without the `\n`). Returns the byte count, or `None` if no complete
/// line is queued yet. Called by the `read_line` syscall, which loops + waits on
/// `None`.
pub fn take_line(dst: &mut [u8]) -> Option<usize> {
    let mut c = CONSOLE.lock();
    // Is there a newline in the committed FIFO?
    let mut found = None;
    let mut i = 0;
    while i < c.len {
        if c.ring[(c.head + i) % RING_CAP] == b'\n' {
            found = Some(i);
            break;
        }
        i += 1;
    }
    let nl = found?; // no complete line yet

    let mut out = 0usize;
    for j in 0..nl {
        let byte = c.ring[(c.head + j) % RING_CAP];
        if out < dst.len() {
            dst[out] = byte;
            out += 1;
        }
    }
    // Consume the line bytes + the newline.
    c.head = (c.head + nl + 1) % RING_CAP;
    c.len -= nl + 1;
    Some(out)
}

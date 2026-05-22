// kernel/src/reactor.rs
//
// The post/drain (actor) primitive (R7b). The OS uses message-passing at every
// boundary where a producer can't safely dispatch a Frame system itself — an
// interrupt context (can't allocate against the spin-locked heap), or a different
// core (the instance is single-owner). The producer *posts* a Send message into a
// mailbox; the owning context *drains* it and dispatches. Until R7b that mailbox
// was hand-rolled three times, byte-for-byte identical, in three modules
// (`crosscore`, `ksched`, `pcsched`). This module is the one primitive they share.
//
// `Mailbox<T, CAP>` is a fixed-capacity FIFO of `T` (no allocation — the events
// live inline). It is *not* itself synchronized: callers wrap it in a
// `spin::SpinLock` when a producer and the drainer run in different contexts (the
// common case), exactly as the three call sites do. The element type is wrapped
// in `Option` so the buffer has a `const` initializer for *any* `T` — no `Copy`
// or `Default` bound, and `Mailbox::new()` is `const` so it can initialize a
// `static` array of per-core mailboxes.
//
// This is the native hand-rolling of framec's proposed deferred-send / `@@[cast]`
// mailbox (RFC-0038): a language-level cast would enqueue here, and a runtime
// drain hook would pop + dispatch.

/// A fixed-capacity FIFO mailbox of `T` (capacity `CAP`). Single-threaded by
/// itself; wrap in a `SpinLock` for cross-context use.
pub struct Mailbox<T, const CAP: usize> {
    buf: [Option<T>; CAP],
    head: usize,
    tail: usize,
    len: usize,
}

impl<T, const CAP: usize> Mailbox<T, CAP> {
    /// An empty mailbox. `const` so it can initialize a `static` (array).
    pub const fn new() -> Self {
        Self {
            buf: [const { None }; CAP],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    /// Enqueue `v`. Returns `false` (without enqueuing) if the mailbox is full.
    pub fn push(&mut self, v: T) -> bool {
        if self.len == CAP {
            return false;
        }
        self.buf[self.tail] = Some(v);
        self.tail = (self.tail + 1) % CAP;
        self.len += 1;
        true
    }

    /// Dequeue the oldest message, or `None` if empty.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let v = self.buf[self.head].take();
        self.head = (self.head + 1) % CAP;
        self.len -= 1;
        v
    }

    /// Number of queued messages.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the mailbox is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<T, const CAP: usize> Default for Mailbox<T, CAP> {
    fn default() -> Self {
        Self::new()
    }
}

// kernel/src/crosscore.rs
//
// Cross-core `post` (B7) — the Frame-relevant reckoning of SMP. The question the
// project has been building toward: can a Frame system be driven safely from a
// *different* core than the one that owns its instance, given framec's generated
// code is neither `Send` nor `Sync`?
//
// The answer here is **yes, without any framec change** — because the post/drain
// architecture already isolates the instance to one core:
//
//   - The Frame instance (`EventCounter`) is a *local* owned by the draining
//     core (the BSP) — see `run_drain_demo`. It is never moved to or touched by
//     another core, so its non-`Send`/non-`Sync`-ness is irrelevant.
//   - Other cores never see the instance. They only enqueue plain, `Copy`/`Send`
//     event *data* (`PostedEvent`) into a `SpinLock`-protected MPSC ring.
//   - The owning core drains the ring and dispatches each event to its local
//     instance — exactly the "ISRs only post, the owner drains" pattern from
//     B4/B5, now generalized from "ISR vs mainline on one core" to "producer
//     cores vs the owner core."
//
// So the long-flagged "framec needs `Send`/`Sync` codegen" gate is **sidestepped
// by the architecture**: only `Send` data crosses cores; the FSM stays put. The
// FSM still runs its real logic on the posted events (here: accumulate while
// `$Counting`, drop after `$Closed`) — so cross-core posts are gated by state
// just like local ones.

use crate::frame_systems::EventCounter;
use crate::serial;
use crate::spin::SpinLock;
use core::sync::atomic::{AtomicUsize, Ordering};

/// An event posted across cores. Plain `Copy` data — this is all that crosses a
/// core boundary; the `EventCounter` instance never does.
#[derive(Clone, Copy)]
pub enum PostedEvent {
    Tick(u32),
}

const QUEUE_CAP: usize = 1024;

/// A fixed-size MPSC ring: many producer cores `push`, the owner core `pop`s.
/// Wrapped in a `SpinLock` for cross-core mutual exclusion (no allocation on the
/// post path — bounded, interrupt-safe).
struct PostQueue {
    buf: [PostedEvent; QUEUE_CAP],
    head: usize,
    tail: usize,
    len: usize,
}

impl PostQueue {
    const fn new() -> Self {
        Self {
            buf: [PostedEvent::Tick(0); QUEUE_CAP],
            head: 0,
            tail: 0,
            len: 0,
        }
    }
    fn push(&mut self, e: PostedEvent) -> bool {
        if self.len == QUEUE_CAP {
            return false; // full — caller applies backpressure
        }
        self.buf[self.tail] = e;
        self.tail = (self.tail + 1) % QUEUE_CAP;
        self.len += 1;
        true
    }
    fn pop(&mut self) -> Option<PostedEvent> {
        if self.len == 0 {
            return None;
        }
        let e = self.buf[self.head];
        self.head = (self.head + 1) % QUEUE_CAP;
        self.len -= 1;
        Some(e)
    }
}

static POST_QUEUE: SpinLock<PostQueue> = SpinLock::new(PostQueue::new());

/// How many producer cores (APs) have finished their post phase.
pub static AP_POSTED: AtomicUsize = AtomicUsize::new(0);

/// Posts each core contributes (BSP + every AP). Total = cores × this.
const POSTS_PER_CORE: u32 = 200;

/// Post a `Tick(n)` from any core (the cross-core producer side). Spins if the
/// ring is momentarily full (the owner core drains concurrently).
fn post_tick(n: u32) {
    loop {
        if POST_QUEUE.lock().push(PostedEvent::Tick(n)) {
            return;
        }
        core::hint::spin_loop();
    }
}

/// An application processor's post phase: contribute `POSTS_PER_CORE` ticks, then
/// signal done. Called from `ap_entry` (on the AP).
pub fn ap_post_phase() {
    for _ in 0..POSTS_PER_CORE {
        post_tick(1);
    }
    AP_POSTED.fetch_add(1, Ordering::SeqCst);
}

/// Pop everything currently queued and dispatch it to the (BSP-local) counter.
/// The queue lock is released before each dispatch — it's a leaf lock, never
/// held across the Frame dispatch.
fn drain_into(counter: &mut EventCounter) {
    while let Some(ev) = POST_QUEUE.lock().pop() {
        match ev {
            PostedEvent::Tick(n) => counter.tick(n),
        }
    }
}

/// The owner-core (BSP) side: drive an `EventCounter` from cross-core posts.
/// `ap_count` is the number of producer APs. The instance is a **local** here —
/// pinned to this core, never shared — which is why framec's non-`Send` codegen
/// is fine.
pub fn run_drain_demo(ap_count: usize) {
    let mut counter = EventCounter::__create();

    // The BSP contributes its own share too (so all `ap_count + 1` cores post).
    for _ in 0..POSTS_PER_CORE {
        post_tick(1);
    }

    // Drain concurrently with the APs, then once they've all finished posting do
    // a final pass to catch anything that raced in.
    let mut spins = 0u64;
    loop {
        drain_into(&mut counter);
        if AP_POSTED.load(Ordering::SeqCst) >= ap_count {
            drain_into(&mut counter); // final pass: APs increment AP_POSTED only
                                      // after all their posts are enqueued
            break;
        }
        spins += 1;
        if spins > 1_000_000_000 {
            break; // bounded — never hang
        }
        core::hint::spin_loop();
    }

    let total = counter.count();
    let expected = (ap_count as u32 + 1) * POSTS_PER_CORE;
    serial::write_str("[smp] cross-core post: counter ");
    serial::write_u32_decimal(total);
    serial::write_str(" (expected ");
    serial::write_u32_decimal(expected);
    serial::writeln(")");
    if total == expected {
        serial::writeln("[smp] cross-core post -> Frame dispatch: ok");
    } else {
        serial::writeln("[smp] cross-core post: FAILED");
    }

    // State gating: close the counter, then a further tick must be dropped
    // ($Closed has no `tick` handler) — proving the FSM gates cross-core posts
    // by state just like local ones.
    counter.close();
    counter.tick(999);
    if counter.count() == expected {
        serial::writeln("[smp] post-close tick ignored ($Closed gates it): ok");
    }
}

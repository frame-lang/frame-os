# The post/drain contract (observation note)

> **Status:** Observation note, not an RFC. This was originally drafted as a
> full cross-environment "Port" specification; on review that was premature
> (it abstracted from zero implementations and coupled FOS to SOS for ~100
> lines of shared logic that don't exist yet). It has been demoted to the
> one thing that's actually true and load-bearing today. The mechanism gets
> *built* where a concrete environment needs it — FOS at B1 (preemption
> makes the kernel interrupt-driven), SOS if/when SOS is built. A shared
> abstraction, if it's worth extracting, gets extracted *after* two real
> implementations exist, not before.

## The property

A Frame `@@system` instance dispatches **one event at a time, run to
completion**, over `&mut self`. It is single-threaded and non-reentrant by
construction (the `_context_stack`, the transition-drain loop, the
in-flight bookkeeping all assume no second event arrives mid-flight).

**Consequence:** any environment that delivers events to a Frame system
from *concurrent or interrupt sources* must put a queue in front and
separate **delivery** from **dispatch**. You cannot call an interface
method directly from an interrupt handler or another core/thread while the
system might be in flight — that's a reentrancy bug.

## The contract (when you build the queue)

Whatever the substrate (a static ISR-safe ring in the kernel; a heap
concurrent queue in a server), a correct event queue in front of a Frame
system obeys four rules:

1. **Serial dispatch** — at most one event per instance is in flight.
2. **Run-to-completion** — once an event begins, it finishes (including the
   `$>`/`<$` lifecycle handlers its transitions synthesize) before the next.
3. **`post` is decoupled** — delivering an event only *enqueues*; it never
   runs the handler inline, it's non-blocking, and it's safe to call from
   any producer context (ISR, other thread, other core). This is the rule
   that actually kills the reentrancy bug.
4. **`@@defer`** (if/when added) — "this state can't handle this now": the
   event is requeued and retried after the next state transition, in
   arrival order. Liveness caveat: a queue holding *only* deferred events
   never retries until some other event drives a transition — an
   application-design hazard, not a runtime bug.

The drain step (who pulls from the queue and calls the system's dispatch
entry — `__kernel`) is environment-specific and not part of this note: in
the kernel it's the main loop; in a server it's an executor.

## Where this lands

- **FOS, B1:** the timer ISR `post`s a tick/preempt event to the
  `Scheduler`'s queue; the kernel main loop drains it. This is the first
  real, non-speculative need for the queue. Build it from that requirement
  (interrupt-safe, no heap), and let its actual shape be the reference.
- **SOS (advisory):** the SOS comms design is a richer realization of the
  same property — a per-identity mailbox with passivation, supervision, and
  brokers layered on. Useful as concept harvest; built when SOS is built.
- **Shared code:** consider extracting only after both a kernel queue and a
  server mailbox exist and the duplication is visible. Not before.

## What was cut from the earlier draft (and why)

The RFC-grade version had named-invariant ceremony, trait/code sketches,
SOS/FOS conformance sections, a lineage table, and an open-questions list —
all for a mechanism that's ~100 lines and has no implementation yet. That's
abstraction-first, which inverts build-then-extract. If a real shared
`frame_runtime` queue is warranted later, it earns a proper RFC then, with
two working implementations to generalize from.

# Pipelined / multi-request virtio-blk — design note (lifting the single-flight ceiling)

**Status: PROPOSED (not started).** Written 2026-05-26. The disk path is
deliberately *single-flight* today (S6/`s6_io_scheduler_handoff.md`): one
outstanding request, one shared scratch buffer, serialized by the `IoScheduler`
FSM. This note records *how* we'd move to N concurrent in-flight requests, the
Frame angle (why it's the most thesis-relevant storage investment), the caveats,
and a recommendation.

## Why (and why not)

**Win:** processes blocked on disk I/O no longer serialize the whole engine. The
concrete beneficiary is **concurrent `exec`** — e.g. a pipeline (`a | b`) forks
two children that both `exec` (read their ELFs) at once; today the second blocks
on the first's whole transaction. Multi-flight overlaps them. Any future
I/O-concurrent workload benefits too.

**Not a #110 fix.** The residual `mv` flake is a QEMU/TCG-on-arm64 **stale-read**
artifact (writes verified correct, a later read returns stale — see
`frame_assessment.md` 2026-05-26). Multi-flight uses the *same* QEMU read path
and would likely **expose it more** (more concurrent reads). Validate knowing the
flake exists.

**Cost/risk:** a rewrite of the critical disk path that `fs` + `exec` depend on.
High regression risk; modest practical payoff on a single-core, not-I/O-bound OS.
This is an *excellence/thesis* investment, not a need.

## The mechanism

### Request-slot pool (native)
Replace the single shared scratch + fixed descriptors 0/1/2 with a pool of `N`
slots (start `N = 8`). Each slot owns:
- **A DMA buffer region**: header (16B) + status (1B) + data (512B). Simplest:
  one 4 KiB frame per slot (8 frames). (Packing ~7 slots/frame is possible but
  not worth the alignment fuss.)
- **A fixed descriptor triple**: slot `i` → descriptors `[3i, 3i+1, 3i+2]`, chain
  head `3i`. QEMU's qsize (128/256) ≫ `3N`, so static assignment is fine and the
  reverse map is trivial: **used-ring `id` → slot = `id / 3`**.
- **State**: `free | in-flight`, plus the waiting pid, sector, R/W, and the
  completion status/len once drained.

### Submit (per request)
1. Acquire a free slot (block if the pool is full — see IoScheduler below).
2. Fill the slot's header (`BLK_T_IN/OUT`, sector), status = `0xFF` sentinel;
   for a write, copy data into the slot's data buffer.
3. Program the slot's 3 descriptors (header R → data → status W) at the slot's
   buffers.
4. Publish the chain head (`3i`) in the avail ring at `avail_idx % qsize`, fence,
   bump `avail.idx`, fence, NOTIFY. (Same publish discipline as today.)
5. Record the waiter (current pid) in the slot; `block_current_until(slot.done)`.

### Completion — the part single-flight skips: read the used-ring ELEMENT
Single-flight only watches `used.idx` advance (it knows the one completion is
"the" request). Multi-flight MUST consume the used-ring *elements* to know WHICH
request finished. A native `drain_used()`:
- Read `used.idx` (fenced). For each new entry from `last_used` to `used.idx`:
  read `ring[idx % qsize] = { id, len }`; `slot = id / 3`; read that slot's
  status byte; mark `slot.done` (store status/len); `wake_pid(slot.waiter)`;
  advance `last_used`.
- Called from **`on_irq`** (native, short, no Frame — OK in the ISR) AND from the
  busy-wait poll (boot, pre-scheduler). A waiter's block predicate is just its
  **own** `slot.done` (set by the drain); on any completion IRQ the drain marks
  every finished slot + wakes its waiter, and each waiter rechecks its own slot.
- The `on_irq` drain stays native; the **`BlockRequest` FSM transition** to
  `$Complete/$Error` happens in the woken waiter's *syscall* context (where Frame
  dispatch is legal) — same post/drain split as today.

## The Frame angle (why this is the thesis-relevant choice)

Both Frame systems get *richer*, in the recurring **"supervisor + N instances"**
pattern (cf. `ProcessTable` + `Process`):

- **`IoScheduler`** changes role: from *single-engine ownership* (one owner + a
  wait queue) to a **slot-pool supervisor** — `$HasFreeSlots` (hand out a slot) ↔
  `$Full` (queue requesters); a completion frees a slot and admits a queued
  requester. Mirrors `ProcessTable`'s `$HasCapacity`/`$Full`.
- **`BlockRequest`**: today one lifecycle instance; multi-flight runs **N
  concurrent instances**, one per in-flight slot, each `$Queued → $InFlight →
  $Complete/$Error`, driven independently by the drain (matched via the used-ring
  `id`). Genuinely concurrent lifecycles — real stateful coordination, not the
  degenerate single case.

That is why multi-flight (not the modern-transport rewrite) is the storage work
that actually advances the Frame thesis: the transport is native plumbing, but
the *concurrency coordination* is exactly what Frame is for.

## Sequencing (incremental, each validated)
1. **Slot pool + per-request buffers**, but keep submit *serialized* (one at a
   time) — pure refactor, no behavior change; validate parity with single-flight.
   **DONE 2026-05-26** (`kernel/src/virtio_blk.rs`): N=8 slots, each its own 4 KiB
   DMA frame + fixed descriptor triple `[3i,3i+1,3i+2]`; `acquire_slot`/`release_slot`
   pool; `submit(slot,..)` / `wait_and_drain(slot)` / `read_sector` / `write_sector`
   route through a slot. Completion still `used.idx`-only (one in flight), submit
   still serialized by `IoScheduler`. Parity validated: `blk_roundtrip_b4` PASS;
   clippy + fmt clean both kernel configs.
2. **Used-ring-element drain** (`id → slot`, multi-completion) replacing the
   `used.idx`-only completion; still serialized; validate.
   **DONE 2026-05-26**: `drain_used()` consumes used-ring elements (fenced
   `used.idx` read; `id/3 → slot`; records `slot_status`, sets `slot_done`,
   advances `last_used`). `wait_and_drain(slot)`'s predicate is now
   `{ drain_used(); slot_done[slot] }` — per-request, not the global
   `used.idx`-advanced test. Drain currently runs in the wait predicate (Step 3
   moves it into `on_irq` to wake concurrent waiters by id). Parity validated:
   `blk_roundtrip_b4` PASS; clippy + fmt clean both configs.
3. **Concurrent submit** — allow a second request before the first completes;
   the drain wakes each by id. Validate with concurrent `exec` (a pipeline).
4. **`IoScheduler` → slot-pool supervisor + N `BlockRequest` instances** (the
   Frame redesign). Validate; update diagrams + `frame_assessment.md`.

## Caveats
- BSP-only ring-3 today, so "concurrent" = one process blocked on I/O while
  another runs and also issues I/O (e.g. concurrent `exec`). Real, but bounded.
- The fs buffer cache is unlocked (safe only because fs is BSP-only — see
  `fs.rs`); multi-flight doesn't change that, but if user scheduling ever goes
  multi-core, both the cache and the slot pool need locks.
- Do **not** re-kick a single-flight legacy queue (the documented double-
  completion trap); multi-flight's per-id completion is the spec-correct way to
  have multiple avail entries outstanding.

## Recommendation
Worth doing **as a thesis/excellence step when storage gets attention** — it's
the most Frame-relevant disk work and a real capability. **Not** worth doing
purely to "fix" anything (it fixes nothing #110-related) or ahead of roadmap
work, given the critical-path regression risk vs. modest single-core payoff.
Sequence it incrementally behind validation if pursued.

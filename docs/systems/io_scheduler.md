# `IoScheduler`

> The **slot-pool admission supervisor** over the virtio-blk request slots (S6 → multi-flight): `$HasFreeSlots ⇄ $Full`, with a free-slot pool + a FIFO wait queue in the domain. `acquire(pid)` hands out a free slot index (or queues the caller and returns −1 when all N are busy); `release(pid)` frees the slot and hands it to the next queued waiter (or back to the pool). A *coordinator* FSM, not a per-instance lifecycle — the "manager + N instances" pattern (cf. `ProcessTable`), here admission control over the disk's N concurrent request slots.

| Property | Value |
|---|---|
| Track | Bare-metal |
| Milestone introduced | S6 (forced by concurrent `exec` in a pipeline) |
| Source file | [`../../frame/io_scheduler.frs`](../../frame/io_scheduler.frs) |
| State diagram | [`io_scheduler.svg`](io_scheduler.svg) |
| Instances at runtime | One (a shared supervisor, owned by `sched.rs`) |
| Status | Implemented — every disk transaction acquires/releases a slot; multi-flight (up to N=8 concurrent). |

## State diagram

![IoScheduler state graph](io_scheduler.svg)

## Why this is a Frame system (a new shape for this codebase)

The virtio-blk driver is **single-flight**: one shared scratch buffer + a single completion waiter. That holds when one process does disk I/O at a time (sequential shell commands), but **not** once two run concurrently — a pipeline forks two children that both `exec`, reading their ELFs off disk at once, and overlapping transactions clobber the shared buffer. The first attempt coordinated this with native flags (a `DISK_BUSY` bool + a hand-rolled waiter array), which produced lost-wakeup / clobber bugs — *the* class of bug scattered atomics are bad at.

The fix was to recognize that slot *admission* is genuinely **state-shaped**: `acquire` does different things in `$HasFreeSlots` (grant a slot) vs `$Full` (enqueue), and `release` does different things with an empty vs non-empty queue. That earns states. Every prior Frame win in this codebase was a per-instance *lifecycle* (`TcpConnection`, `OpenFile`); `IoScheduler` is the codebase's **coordinator** — one shared instance arbitrating a contended resource across processes, the same "manager + N instances" shape as `ProcessTable` over the process array. The native side owns only the *mechanism*: the per-slot DMA buffers, the "block until I hold a slot" / "block until my slot completed" waits (`sched::block_current_until`), and the per-slot `wake_pid`.

**Multi-flight (Step 3/4).** The driver gained a pool of N request slots (each its own DMA buffers + descriptor triple + per-slot completion), so up to N requests run concurrently. `IoScheduler` accordingly changed role from *single-engine owner* (`$Idle/$Busy`, one `owner`) to *slot-pool supervisor* (`$HasFreeSlots/$Full`, a free-slot set + `owners` map + wait queue) — it now hands out **which** slot each requester holds, rather than gating to one. This is the storage step that most advances the Frame thesis: the transport is native plumbing, but the concurrency coordination is exactly what Frame is for.

> Note the boundary, recorded in [`../frame_assessment.md`](../frame_assessment.md) (2026-05-25): the *coordination* belonged in Frame, but the disk **completion-detection** (poll `used.idx`, the spec-correct "all buffers written" signal) is a hardware-contract fact and stays native — and that completion bug, not the coordination, was the hard one.

## States

### `$HasFreeSlots` (initial)
At least one slot is free (invariant: the wait queue is empty here — a free slot is always granted immediately, never queued). `acquire(pid)` pops a free slot, records `pid → slot`, returns the slot index; if that took the last slot, transitions to `$Full`. `release(pid)` returns `pid`'s slot to the free pool.

### `$Full`
All N slots are busy. `acquire(pid)` pushes `pid` onto the wait queue and returns `−1` (the caller blocks until handed a slot). `release(pid)` frees `pid`'s slot via `do_release`: hand it to the next queued waiter → returns that pid to wake, stays `$Full`; or no waiter → the slot goes free, returns `0`, transitions to `$HasFreeSlots`. Overrides `is_full()` to `true`.

## Interface

| Method | Parameters | Returns | Purpose |
|---|---|---|---|
| `acquire` | `pid: u32` | `i32` | Grant a free slot index, or `−1` and enqueue `pid` when full. |
| `release` | `pid: u32` | `u32` | Free `pid`'s slot; return the admitted waiter's pid to wake, or `0`. |
| `slot_of` | `pid: u32` | `i32` | The slot `pid` holds (`≥ 0`), or `−1` if queued/none — a waiter's block predicate. |
| `is_full` | (none) | `bool` | All slots busy? |

**Created with** `IoScheduler(slots: u32)` (the driver's N). **Domain:** `free: VecDeque<u32>` (seeded `0..slots`), `owners: BTreeMap<u32,u32>` (pid → slot), `wait_q: VecDeque<u32>`. **Actions:** `assign`/`do_release`/`slot_for`.

## Composition

**Driven by:** `crate::sched` — `acquire_disk()` calls `with_io_sched(|s| s.acquire(pid))`, and if full `block_current_until(|| s.slot_of(pid) >= 0)`, returning the granted slot index; `release_disk()` calls `s.release(pid)` and `wake_pid(next)`. `virtio_blk::{read_sector,write_sector}` bracket every transaction with these and run the transfer on the returned slot. A boot bypass (`!is_preemption_active() || pid == 0`) uses slot 0 directly during single-threaded early boot (the instance may not exist yet). The instance lives beside the `Scheduler` FSM in `sched.rs`, created `IoScheduler(virtio_blk::N_SLOTS)`, guarded by `without_interrupts` (non-reentrant, syscall/drained context only — never an ISR). Per-slot completion (`drain_used` → `slot_done`/`SLOT_WAITER[slot]`) is native, in `virtio_blk`.

## Testing

**Behavioral (Level 3):** [`kernel-tests/tests/io_scheduler_behavior.rs`](../../kernel-tests/tests/io_scheduler_behavior.rs) — 8 tests of the slot-pool logic in isolation: grants distinct slots, fills then queues overflow, FIFO admission of a queued waiter into the freed slot, `release` with no waiter frees capacity, non-holder release is a no-op. **State graph (Level 2):** `io_scheduler_state_graph` insta snapshot.

**QEMU (Level 7):** `concurrent_exec_buffers` — two children `exec` concurrently, now genuinely overlapping in flight (each gets its own slot + per-slot completion). `console-test`'s `echo pipe one two | wc` (S6) and the on-device `tcc` compile path exercise it under the interactive build (over the RAM backend; the supervisor still admits/sequences).

## Open questions
- **Per-sector, not per-fs-operation.** The lock brackets each sector transaction; a multi-block `fs::create` releases between block ops. Fine for the current single-writer workloads; concurrent *writers* to the FS would want a higher-level fs lock.
- **IRQ wakes stay native.** Disk *completion* (IRQ) and console RX can't drive Frame (non-reentrant in an ISR); they wake a cached pid via native `wake_pid`. The supervisor owns the syscall-context sequencing only.

## Related documents
- [`BlockRequest`](block_request.md) — the per-*request* lifecycle (this serializes *access* to the engine those requests run on).
- [`Pipe`](pipe.md) — the S6 sibling; concurrent pipeline `exec` is what forced this supervisor.
- [`../frame_assessment.md`](../frame_assessment.md) — 2026-05-25 entries: the coordinator shape + why the completion fix was native, not Frame.

## Change log
- **2026-05-25** — initial doc; S6. `$Idle/$Busy` + waiter queue; the first coordinator/supervisor Frame system, replacing an ad-hoc native disk lock.
- **2026-05-27** — multi-flight (Steps 3/4). Redesigned single-engine owner → **slot-pool supervisor** (`$HasFreeSlots/$Full`): `acquire`/`release`/`slot_of`/`is_full` over a free-slot pool + `owners` map + wait queue, created with the driver's `N_SLOTS`. Up to N concurrent in-flight requests; native side gained per-slot waiters + an `on_irq` used-ring-element drain that wakes by slot id. Added 8 behavioral tests + a state-graph snapshot (the system had no host test before).

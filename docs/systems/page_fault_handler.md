# `PageFaultHandler`

> Classifies a CPU page fault and dispatches to the response, **running inside the `#PF` exception handler**: `$Classifying → $LazyFault | $Fatal` under an HSM parent `$FaultActive`. B2's Frame showcase — a state machine driving fault recovery from a hardware trap.

| Property | Value |
|---|---|
| Track | Bare-metal |
| Milestone introduced | B2 |
| Source file | [`../../frame/page_fault_handler.frs`](../../frame/page_fault_handler.frs) |
| State diagram | [`page_fault_handler.svg`](page_fault_handler.svg) |
| Instances at runtime | One global instance |
| Status | Implemented and load-bearing — the `#PF` handler drives it on every page fault. |

## State diagram

![PageFaultHandler state graph](page_fault_handler.svg)

## Why this is a clean Frame-in-kernel fit

Unlike the timer IRQ (asynchronous → needs the native ready-queue, no Frame from the ISR), a page fault is a **synchronous exception**: the `#PF` interrupt gate clears IF, so while handling a fault there is no preemption and no concurrent fault (a fault *during* fault handling is a double-fault, caught by the safety net). So the `#PF` stub can drive one global `PageFaultHandler` **synchronously — no lock, no queue, no reentrancy hazard**. This is where a Frame HSM fits the kernel directly, and it's a genuinely nice demonstration: the classification + dispatch logic of a fault handler *is* a state machine.

## States

### `$Classifying` (initial, child of `$FaultActive`)
Each `fault(addr, error_code)` lands here. The handler stashes `addr`, then classifies via the native `crate::vm::is_lazy_region(addr)`:
- in a registered demand-paged region → `-> $LazyFault`;
- otherwise → `-> $Fatal`.
**Forwarding:** `=> $^` (see parent).

### `$LazyFault` (child of `$FaultActive`)
**Enter (`$>`):** call `crate::vm::lazy_map(addr)` (allocate a frame + map the page). On success `-> $Classifying` (recovered; the faulting instruction will retry and succeed). On failure (out of frames) `-> $Fatal`.
**Forwarding:** `=> $^`.

### `$FaultActive` (HSM parent)
Empty at B2. At **B4** this hosts the shared "unrecoverable → kill the process" handler that the deep children (`$StackGrow`, `$CopyOnWrite`, `$LazyFault`) forward to via `=> $^`. At B2 the two children fully handle their cases, so the forwarding edge is **declared (matching the committed design) but not yet traversed** — the same "declared now, load-bearing later" pattern as `Task.$Blocked` (B1).

### `$Fatal`
**Enter (`$>`):** report the faulting address on serial. `is_fatal()` overrides to `true`; the native `#PF` handler reads it and halts (B2) — a clean fatal, not a silent triple-fault. (B3: kill the faulting process instead.)

## Interface

| Method | Parameters | Returns | Purpose |
|---|---|---|---|
| `fault` | `addr: u64, error_code: u64` | (none) | Classify + dispatch one page fault. |
| `is_fatal` | (none) | `bool` | `true` once classified `$Fatal`; the `#PF` handler halts on it. |
| `fault_addr` | (none) | `u64` | The most recent faulting address (domain read). |

## Domain

| Field | Type | Initial | Purpose | Lifetime |
|---|---|---|---|---|
| `fault_addr` | `u64` | `0` | The address of the fault being handled. | System lifetime |

## Composition

**Driven by:** `crate::vm::page_fault_handler(addr, ec)` — the Rust half of the `#PF` stub (`interrupts.rs::isr_page_fault`, vector 14). It reads CR2 + the error code, calls `fault()`, then halts if `is_fatal()`, else returns so the stub `iretq`s and retries.

**Calls into (native):** `crate::vm::is_lazy_region` / `crate::vm::lazy_map` (the demand-paging policy + alloc/map mechanics, over `frames`/`paging`), and `serial::*`. The `crate::vm` paths resolve per crate — real in the kernel, a controllable test-double in `kernel-tests` (the "shared `.frs`, different native actions per target" pattern).

## Testing

**State graph snapshot (Level 2):** `kernel-tests/tests/state_graphs.rs::page_fault_handler_state_graph_snapshot`.

**Behavioral (Level 3):** `kernel-tests/tests/page_fault_handler_behavior.rs` — 5 tests against the `vm` double: fresh-not-fatal; non-lazy fault → fatal; lazy fault that maps → recovered; lazy fault OOM → fatal; independent classification across faults.

**QEMU (Level 7):**
- `page_fault_demand_b2` — touching a registered lazy region faults in (`$LazyFault` maps, the access then succeeds): `[#PF] demand fault recovered: ok`. No `KERNEL EXCEPTION` (the `#PF` goes to `isr_page_fault`, not the generic safety net).
- `page_fault_fatal_b2` — an unmapped, non-lazy access is `$Fatal`, reported (`[#PF] FATAL unhandled fault at 0x0000600000000000`) and halted cleanly — no `triple fault`.

## Open questions
- **`$StackGrow` / `$CopyOnWrite`** are part of the committed design but not implemented at B2 (no user VMAs / `fork` yet). They land at B3/B4, where the `=> $^` forwarding to `$FaultActive`'s kill handler becomes load-bearing.
- **Allocation in exception context:** `fault()` dispatch allocates (Rc event + context), which is fine at B2 (the heap is fully mapped, faults are on demand regions). A `no-alloc` codegen mode (tracked framec gate) would remove even that.

## Related documents
- [Roadmap](../roadmap.md) — B2 (B2-1/B2-2/B2-5); deeper children at B3/B4
- [Scheduler](scheduler.md) — the *async* counterpart (why the timer ISR can't drive Frame but `#PF` can)
- [Architecture](../architecture.md) — `PageFaultHandler` (B4) HSM note

## Change log
- **2026-05-20** — initial doc; B2 Step 3. `$Classifying → $LazyFault | $Fatal` driven from the `#PF` handler; `$StackGrow`/`$CopyOnWrite` + `=> $^` kill-forwarding deferred to B3/B4.

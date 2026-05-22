# Frame in an OS kernel — a running, honest assessment

**Purpose.** A candid, evidence-based evaluation of how well Frame (the state-machine language) serves an *operating-system* codebase, written from inside the Frame OS build. Not marketing: the goal is to record where Frame genuinely earns its place, where it's neutral ceremony, and where its execution model actively fights kernel realities — so the language's value for systems work can be judged from real evidence rather than intuition.

**Method.** One entry per major milestone (B0…), each noting the Frame systems introduced, what Frame bought *over plain Rust*, and what it cost. Plus a cross-cutting "OS-level findings" section that's the real payload. **This is a living document** — appended as the project progresses; the change log at the bottom tracks revisions.

**Standing caveat.** The maintainer explicitly directed "maximize Frame utilization, even for one-state components." So some Frame usage here is *deliberate demonstration*, not the leanest engineering choice — that's called out where relevant rather than scored as a Frame win.

---

## TL;DR verdict (as of B5 Step 4e)

- **Frame is a clear net win for exactly one class of problem: genuinely state-shaped, multi-state protocols/lifecycles where the same event means different things per state.** `TcpConnection` (RFC-793) is the strongest case in the codebase; `PageFaultHandler`'s classify-then-funnel is a solid second.
- **It is roughly a wash for the many small lifecycle/mode machines** (`Mount`, `OpenFile`, `UdpSocket`, `SerialDriver`, `Scheduler`, `BlockRequest`): a `bool`/`enum` would be fewer lines; the Frame version is justified mainly by uniformity + free diagrams.
- **It does not touch the hard ~70% of a kernel** (paging, GDT/IDT, context switch, DMA, checksum/sequence arithmetic), and **every difficult bug in the project lived in native code or the Frame↔native boundary**, not in Frame state logic.
- **Its runtime model is mismatched with kernel constraints** in two concrete, load-bearing ways (per-event heap allocation; run-to-completion on a shared, stateful instance) that forced real indirection (`post`/`drain`, pending-flags) and would be unacceptable on a production hot path.
- **The project's truest value is as a Frame stress test** — it has already produced a fixed framec bug (stringified enter args) and a reusable idiom (timer-via-enter-handler + native wheel) — more than as evidence Frame makes OS development easier.

---

## Per-milestone

### B0 — boot (`Kernel` HSM, `SerialDriver`)
- **Bought:** the `Kernel` boot chain (`$Booting` parent → init-phase children → `$Running`) gives a readable boot sequence, and the `=> $^` panic-forwarding (write the panic path once on the parent, inherit it per phase) is a genuine, if small, win over five hand-copied panic calls. The committed `kernel.svg` is real documentation.
- **Cost / neutral:** `SerialDriver` is a 2-state init gate (`$Uninitialized → $Ready`); a `bool initialized` is equivalent and shorter. The doc admits it's "deliberately minimal." Net: the HSM proved out `=> $^` forwarding end-to-end before anything depended on it — useful as scaffolding, marginal as engineering.
- **OS note:** none of the actual B0 hard work (Limine handoff, GDT/IDT, paging bring-up, UART register programming) is Frame; it's all native. Frame organized the *narration* of boot, not boot.

### B1 — preemptive scheduling (`Scheduler`, `Task`)
- **Bought:** little, honestly. `Scheduler` is `$Idle`/`$Active` (a runnable-count mode); `Task` is a lifecycle that the architecture itself notes omits `$Running` because "on the CPU" flips every timer tick from the ISR, which **cannot fire Frame events** (non-reentrant) — so the most important scheduler state is *deliberately not* in Frame.
- **OS note (important, recurring):** the very first milestone surfaced the core mismatch — the preemptive scheduler's hot transition (which task is running) is native because the ISR can't drive Frame. Frame models the *coarse* mode; native does the real switching. This pattern repeats everywhere.

### B2 — virtual memory (`PageFaultHandler`)
- **Bought (real):** `PageFaultHandler` is the first place `=> $^` becomes load-bearing: `$Classifying → $LazyFault` recovers; on giving up, children self-send `unrecoverable()`, funneled to one parent handler that decides `$Killing` vs `$Fatal`. "What to do when we give up" is written once. In plain Rust this is a helper everyone must remember to call; the Frame version makes it structural. Classify-then-dispatch is a legitimate state-machine shape.
- **Cost:** the fault *data* (CR2 address, error code, is_user) is shared/queried state, not flowing data — correctly kept in the domain/native side. (When we later tried to "thread it" for the pipeline exercise, it didn't fit — see B5.)

### B3 — the multitasking core (`SyscallDispatcher`, `Process`, `ProcessTable`, `ElfLoader`)
- **Bought:** `SyscallDispatcher`'s `$Active.reject` funnel (the `=> $^` showcase again) is clean. `ElfLoader`'s phase pipeline with a single `$Failed` sink (rollback written once) reads well. `Process`/`ProcessTable` are reasonable lifecycle models.
- **Cost (the big one — see OS findings #2):** B3 is where Frame's execution model *created* a bug class. Implementing `exit`/`wait` the obvious way — diverging/blocking *inside* a `SyscallDispatcher` handler — corrupts the **single shared** dispatcher (leaves it stuck in `$Executing`), silently dropping the next process's syscalls (and deadlocking `wait`). The fix is pure Frame-tax: `PENDING_EXIT`/`PENDING_WAIT` flags so the block/diverge happens *after* the handler returns and the machine has settled. A plain `match` syscall demux has no "stuck mid-transition" state to corrupt.
- **OS note:** every genuinely hard B3 bug — register clobbering across the `syscall` boundary, the `IF=0` dead-task park hang, the `task_unready` scheduler race — was in native/asm or the boundary. Frame's state graphs were correct; the difficulty was elsewhere.

### B4 — block device + filesystem (`BlockRequest`, `Mount`, `OpenFile`)
- **Bought:** `OpenFile`'s "access mode is the state" (a write to a read-fd is *undispatchable*) is a nice correctness-by-construction property. The `post`/`drain` pattern is *born* here and is genuinely good design.
- **Cost / neutral:** `BlockRequest` and `Mount` are small lifecycles a `bool`/`enum` covers. The on-disk format, buffer cache, virtio-blk DMA, path walking — all native, all the actual work.
- **OS note (the decisive finding — see #3):** `post`/`drain` exists because **you cannot dispatch a Frame system from an interrupt handler**: Frame dispatch allocates (`Rc` + `BTreeMap`) through a spinlocked heap, and allocating in an ISR that preempted a lock-holding mainline is a hard deadlock. (Reentrancy is a *secondary*, mitigable concern — interrupt masking would fix it; the allocation deadlock is *not* mitigable while Frame allocates per event.) The constraint happened to push us toward the correct top-half/bottom-half architecture — a constraint that coincides with good practice.

### B5 — networking (`ArpResolver`, `RxPipeline`, `UdpSocket`, `TcpConnection`)
- **Bought (the headline win):** `TcpConnection` is the best case for Frame in the whole project. TCP's per-state event interpretation (a FIN means passive-close in `$Established`, simultaneous-close in `$FinWait1`, advance-to-`$TimeWait` in `$FinWait2`) is *exactly* what a state machine is for. The FSM was **correct on the first host run** — 16 behavioral tests mapped 1:1 to RFC-793 transitions, including simultaneous open/close, and the state logic never needed debugging. The forced enumeration of states/events produced a right-first-time control skeleton. The `$Open.rst` funnel makes RST-from-any-state one handler. The committed SVG *is* the RFC-793 diagram.
- **Bought (a genuine idiom):** `ArpResolver` established the answer to "Frame has no `after(ms)`": **arm the timer in the enter handler, cancel in the exit handler, let a native timer wheel fire a `timeout()` event via `post`/`drain`.** `TcpConnection` reuses it for retransmit + `$TimeWait`. This is a clean, reusable pattern and a real positive finding.
- **Cost #1 — the pipeline-threading was partly ceremony.** Converting `SyscallDispatcher`/`ElfLoader`/`RxPipeline` to thread data via enter params is, in straight Rust, "pass an argument." The payoff is "the data flow appears in the diagram." Worth it as a cookbook demonstration (and it surfaced the stringified-enter-arg bug); not obviously the leanest design. `PageFaultHandler` was *correctly left unconverted* because its data is shared/queried, not flowing — an honest case where the pattern doesn't fit.
- **Cost #2 — the runtime allocation mismatch is now load-bearing.** `RxPipeline.deliver()` and `TcpConnection.segment()` allocate `Rc` + `BTreeMap` **per packet/segment**. Fine for a one-connection demo at trivial rates; a non-starter for a real TCP stack (no kernel allocates on the packet path). The headline system, at production scale, would have to abandon Frame's runtime exactly where it matters most.
- **Boundary friction returned:** the live TCP work's hard parts were all native/boundary — slirp accepting host connections before the guest handshakes (forcing connection-recycling + a "wait for `[tcp] listening`" probe), and QEMU `guestfwd` connecting to its target at startup. Frame's FSM was not involved in any of these.

---

## OS-level findings (the real payload)

**1. Frame's leverage over a kernel is bounded by construction (~30/70).** It governs control flow; native owns data, primitives, unsafe, and arithmetic — which is the majority of a kernel and essentially all of its difficulty. So even a *great* result on the Frame portion moves the whole-project needle modestly.

**2. The Frame↔native boundary is where the bugs are.** State graphs came out correct and were cheap to test (host behavioral tests map 1:1 to transitions). Every painful bug was native (asm, DMA, sequence/checksum, slirp/QEMU quirks) or at the boundary (the shared-dispatcher corruption). Frame neither caused most of these nor helped debug them — except the shared-dispatcher class, which it *did* cause.

**3. The runtime model is the core mismatch, and it has two heads:**
   - **Per-event heap allocation.** Every dispatch allocates (`Rc<FrameEvent>` + `BTreeMap` context). Consequences: (a) **cannot dispatch from an ISR** — allocating against the spinlocked heap while a preempted mainline holds the lock deadlocks; this forces `post`/`drain`. (b) **untenable on hot paths** — per-segment allocation in a TCP stack is unacceptable. Both are the same root cause.
   - **Run-to-completion on a shared, stateful instance.** A handler must return so the generated `__kernel` can finish the compartment switch; the "current state" is mutable data on a single shared object. Consequence: **you cannot block or diverge inside a handler** without leaving the shared machine corrupt — forcing the `PENDING_*` deferral indirection.

**4. Where Frame is unambiguously good:** the `=> $^` parent-funnel (write a disposition once, inherit structurally) and the forced-explicitness discipline (the compiler makes you enumerate each state's handled events, which produced a correct-first-time TCP FSM). These are real, repeatable wins on state-shaped problems.

**5. Where Frame is overhead:** small 2-state lifecycle/mode machines generate ~200 lines of framework (event enum, compartments, dispatch kernel, alloc) to replace a `bool` + an `if`. Uniformity and free diagrams are the only justification; correctness/expressiveness gains are negligible at that size.

**6. Tooling/process wins are underrated:** committed, drift-checked state-graph SVGs and the 1:1 transition→test mapping are genuine workflow benefits independent of the runtime. A reviewer audits the TCP graph without reading code; CI fails on drift.

**7. framec maturity, as exercised here:** found and (maintainer-)fixed the stringified enter/exit-arg bug (typed values were round-tripped through `String`, breaking byte arrays/structs — now typed per-state context structs). Minor ongoing friction: native statements in handlers need manual semicolons; debugging a misbehaving handler means reading generated Rust. No correctness problems in the generated code itself once the enter-arg fix landed.

---

## Scorecard

| Dimension | Verdict |
|---|---|
| Complex protocol/lifecycle FSMs (TCP, PFH) | **Net positive** — the reason to use Frame |
| Error/disposition funneling (`=> $^`) | **Net positive** — small but repeatable |
| Forced-explicitness → correctness | **Net positive** — TCP correct first try |
| Diagrams as committed, checked docs | **Net positive** — workflow, not runtime |
| Small lifecycle/mode machines | **Neutral** — ceremony vs. a `bool`/`enum` |
| Data threading via enter params | **Neutral→negative** — vs. function args; doc value only |
| Interrupt-driven paths | **Negative** — per-event alloc forces `post`/`drain` |
| Blocking/diverging control flow | **Negative** — forces `PENDING_*` deferral |
| Performance-sensitive hot paths | **Negative** — per-event allocation |
| The hard 70% (native primitives) | **N/A** — Frame doesn't apply |

---

## Open questions to watch (future milestones)

- **B5 scale:** many concurrent `TcpConnection` instances + a connection table — does the per-event allocation + per-instance dispatch hold up, or does it confirm the hot-path concern quantitatively?
- **B6 (USB):** deep protocol HSMs with timed transitions and many ports — another genuine FSM domain; should be a *positive* case like TCP.
- **B7 (SMP):** the concurrency gate — does framec need `Send`+`Sync` codegen, and does the cross-core `post` story hold? This is where the "non-reentrant, allocating, shared instance" model meets true parallelism; expected to be the hardest test of the runtime model.
- **A no-alloc / preallocated event path** would resolve both heads of finding #3 — worth flagging to framec as the single highest-value change for systems use.

---

## Change log
- **2026-05-21** — initial draft, covering B0–B5 Step 4e. Established the per-milestone format + the OS-level findings (esp. the allocation-driven `post`/`drain` and the run-to-completion `PENDING_*` constraints). To be appended as B5 finishes and B6/B7 begin.
- **2026-05-21** — B5 Step 5 (TAP inbound responders). One small Frame-positive worth recording: adding the inbound ARP/ICMP responders required **no new states and no new events** — they slot into the existing `RxPipeline` `$Arp`/`$Icmp` leaf handlers (the frame is already classified to that leaf; the leaf just gains a "reply" branch). This is the classify-once, dispatch-to-leaf shape paying off: behavior grows at the leaf without touching the graph. The actual responder logic is, as ever, native (Ethernet/ARP/ICMP encode), so Frame's contribution here is purely the dispatch structure, not the protocol work — consistent with the standing ~30/70 split. (No new Frame findings; the headline B5 conclusions stand.)

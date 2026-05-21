# Frame Systems

This directory holds per-system reference documentation. Each Frame system used by Frame OS gets its own document covering its purpose, state graph, interface, and the rationale for organizing it as a state machine.

## Reading order

- If you want the project's overall structure, read [`../architecture.md`](../architecture.md) first.
- If you want to write a new per-system doc, read [`_template.md`](_template.md). It defines the required sections and the expected level of detail.
- If you want to find a specific system, scan the index below.
- If you want to know what testing each system needs, read [`../testing.md`](../testing.md) for the project-wide approach; each per-system doc's Testing section enumerates the system's specific coverage.

Per-system docs are written when the system is implemented, not before. A "Planned" entry below has no document yet; an entry marked "Documented" links to its file.

## Status conventions

- **Planned** — referenced in [`../architecture.md`](../architecture.md) and [`../roadmap.md`](../roadmap.md); no implementation, no per-system doc.
- **In progress** — implementation underway; doc is a stub or partial.
- **Documented** — implementation complete enough for the doc to reflect actual behavior, including a generated state diagram.

## Hosted-mode systems

These run inside the hosted-mode shell (`cargo run --bin frame-os-shell`) on Linux, macOS, or Windows. They do not appear in the bare-metal kernel.

| System | Milestone | Status | Description |
|---|---|---|---|
| [`Shell` (hosted variant)](shell.md) | H0–H3 | In progress (H3 complete) | Top-level shell lifecycle: prompt, parse, run builtins or external commands, repeat. State-dependent Ctrl-C and Ctrl-Z handling. `&` background launch and `jobs`/`fg`/`bg`/`wait`/`kill` builtins. |
| [`Parser`](parser.md) | H1 | In progress (H1) | Per-char event-driven tokenizer. `$ReadingWord → $InWord → $InQuotedString → $Done / $Failed`. Handles whitespace separation and double/single quoted substrings. |
| [`JobControl`](job_control.md) | H3 | In progress (H3 Step 2 — standalone FSM landed; integration at Step 3) | Manager system for background jobs. Holds `Vec<Job>`. 2 states, 6 edges, 19 behavioral tests. |
| [`Job`](job.md) | H3 | In progress (H3 Step 1 — standalone FSM landed; integration at Step 3) | Per-instance job state machine. One instance per running, stopped, or completed external command. 5 states, 14 edges, 16 behavioral tests. |

## Bare-metal kernel systems

These run inside the bare-metal kernel image. They do not appear in the hosted-mode shell.

| System | Milestone | Status | Description |
|---|---|---|---|
| [`Kernel`](kernel.md) | B0 | Documented | Top-level kernel lifecycle. HSM: `$Booting` parent over per-phase init children, then `$Running`, then `$Halted`. |
| [`SerialDriver`](serial_driver.md) | B0 | Documented | COM1 console driver. `$Uninitialized → $Ready` (enforces "program the UART before you transmit"). The first bare-metal Frame system. |
| [`Scheduler`](scheduler.md) | B1 | Documented | Run/halt mode for the preemptive scheduler. `$Idle` (halt) / `$Active` (≥1 runnable). The native ISR does the round-robin picking. |
| [`Task`](task.md) | B1 | Documented | Task lifecycle. `$Created → $Ready ⇄ $Blocked → $Terminated`. Host-validated; becomes load-bearing as `Process` at B3. |
| [`PageFaultHandler`](page_fault_handler.md) | B2 | Documented | Classifies a page fault and dispatches the response from inside the `#PF` handler. `$Classifying → $LazyFault \| $Fatal` under `$FaultActive`. |
| [`SyscallDispatcher`](syscall_dispatcher.md) | B3 | Documented | Validate + execute a syscall, errors funneled to the `$Active` parent via `=> $^`. `$Validating → $Executing` under `$Active`. |
| `Process` | B3 | Planned | Replaces `Task`. Full process lifecycle including `$Zombie` and `$Reaped`. |
| `ProcessTable` | B3 | Planned | Slot management for the process array. One state machine per slot. |
| `ElfLoader` | B3 | Planned | Parses ELF bytes and produces a process image. Phase-by-phase loading with cleanup on failure. |

## Shared systems

Some Frame source files are reused between the hosted and bare-metal tracks. The Frame state machines are identical; the native action implementations differ.

| System | Hosted milestone | Bare-metal milestone | Notes |
|---|---|---|---|
| `Shell` | H0–H3 | B2 | Same `.frs` source, different actions (`std::process::Command` in hosted; bare-metal task interface in kernel). |
| `Parser` | H1 | B2 | Same `.frs` source; bare-metal version uses fewer Rust standard-library types. |

## Cross-cutting documentation

When the project grows enough to need them, additional documents will live alongside this index:

- **`_template.md`** — required structure and tone for a per-system doc.
- **`_patterns.md`** *(not yet written)* — recurring HSM patterns used across multiple systems. Examples: parent-state-as-shared-error-handler, manager + N instances, classifier-then-dispatch, fetch-decode-execute loop.
- **`_interactions.md`** *(not yet written)* — diagrams of how systems compose at runtime. Which systems hold references to which, which events flow between them, what the kernel's top-level supervisor relationships look like.

These docs are deferred until at least three per-system docs exist — the patterns aren't visible until there are enough concrete examples to factor from.

## Diagram convention

Each documented system has a generated GraphViz diagram alongside its doc:

```
docs/systems/
├── README.md
├── _template.md
├── shell.md
├── shell.svg            ← generated from frame/shell.frs via `framec -l graphviz`
├── parser.md
├── parser.svg
└── ...
```

The `.svg` files are committed to the repo and regenerated as part of the build. A reader browsing the repo on GitHub sees the diagram inline in the corresponding `.md` file.

The generation step is wired into `cargo xtask diagrams`. When a `.frs` file changes, its corresponding `.svg` is regenerated before commit (via a pre-commit hook or CI check, decision deferred until the first system lands).

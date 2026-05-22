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
| [`Shell` (hosted variant)](shell.md) | H0–H3 | Documented (H0–H3) | Top-level shell lifecycle: prompt, parse, run builtins or external commands, repeat. State-dependent Ctrl-C and Ctrl-Z handling. `&` background launch and `jobs`/`fg`/`bg`/`wait`/`kill` builtins. |
| [`Parser`](parser.md) | H1 | Documented (H1) | Per-char event-driven tokenizer. `$ReadingWord → $InWord → $InQuotedString → $Done / $Failed`. Handles whitespace separation and double/single quoted substrings. |
| [`JobControl`](job_control.md) | H3 | Documented (H3 — integrated) | Manager system for background jobs. Holds `Vec<Job>`. 2 states, 6 edges, 19 behavioral tests. |
| [`Job`](job.md) | H3 | Documented (H3 — integrated) | Per-instance job state machine. One instance per running, stopped, or completed external command. 5 states, 14 edges, 16 behavioral tests. |

## Bare-metal kernel systems

These run inside the bare-metal kernel image. They do not appear in the hosted-mode shell.

| System | Milestone | Status | Description |
|---|---|---|---|
| [`Kernel`](kernel.md) | B0 | Documented | Top-level kernel lifecycle. HSM: `$Booting` parent over per-phase init children, then `$Running`, then `$Halted`. |
| [`SerialDriver`](serial_driver.md) | B0 | Documented | COM1 console driver. `$Uninitialized → $Ready` (enforces "program the UART before you transmit"). The first bare-metal Frame system. |
| [`Scheduler`](scheduler.md) | B1 | Documented | Run/halt mode for the preemptive scheduler. `$Idle` (halt) / `$Active` (≥1 runnable). The native ISR does the round-robin picking. |
| [`Task`](task.md) | B1 | Documented | Task lifecycle. `$Created → $Ready ⇄ $Blocked → $Terminated`. Host-validated; becomes load-bearing as `Process` at B3. |
| [`PageFaultHandler`](page_fault_handler.md) | B2 | Documented | Classifies a page fault from inside the `#PF` handler. `$Classifying → $LazyFault` recovers; `$FaultActive`'s `=> $^` funnel routes unrecoverable faults to `$Killing` (ring-3 → kill process) or `$Fatal` (kernel → halt). Isolation added B3 Step 4b. |
| [`SyscallDispatcher`](syscall_dispatcher.md) | B3 | Documented | Validate + execute a syscall, errors funneled to the `$Active` parent via `=> $^`. `$Validating → $Executing` under `$Active`. |
| [`Process`](process.md) | B3 | Documented | Per-process lifecycle: `$Created → $Ready ⇄ $Blocked → $Zombie → $Reaped`. Successor to `Task`; `kill()` funneled to the `$Alive` parent via `=> $^`. No `$Running` (native scheduler state). |
| [`ProcessTable`](process_table.md) | B3 | Documented | Manager holding `Vec<Process>`; forwards lifecycle by pid. `$HasCapacity ⇄ $Full` under `$Managing`. The B3 instance of the manager+instances pattern. |
| [`ElfLoader`](elf_loader.md) | B3 | Documented | Loads a static ELF into a process address space. `$ReadingHeader → $ValidatingHeader → $MappingSegments → $BuildingStack → $Done`, any phase → `$Failed` (rolls back partial mappings). Flat phase pipeline; the `$Failed`-funnel showcase. |
| [`BlockRequest`](block_request.md) | B4 | Documented | One block-I/O request's lifecycle: `$Queued → $InFlight → $Complete \| $Error`. Driven by the virtio-blk completion via the post/drain deferred-event pattern (first async-interrupt → Frame boundary). |
| [`Mount`](mount.md) | B4 | Documented | A filesystem's mount/unmount lifecycle: `$Unmounted → $Mounting → $Mounted → $Unmounting`. Gates FS reads on `is_mounted()`. |
| [`OpenFile`](open_file.md) | B4 | Documented | One open file descriptor's lifecycle, access mode as state: `$Open → $Reading \| $Writing → $Closed`. The VFS holds one per fd; wrong-mode ops are gated out. |
| [`ArpResolver`](arp_resolver.md) | B5 | Documented | One IPv4→MAC resolution's lifecycle: `$Incomplete → $Resolved`, with a retransmit timer armed in the enter handler and `-> $Failed` at the retry cap. The first networking Frame system; the timer-via-enter-handler pattern. |
| [`RxPipeline`](rx_pipeline.md) | B5 | Documented | Classify a received frame and dispatch it: `$Idle → $Classifying → ($Arp \| $Ipv4 → $Icmp \| $Udp)`, threading a parsed `RxDescriptor` down the graph via enter params. The marquee data-pipeline recipe. |
| [`UdpSocket`](udp_socket.md) | B5 | Documented | One UDP socket's bind lifecycle: `$Unbound → $Bound`. `recv()` is gated to `$Bound` (bind state as the invariant). The `RxPipeline` `$Udp` leaf delivers to it; drives the DHCP exchange. |
| [`TcpConnection`](tcp_connection.md) | B5 | Documented (FSM; live handshake at 4b–4d) | The full RFC-793 state machine: 11 states under an `$Open` parent funneling RST → `$Closed`. The deepest Frame system; segments processed per-state, timers via enter handlers. |
| [`IpReassembly`](ip_reassembly.md) | B5 | Documented | Reassemble a fragmented IPv4 datagram: `$Idle → $Reassembling → ($Complete \| $Expired)`, threading a `Fragment` via enter params (self-transition re-store). The second data-pipeline; validated by a real `ping -s 4000` over TAP. |
| [`HubPort`](hub_port.md) | B6 | Documented | One xHCI port's connect/reset/enable lifecycle: `$Disconnected → $Connected → $Resetting → $Enabled`, disconnect funneled via an `$Attached` parent (`=> $^`). Timed reset transition; drives the qemu-xhci usb-kbd port to enabled. |
| [`UsbEnumeration`](usb_enumeration.md) | B6 | Documented | A device's full enumeration lifecycle: `$Powered → $SlotEnabled → $AddressAssigned → $DeviceDescribed → $Configured`, fail funneled via an `$Enumerating` parent. Enter handlers issue xHCI commands / EP0 control transfers; completion events advance the FSM. Enumerates the qemu-xhci usb-kbd end to end. |

## Shared systems

Some Frame source files are reused between the hosted and bare-metal tracks: the Frame state machines are identical; the native action implementations differ. **`Parser` reuse landed at B4 Step 4b** — the *same* `frame/parser.frs` now compiles into the ring-3 `user/` crate (a userspace program, **not** a kernel task), backed by a small allocator (`user/src/mem.rs`). **`Shell` reuse is still pending**: the `Shell` `.frs` needs its `std`-only actions re-implemented for ring 3 *and* a real input device, so the userspace shells so far are hand-written Rust (one of which, `frameshell`, drives the reused `Parser`).

| System | Hosted milestone | Bare-metal milestone | Notes |
|---|---|---|---|
| `Shell` | H0–H3 (done) | B4 Step 4b+ (pending) | Same `.frs` source, different actions (`std::process::Command` in hosted; raw syscalls in a ring-3 userspace program). Not yet reused — the B4 userspace shells (`shell`, `frameshell`) are hand-written; porting the `Shell` `.frs` awaits userspace actions + an input device. |
| `Parser` | H1 (done) | B4 Step 4b (done) | Same `.frs` source, compiled unchanged for `x86_64-unknown-none` (pure system; the ring-3 build just adds a heap). Proven by `userspace_frame_parser_reuse_b4`. |

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

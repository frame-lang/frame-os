# Frame OS — B0–B7 capstone

The bare-metal track (B0–B7) is functionally complete: a real-OS-class kernel
that boots, multitasks, pages, runs user processes, mounts a filesystem, speaks
TCP/IP, drives USB, and runs on multiple cores — organized around explicit Frame
state machines for its control flow and native Rust for its hardware primitives.

This document is the retrospective: *what got built, the architecture that
emerged, and what the project actually proved about writing an OS in Frame.* For
the running blow-by-blow see [`roadmap.md`](roadmap.md); for the honest,
finding-by-finding assessment see [`frame_assessment.md`](frame_assessment.md);
for the one-page "what kind of OS + how big" see [`overview.md`](overview.md).

## What got built

| Milestone | In one line | Headline validation |
|---|---|---|
| **B0** | Boots via Limine/UEFI, runs a boot HSM to `$Running`, halts cleanly | `boot_*_b0` |
| **B1** | Preemptive multitasking (timer-driven context switches; the deferred-event queue is born) | `preemption_b1` |
| **B2** | Virtual memory, paging, per-process address spaces, demand paging via the `PageFaultHandler` HSM | `page_fault_*`, `address_space_switch_b2` |
| **B3** | User mode: processes, syscalls, ELF loading, `fork`/`exec`/`wait`, signals | `ring3_syscall`, `fork_concurrency`, `exec`, `wait_reap` |
| **B4** | virtio-blk + an on-disk filesystem (VFS, path lookup, persistence) | `fs_*`, `blk_roundtrip`, `userspace_shell_runs_program_from_disk` |
| **B5** | A full TCP/IP stack: NIC → ARP → IPv4/ICMP → UDP/DHCP → RFC-793 TCP → IP reassembly, over slirp + TAP | `tcp_echo`, `tcp_active_open`, `qemu-tap` (real ping + reassembly) |
| **B6** | USB: xHCI controller bring-up → port reset → enumeration to `$Configured` → a real interrupt-IN transfer | `usb_enumerates`, `usb_transfer` (HID key report) |
| **B7** | SMP: AP startup, per-CPU data, IRQ-safe locking, cross-core `post`, per-core LAPIC preemption, TLB shootdown | `smp_concurrent`, `smp_cross_core_post`, `smp_preempt`, `smp_tlb_shootdown` |

**The Frame systems** (25 `.frs` state machines, ~3,086 lines, each with a
committed, drift-checked state-graph SVG):

- *Boot/core:* `Kernel`, `SerialDriver`, `Scheduler`, `Task`, `PageFaultHandler`.
- *Processes:* `Process`, `ProcessTable`, `SyscallDispatcher`, `ElfLoader`.
- *Filesystem:* `BlockRequest`, `Mount`, `OpenFile`.
- *Networking:* `ArpResolver`, `RxPipeline`, `UdpSocket`, `TcpConnection` (the
  RFC-793 11-state machine), `IpReassembly`.
- *USB:* `HubPort`, `UsbEnumeration`, `UsbTransfer`.
- *SMP:* `EventCounter` (the cross-core-post demonstrator).
- *Shared with the hosted shell:* `Shell`, `Parser`, plus hosted-only
  `JobControl`/`Job`.

## The architecture that emerged

Three patterns recurred at every milestone and became the de-facto design rules:

1. **The ~30/70 split.** Frame owns the *lifecycle and control flow* — "which
   state, which events are legal, what transitions." Native Rust owns the *hard
   primitives* — page tables, context switches, DMA rings, MMIO, the LAPIC, IPIs.
   The split never moved across seven milestones; it is the honest division of
   labor between a state-machine language and a systems language.

2. **post/drain.** Hardware (and, on SMP, other cores) never dispatch a Frame
   system directly — they *post* plain event data into a queue, and the owning
   context *drains* it and dispatches to a single-owner instance. Born at B4 (the
   block-device completion interrupt), reused for B5 (the NIC), and generalized at
   B7 to cross-core posting. It's the integration seam between async hardware and
   run-to-completion state machines.

3. **`PENDING_*` deferral.** Because a handler must run to completion on a shared
   instance, anything that needs to *block or diverge* (the `exit`/`fork`/`wait`
   syscalls, TCP's `TIME_WAIT`) sets a flag and acts *after* the dispatch returns,
   never inside the handler.

And two Frame idioms paid off repeatedly: the **`=> $^` parent funnel** (write a
disposition once — `Process.$Alive.kill`, `TcpConnection.$Open.rst`,
`HubPort.$Attached.disconnect`, `UsbEnumeration.$Enumerating.fail` — and inherit
it structurally) and the **enter-handler-arms-an-async-step, completion-event-
advances-the-FSM** shape (ARP/TCP timers; xHCI commands + transfers; cross-core
posts — all the same skeleton).

## What the project proved about Frame

The thesis was: *OS lifecycle logic is clearer, more correct, and self-
documenting written as explicit state machines than as implicit `switch`-on-an-
int conditionals.* Across a full feature set, here is the honest verdict (the
scorecard lives in `frame_assessment.md`):

**Where Frame is a net win — repeatably:**
- **Complex protocol/lifecycle FSMs.** The RFC-793 TCP machine came out correct on
  the first try because the compiler made every state's handled events explicit;
  the page-fault classifier, USB enumeration, and transfer lifecycles were all
  clean. This is the reason to use Frame.
- **Disposition funnels (`=> $^`).** Small but repeatable: one teardown edge,
  inherited.
- **Diagrams as committed, checked documentation.** `cargo xtask check-diagrams`
  makes the SVG an authoritative artifact that can't drift — highest per-system
  payoff on register-dense protocols (a reviewer reads the USB enumeration graph,
  not 1,000 lines of `xhci.rs`).
- **The B7 surprise — cross-core safety for free.** framec's generated code is
  neither `Send` nor `Sync`, and the long-standing fear was that SMP would force
  an `Arc`-based codegen mode. It didn't: the single-owner-instance + post/drain
  model the runtime *forces* means only `Send` event data ever crosses a core
  boundary. The model's biggest apparent *liability* is what makes it
  concurrency-safe. No framec change was needed.

**Where Frame is overhead — equally repeatably:**
- **The ~30/70 split is real:** Frame doesn't touch the hard 70% (the unsafe
  primitives), so on a systems project most of the code is still native Rust.
- **Per-event allocation** (every dispatch allocates) forces post/drain on every
  interrupt path and rules out hot-path use; a no-alloc/preallocated event path is
  flagged as the single highest-value framec change for systems work.
- **Run-to-completion** forces the `PENDING_*` indirection for any blocking flow.
- **Small mode-machines are ceremony** — a 2-state lifecycle generates ~200 lines
  to replace a `bool`; justified only by uniformity + the free diagram.
- **~10× expansion:** the 3,086 lines of `.frs` compile to ~32,858 lines of Rust.

**Net:** Frame earns its place on the *lifecycle-shaped* parts of an OS and stays
out of the way of the rest — which is exactly what the project set out to test.
The bug distribution confirmed it: essentially every painful bug was in native
code (asm, DMA, checksums, MMIO, the GS-base ordering) or at the Frame↔native
boundary (the shared-dispatcher corruption) — almost none were in the state
machines themselves. Two framec bugs were found and filed (stringified enter-args;
the `$Empty` context-variant collision), both at the boundary, neither in the
runtime model.

## By the numbers

- **~11,000 hand-written kernel lines** (~3,086 Frame across 25 systems + ~7,826
  native Rust) — the xv6 size class, but with TCP, USB, and SMP.
- **37/37** QEMU smoke tests, green under `-smp 4`; full host behavioral + snapshot
  suite; `check-diagrams` clean.
- Dev/CI parity via the Linux dev container (QEMU under TCG, TAP networking).

## What's next

The committed B-track is done. Forward work is *refinement*, not new milestones —
see the **Post-B7 refinement track** in [`roadmap.md`](roadmap.md): a per-CPU
run-queue scheduler, deeper SMP stress, networking/USB depth that further
stress-tests Frame at scale (a TCP connection table; multi-port USB / orthogonal
regions), the no-alloc event path, and the crates.io framec-publish CI gate. An
AArch64 / Raspberry Pi port ("B8") remains a plausible later track, not committed.

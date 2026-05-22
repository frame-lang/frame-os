# Frame OS at a glance

A one-page synthesis of *what kind of OS this is* and *how big it is*. For the
"why" see [`vision.md`](vision.md); for the design see
[`architecture.md`](architecture.md); for an honest assessment of the Frame
approach see [`frame_assessment.md`](frame_assessment.md).

## What kind of OS

**In one line:** a small, monolithic, bare-metal x86_64 kernel whose every
lifecycle-shaped subsystem is written as an *explicit state machine* (in the
Frame language, compiled to Rust), with native Rust underneath for the unsafe
hardware primitives — built to demonstrate what an OS looks like when its control
flow is visible structure rather than implicit `switch`-on-an-int-`state` logic.

- **Purpose first, OS second.** Not a Linux replacement or a daily driver — a
  demonstration + stress-test vehicle for the Frame state-machine language. The
  thesis: schedulers, process lifecycles, syscall dispatch, TCP connections, USB
  enumeration, drivers — the parts of an OS that are *secretly* state machines —
  are clearer, more correct, and self-documenting written as explicit,
  diagrammable state machines. Each Frame system renders to an authoritative SVG
  that can't drift from the code because it *is* the code.
- **Shape:** monolithic (not a microkernel), bare-metal x86_64, Limine/UEFI boot,
  serial console (no GUI). Ships as **two flavors sharing most source**: a
  hosted-mode shell that runs as a normal app on Linux/macOS/Windows, and the
  bare-metal kernel that boots in QEMU (and targets Pi Pico, Pi 4/5). The
  `Shell`/`Parser` state machines are reused across both.
- **Feature set — real-OS-class, not a toy.** Across B0–B7: preemptive
  multitasking, virtual memory + per-process address spaces, user mode with
  processes/syscalls/ELF/`fork`/`exec`/`wait`, an on-disk filesystem, a full
  TCP/IP stack (NIC → ARP → IPv4/ICMP → UDP → RFC-793 TCP → IP reassembly), USB
  (xHCI controller + enumeration + transfers), and SMP (multi-core, per-CPU data,
  locking, cross-core event posting).
- **The defining split (~30 / 70).** Frame owns the **lifecycle + control flow**
  ("which state, which events are legal, what transitions"); native Rust owns the
  **hard primitives** (page tables, context switches, DMA rings, register pokes).
  The recurring integration pattern — proven from interrupts (B4/B5) through SMP
  (B7) — is **post/drain**: hardware/other cores only *post* event data into a
  queue; the owning context *drains* it and dispatches to a single-owner Frame
  instance.
- **Bounded on purpose:** the process is the unit of concurrency (one thread per
  process); static binaries only; a POSIX-subset signal set; no dynamic linking,
  no GUI/graphics/audio, no virtualization.

## How big

Measured at the B7 cross-core-post milestone:

| Part | Lines | Notes |
|---|---|---|
| **Frame state machines** (`.frs`) | **~3,086** across **25 systems** | the hand-written control-flow "intent" |
| **Native kernel Rust** (`kernel/src`) | **~7,826** | the hardware primitives (the ~70%) |
| **Hand-written kernel total** | **~11,000** | Frame + native (excl. generated, tests, tooling) |

Largest native files: `xhci.rs` (~1,035), `net.rs` (~813), `usermode.rs` (~619),
`sched.rs` (~518). Binary: `.text` ≈ 700 KB (debug; release is smaller), `.bss`
≈ 595 KB (static buffers — stacks, DMA rings, heap region).

**Calibration.** This is in the **xv6 (teaching-OS) size class** (~9K lines) — a
kernel you could read end-to-end in an afternoon — but **unusually
feature-complete for that size**: it has a full TCP/IP stack, USB, and SMP, which
xv6 does not. Linux is ~30M+ lines; we are ~0.04% of that.

**One Frame-specific number worth flagging:** those ~3,086 lines of `.frs`
compile to **~32,858 lines of generated Rust** — a ~**10× expansion**. That's the
framework ceremony (event enums, compartments, the dispatch kernel, per-state
context structs) — the quantitative side of `frame_assessment.md`'s "small
lifecycle machines are neutral/overhead" verdict: you pay generated boilerplate
for uniformity + free diagrams.

## The bottom line

A pedagogically-motivated but genuinely full-featured monolithic OS, organized
around explicit Frame state machines for its control flow and native Rust for its
hardware mechanics — small in the xv6 sense (~11K hand-written lines), but with
TCP, USB, and SMP — a working argument that OS lifecycle logic is better written
as visible state machines than as implicit conditionals.

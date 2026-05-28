# B-HAL — a hardware-abstraction seam under the kernel (port toward AArch64 / Pi)

**Status: PLANNED (2026-05-27).** Goal: pull the kernel's x86-specific *mechanism*
behind a small set of arch traits (a HAL) so the platform-agnostic kernel — the
Frame FSMs + the pure-logic subsystems — sits on top unchanged, and a second
architecture (AArch64, e.g. Raspberry Pi) can be added by implementing the HAL
rather than forking the kernel.

This is the same **FSM-owns-logic / native-owns-mechanism** seam the project
already uses (`ProcessBackend`, `ShellEnv`, virtio_blk's backend), applied at the
*platform* level. The HAL is just the biggest seam.

## Why this is tractable (the audit, 2026-05-27)

A touchpoint audit of `kernel/src/` (13.8k LOC) found:

- **The only external arch/boot dependency is `limine` (0.5).** Every other x86
  primitive is **hand-written `asm!`** — ~**53 inline-asm sites across 13 files**
  (port I/O, MSRs, CR3, `sti`/`cli`/`hlt`, IDT/GDT loads, context switch). There
  is *no* `x86_64` crate boundary to untangle; the coupling is our own code,
  grouped in identifiable files.
- **~11 source files + all 31 `.frs` FSMs are already arch-agnostic** — they
  contain zero asm / MSR / port / Limine references: `allocator`, `console`,
  `crosscore`, `elf`, `frame_systems`, `lockorder`, `pci`, `ramdisk`, `reactor`,
  `sched_demo`, `vfs`, plus the protocol/logic crates (`fs`, `net`, `tcp`,
  `ip_reasm`, `pipe`, and the `.frs` systems). These already *sit on a HAL*; they
  just call into mechanism that isn't behind a trait yet.

So the kernel already splits cleanly into **"mechanism" (becomes the x86 HAL
impl)** and **"logic" (sits on the HAL)**. The job is to name the boundary.

## The coupling map (what goes behind the HAL)

| Concern → HAL trait | x86 mechanism today (files) | AArch64 equivalent |
|---|---|---|
| **`Cpu`** — enable/disable IRQs, halt, pause | `sti`/`cli`/`hlt` in `interrupts`, `main`, `spin` | `msr daifset/clr`, `wfi` |
| **`Context`** — register frame + switch | `context.rs`, `pcsched.rs` (switch asm) | AArch64 reg frame + `eret` |
| **`Mmu`** — map/unmap, switch AS, TLB | `paging.rs` (CR3, `invlpg`) | TTBR0/1, `tlbi` |
| **`Irq`** — controller init, EOI, mask | `interrupts.rs` (IDT), `lapic.rs`, `pic.rs` | GICv2/3 (`gicd`/`gicc`) |
| **`Timer`** — periodic tick, oneshot | `lapic.rs` (LAPIC timer), `pit.rs` | ARM generic timer (`CNTP_*`) |
| **`Clock`** — wall-clock time | `rtc.rs` (CMOS) | RPi mailbox / RTC |
| **`Console`** — byte in/out | `serial.rs` (16550 UART) | PL011 UART |
| **`PerCpu`** — per-core base ptr | `percpu.rs` (`IA32_GS_BASE` MSR) | `TPIDR_EL1` |
| **`Fpu`** — enable + save/restore | `fpu.rs` (SSE/`fxsave`) | NEON/FP `Q` regs |
| **`Boot`** — memory map + handoff | `main.rs` + `frames.rs` (Limine) | RPi firmware + **device tree** |
| **`SyscallEntry`** — ring-3 trap path | `usermode.rs` (`syscall`/`sysret`, MSRs) | `svc`/`eret`, `ESR_EL1` |

Tightest coupling (do these carefully): the **interrupt path** (`interrupts.rs` —
IDT, ISR stubs, the LAPIC-timer ISR that drives preemption, the syscall entry),
the **context switch** (`context.rs`/`pcsched.rs`), and **boot** (Limine is x86
UEFI; a Pi has a totally different handoff + device tree — this is the one piece
with *no* shared shape, so `Boot` is more "two implementations of the same kernel
init contract" than a thin trait).

## What does NOT move

- **The 31 Frame FSMs** — `Scheduler`, `Process`, `ProcessTable`, `TcpConnection`,
  `Mount`, `Shell`, … They coordinate; they don't poke hardware. Portable by
  construction — the whole point.
- **The pure-logic subsystems** (fs/vfs/net/tcp/ip_reasm/pipe/elf/allocator). They
  call the HAL but contain no arch code.
- **virtio / xHCI drivers** are *mostly* portable (MMIO + rings); the one arch bit
  is **PCI config access** (port I/O `0xCF8/0xCFC` on x86 vs ECAM MMIO on ARM) —
  that hides behind `pci.rs` becoming a tiny HAL call. Real Pi storage/net would
  use different controllers, but that's device work, not HAL.

## Milestones (lowest-risk-first; x86 stays green throughout)

The discipline that worked for M1→M4: **extract the seam on the *working* arch
first, prove no behavior change, then add the new arch.** Never extract-and-port
at once.

- **B-HAL.1 — Define the traits + an `arch::x86_64` module, no behavior change.**
  Create `kernel/src/hal.rs` (the trait definitions) and `kernel/src/arch/x86_64/`
  (move the mechanism files behind them). The kernel calls `hal::cpu()`, `mmu()`,
  `irq()`, etc.; x86 impls are the current code, relocated. Validate: identical
  `qemu-test` smoke + `console-test` green, clippy/fmt clean. **Pure refactor** —
  the high-value, self-contained first step (this is the analogue of M2 / M3b.1).
  *Decision (2026-05-27):* the accessors resolve at **build time** via
  `cfg(target_arch)` to a single concrete arch impl (no runtime `dyn`, no
  injection) — the substrate (spinlocks, ISR stubs, the panic handler) is called
  from no-`self` contexts that can't receive a passed reference, and there is
  only ever one HAL per binary, so the trait is the seam and selection is at
  compile time. *Progress (2026-05-27):* first seam landed — **`Console`** (the
  smallest, most-isolated leaf). `kernel/src/hal.rs` holds `trait Console` +
  `console()`; `kernel/src/arch/x86_64/serial.rs` holds the 16550 impl; the
  existing `serial.rs` stays as the arch-agnostic *text* layer (write_str /
  writeln / write_hex / write_decimal) sitting on the trait, so all ~hundreds of
  `serial::*` call sites are unchanged (only `init_uart`/`write_byte`/`rx_byte`/
  `enable_rx_interrupt` were genuinely arch-specific). Validated: default +
  interactive build, clippy/fmt clean, **49/49 qemu-test smoke, console-test
  PASS**. The remaining concerns (Cpu / Clock / Fpu / Mmu / Irq / Timer / PerCpu
  / Context / SyscallEntry) fan out behind the same proven pattern.
- **B-HAL.2 — Isolate boot + the IDT/IRQ path.** The hardest seam: factor the
  Limine handoff + IDT setup + the timer/syscall ISR entry behind `Boot` + `Irq` +
  `SyscallEntry` so the arch-agnostic kernel init is one sequence calling HAL
  hooks. Still x86-only; still green.
- **B-HAL.3 — AArch64 skeleton: boot + console + a halt loop.** New
  `arch/aarch64/` + `aarch64-unknown-none` target: device-tree-driven boot, PL011
  console, GIC + generic-timer stubs, MMU bring-up — enough to print the banner
  and halt under `qemu-system-aarch64 -M virt`. (The AArch64 B0.)
- **B-HAL.4 — AArch64 scheduling + ring-0 FSMs.** Context switch + timer
  preemption + per-CPU on ARM, until the `Scheduler`/`Task` FSMs run a kernel
  thread. The same FSMs, now on a second ISA.
- **B-HAL.5 — AArch64 user mode + a storage/console device.** `svc` syscall path,
  the `SyscallProcessBackend` over it, a virtio-mmio (QEMU virt) or RPi device, so
  `ish` (already arch-agnostic — its FSMs + syscalls) runs. Then `console-test`
  on `qemu-system-aarch64`. **Real Pi hardware** is a further step (RPi-specific
  drivers + SD-card boot) past the QEMU `virt` board.

## Risks / honest scope

- **This is a real OS port** — B-HAL.3+ is a from-scratch AArch64 substrate (GIC,
  generic timer, MMU, device tree, `svc`). The HAL makes it *additive* rather than
  a fork, and the FSMs come for free, but the mechanism is genuine new work — the
  ~70% the journal keeps saying Frame doesn't touch.
- **Boot has no shared shape.** Limine (x86 UEFI) vs RPi firmware + device tree are
  not one thin trait; `Boot` is two implementations of a kernel-init *contract*
  (give me a memory map + a console + the HAL), not a drop-in.
- **Don't regress x86.** B-HAL.1/.2 are validated against the existing smoke +
  console suites at every step; the AArch64 work is purely additive (new
  `arch/aarch64/`, new target), so x86 can't break from it.
- **Scope is staged + independently shippable.** B-HAL.1 (the trait extraction on
  x86) is valuable on its own — it documents and isolates the arch boundary — even
  if the AArch64 port is never finished.

## What the HAL reaches — and what it doesn't (a note on QEMU + #110)

A recurring confusion worth pinning down: the HAL is **not** a layer that
replaces QEMU, and it does **not** fix `#110`.

- **The HAL abstracts the kernel's choice of mechanism** — which driver,
  controller, timer, MMU the kernel calls. That layer *does* apply to every
  target: e.g. RAM-disk-vs-virtio is a HAL-level backend choice available on any
  platform (we already pick RAM disk for x86 interactive today).
- **QEMU is not a layer Frame OS sits on.** Frame OS is a bare-metal kernel; it
  runs directly on a CPU. The only question is *which* CPU: real silicon, a
  hardware-virtualized CPU (KVM on x86 Linux, HVF on AArch64 Macs), or an
  *emulated* CPU (QEMU TCG). The kernel can't tell the difference; that choice is
  made by *how you launch it*, not by anything in the kernel or the HAL.
- **`#110` lives below the HAL.** It's a stale-read artifact in QEMU's *host-side*
  emulation of the virtio-blk device thread when emulating x86 on an arm64 host
  (TCG). It sits beneath every layer we author — the HAL can no more reach it
  than our virtio-blk driver could when we tried barriers/lost-IRQ recovery.

So the relevant axis for `#110` is not a HAL-design choice; it's the **host
execution mode**: TCG emulation (hits it) vs native virtualization (doesn't).
B-HAL.3+ doesn't fix `#110` on the x86 build — it enables a *second guest ISA*
(AArch64) that an arm64 Mac runs under **HVF (native virt)** instead of TCG,
where the bug doesn't exist. That's the path that retires the RAM-disk
mitigation on developer Macs and puts the real virtio-blk + the
`IoScheduler`/`BlockRequest` Frame systems back on the runtime critical path
there — not by HAL magic, but because the platform stack underneath the HAL no
longer has the bug. "Native on every target" is the *end state* of the port (a
real Pi boots straight on silicon; Mac/Linux run native-virtualized guests);
QEMU stays only as a developer-iteration convenience.

## Why it's worth it

The parity program proved one shell FSM runs on Linux and bare-metal x86. B-HAL
generalizes the claim to the *kernel*: the same `Scheduler`/`Process`/`TcpConnection`
/… FSMs coordinating a real OS on **two ISAs**, with only the mechanism swapped
behind a HAL — "write the state machine once, run the OS anywhere." And the audit
shows the kernel is already structured for it: the logic is arch-clean; only the
~53 asm sites in 13 files need a home behind the seam.

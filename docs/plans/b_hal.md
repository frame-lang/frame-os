# B-HAL — a hardware-abstraction seam under the kernel (port toward AArch64 / Pi)

**Status: B-HAL.1 clean-refactor seams COMPLETE (2026-05-28)** — six no-behavior-change
seams extracted + validated on x86: `Console`, `Cpu`, `Clock`, `Fpu`, `Mmu`,
`PerCpu`. `hal.rs` (the traits + build-time accessors) and `arch/x86_64/` (the
relocated mechanism) are in place; the platform-agnostic kernel calls
`hal::console()`/`cpu()`/`mmu()`/… Each landed as its own validated commit
(default + interactive build, clippy/fmt clean, 49/49 qemu-test smoke,
console-test PASS). The lone remaining concern, `Context` (register frame +
context-switch asm), is entangled with the IDT/ISR save path and is folded into
**B-HAL.2** (boot + the IRQ path). **B-HAL is paused here by decision (2026-05-29):**
a close survey found Irq/Timer/Context are interrupt-controller-and-boot *core*,
not clean leaves, so B-HAL.2+ (incl. the AArch64 substrate) is deferred until a
second arch exists to design the contracts against (see the B-HAL.2 note below).
Goal: pull the kernel's x86-specific *mechanism*
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
  PASS**. *Progress (2026-05-27):* second seam landed — **`Cpu`** (the broad
  one: maskable-IRQ enable/disable, halt, IF state). `kernel/src/arch/x86_64/
  cpu.rs` holds the `sti`/`cli`/`hlt`/RFLAGS mechanism (`#[inline]`); the
  IRQ-safe `SpinLock` (spin.rs, the hot path) calls `hal::cpu()` directly, and
  the `interrupts::{enable,disable,wait_for_interrupt,wait_for_interrupt_enabled,
  without_interrupts}` wrappers become the arch-agnostic facade over the seam so
  their many callers (main.rs, pcsched.rs idle loops, every Frame-dispatch
  critical section) are unchanged. PAUSE is *not* a HAL primitive —
  `core::hint::spin_loop()` already abstracts it. The `global_asm!` ISR stubs and
  the QEMU-exit `out 0xf4` are deliberately left for B-HAL.2 (IRQ path / Boot).
  Validated: both builds, clippy/fmt clean, **49/49 qemu-test smoke (all `smp_*`
  cross-core locking paths), console-test PASS**. *Progress (2026-05-28):* the
  two remaining isolated leaves landed — **`Clock`** (CMOS RTC →
  `arch/x86_64/rtc.rs`, `epoch_secs()`) and **`Fpu`** (SSE enable + fxsave/
  fxrstor → `arch/x86_64/fpu.rs`). `Fpu` is the first seam whose *type* is
  arch-specific: the 512-byte FXSAVE `FpuState` the scheduler embeds per-thread
  is re-exported as `hal::FpuState`, so sched.rs names it without naming the arch
  module. Both keep thin top-level facades (`rtc.rs`, `fpu.rs`) so their callers
  (the `time()` syscall; the scheduler's save/restore; `init_this_cpu`) are
  unchanged. Validated: both builds, clippy/fmt clean, 49/49 qemu-test smoke,
  console-test PASS (tcc exercises FPU + RTC; the job-control suite exercises FPU
  context-switch save/restore). *Progress (2026-05-28):* the load-bearing
  **`Mmu`** seam landed — the *full* paging API behind `hal::Mmu` (current/map/
  map_in/unmap/translate/new/fork/free/switch address space), not just the CPU
  primitives. All of paging.rs moved to `arch/x86_64/paging.rs` (the 4-level
  PML4 walk + CR3/invlpg) and the top-level module was retired; 8 caller files
  (vm, lapic, xhci, elf, sched, usermode, main) route through `hal::mmu()`. The
  key design call: an arch-neutral **`MapFlags`** (`WRITABLE`/`USER`/`DEVICE`)
  that the x86 impl translates to PTE bits — so lapic/xhci's raw MMIO cache bits
  (`PCD|PWT`) become `MapFlags::DEVICE` and no caller names an x86 page-table
  bit. The internal table walk keeps raw PTE bits (private `*_raw` helpers); only
  the trait boundary is neutral. Validated: both builds, clippy/fmt clean,
  **49/49 qemu-test smoke** (paging/page-fault/address-space/fork/exec/wait-reap/
  fpu-preempt/tlb-shootdown), **console-test PASS** (full fork/exec/exit
  lifecycle + xHCI/LAPIC MMIO + on-device tcc). *Progress (2026-05-28):*
  **`PerCpu`** landed — the per-core base register behind `hal::PerCpu`
  (`set_base` = wrmsr IA32_GS_BASE; `this_cpu_index` = the `gs:[0]` read).
  `arch/x86_64/percpu.rs` holds the MSR/GS mechanism; the top-level `percpu.rs`
  keeps the arch-agnostic per-CPU data blocks + field accessors and forwards the
  two primitives. One naming note: the `hal::PerCpu` *trait* and the per-core
  `PerCpu` *struct* share a name, so the trait is imported anonymously
  (`use crate::hal::PerCpu as _;`). Validated: both builds, clippy/fmt clean,
  49/49 qemu-test smoke (all `smp_*` paths exercise `this_cpu_index` on 4 cores),
  console-test PASS. The one remaining B-HAL.1 concern is **`Context`**
  (context.rs + the pcsched/sched switch asm); it's entangled with the IDT/ISR
  save path, so it pairs naturally with the IRQ/boot-path cluster (Irq, the
  IDT/ISR stubs, Timer, SyscallEntry, Boot) in B-HAL.2.
- **B-HAL.2 — Isolate boot + the IDT/IRQ path.** The hardest seam: factor the
  Limine handoff + IDT setup + the timer/syscall ISR entry behind `Boot` + `Irq` +
  `SyscallEntry` so the arch-agnostic kernel init is one sequence calling HAL
  hooks. Still x86-only; still green.
  *Survey finding + decision (2026-05-29):* a close read of `lapic.rs`/`pic.rs`/
  `pit.rs` showed `Irq` and `Timer` are **not** clean leaves like the B-HAL.1
  six — they're part of this interrupt-controller-and-boot core, for three
  reasons: (1) the LAPIC is one device doing *both* roles, sharing `LAPIC_BASE`
  + the reg helpers between the timer and eoi/IPI — it moves wholesale or not at
  all; (2) the EOI granularity (`lapic::eoi` vs `pic::eoi_master`/`eoi_slave`/
  `eoi_for`) is consumed by the ISR Rust halves in `interrupts.rs`, so a
  *portable* `Irq` trait (one GIC EOI on ARM) wants the ISR dispatch co-designed,
  not a 1:1 x86 wrapper; (3) `lapic::TIMER_VECTOR` / `pic::PIC1_OFFSET` are IDT
  vectors `interrupts.rs` uses to install handlers, tying Irq/Timer to the IDT
  setup. Extracting them on x86 alone would yield a leaky ~10-method x86-shaped
  trait — ceremony, not a portable seam. **Decision: pause B-HAL here.** The six
  clean traits (Console/Cpu/Clock/Fpu/Mmu/PerCpu) are done + pushed; Irq, Timer,
  `Context`, the IDT/ISR stubs, `SyscallEntry`, and `Boot` are deferred to this
  milestone, to be tackled **when an AArch64 target exists** (B-HAL.3) so the
  contracts are designed against real hardware — a GICv2/3, the ARM generic
  timer, the RPi/device-tree boot — instead of an x86-only guess. The HAL
  foundation (the `hal.rs` traits + `arch/x86_64/` layout) is the additive seam
  that makes that work a port, not a fork.
- **B-HAL.3 — AArch64 skeleton: boot + console + a halt loop.** New
  `arch/aarch64/` + `aarch64-unknown-none` target: direct (`-kernel`) boot, PL011
  console, then (later sub-steps) device-tree memory map, GIC + generic-timer
  stubs, MMU bring-up — enough to print the banner and halt under
  `qemu-system-aarch64 -M virt`. (The AArch64 B0.) Approach: **two arch entry
  points in one crate, converging over B-HAL.4/.5** — x86 keeps its monolithic
  Limine `kmain`; aarch64 gets its own minimal `_start`/`kmain` that uses the
  existing HAL traits where they exist. Toolchain confirmed available
  (2026-05-29): `aarch64-unknown-none` rustup target + host `qemu-system-aarch64`
  11.0 (the Mac runs the ARM guest directly; the docker image is x86-qemu only).
  Numbered sub-plan (M1–M4 style; each its own validated commit, x86 stays 49/49):
  - **B-HAL.3.0 — Build plumbing (x86 no-op).** Add `linker-aarch64.ld` (ELF
    `littleaarch64`, load at the `virt` base `0x4008_0000`, `ENTRY(_start)`);
    branch `build.rs` on `CARGO_CFG_TARGET_ARCH` to pick the linker script + skip
    the x86 user-program staging on aarch64; make `limine` an
    `[target.'cfg(target_arch = "x86_64")'.dependencies]` dep. Validate: x86
    builds + 49/49 unchanged; the aarch64 build now fails *only* on x86-coupled
    code (the gating list for 3.1).
  - **B-HAL.3.1 + 3.2 — cfg-split + PL011 `Console` + banner. DONE (2026-05-29).**
    `main.rs` cfg-split: the 3 arch-agnostic mods (`hal`/`arch`/`serial`) stay
    unconditional, every other mod + the Limine statics + the SMP statics +
    `ap_entry` + `kmain` + `halt_forever` gated to `cfg(target_arch = "x86_64")`;
    `limine` is a target-x86 dep, `extern crate alloc` x86-only, `hal.rs`'s
    Cpu/Clock/Fpu/Mmu/PerCpu accessors x86-only (aarch64 gains them in B-HAL.4).
    `arch/aarch64/`: `boot.rs` (`_start` — enable FP/SIMD at EL1 via CPACR_EL1,
    set SP, clear BSS, `bl kmain`), `serial.rs` (PL011 `hal::Console` impl). The
    aarch64 `kmain` prints the banner through the *same* `serial.rs` text layer.
    **Two real bare-metal bugs found + fixed** (as the journal predicted): the
    `aarch64-unknown-none` target emits NEON, so FP/SIMD must be enabled at EL1
    (the ARM analogue of x86 SSE-enable; ESR EC=0x7 trap without it); and the
    PL011 needed enabling in `init()`. Validated: **aarch64 boots under
    `qemu-system-aarch64 -M virt`, prints the banner via PL011, no faults**;
    x86 build (default + interactive) + clippy (both arches) + fmt clean;
    **x86 smoke 49/49** (the cfg-split left x86 byte-for-byte unaffected). The
    visible milestone: one kernel source, two ISAs.
  - **B-HAL.3.3 — Device tree + memory map. DONE (2026-05-29).**
    `arch/aarch64/fdt.rs`: a minimal big-endian FDT reader (`valid`/`total_size`/
    `find`/`memory_region`) that walks the struct block to the `/memory` node's
    `reg` and returns `(RAM base, size)`. `kmain` takes the DTB pointer (x0,
    preserved through `_start`) and prints the memory map — the AArch64 half of
    the `Boot` "give me a memory map" contract (x86 reads Limine's map). **Boot
    finding:** QEMU's bare `-kernel <ELF>` path on `-M virt` delivers the DTB
    *neither* in x0 (it's 0) *nor* auto-loaded into RAM — that setup is only done
    for the Linux Image protocol. So the kernel uses x0 if set (real hardware / a
    future Image boot) and otherwise **scans the RAM window for the FDT magic**;
    the run/harness places the DTB at a fixed address via QEMU `-device loader`
    (`qemu -M virt … -kernel <elf> -device loader,file=virt.dtb,addr=0x4400_0000`,
    DTB obtained once via `-machine dumpdtb`). Validated under
    `qemu-system-aarch64 -M virt`: parser reports **RAM base 0x4000_0000, size
    128 MiB** (matches the machine); x86 build + clippy (both arches) + fmt clean;
    no faults. (Frame-allocator wiring deferred to pair with the MMU in 3.4; the
    FDT parser is the natural first `@@fsm` target when that's folded in.)
  - **B-HAL.3 harness — `cargo xtask qemu-aarch64`. DONE (2026-05-29).** The ARM
    analogue of the x86 `qemu-test` smoke: builds the aarch64 kernel, dumps the
    `virt` DTB (`-machine virt,dumpdtb=…`), boots `qemu-system-aarch64 -M virt`
    with the DTB at `0x4400_0000` via `-device loader`, captures serial on a
    reader thread, and asserts the banner + `PL011 console up` + `RAM base
    0x…40000000` (then kills QEMU — the kernel parks). Runs **on the host** (the
    dev container ships only x86 QEMU). So the aarch64 boot is now regression-
    tested the same way x86's 49/49 is.
  - **B-HAL.3.4 — MMU bring-up. DONE (2026-05-29).** `arch/aarch64/mmu.rs`:
    stand up an identity map (VA==PA) — one L1 table of 1 GiB block descriptors,
    39-bit VA (T0SZ=25, 4 KiB granule): `[0,1 GiB)` Device-nGnRnE (flash/GIC/
    PL011), `[1,2 GiB)` Normal-WB cacheable (RAM) — program MAIR/TCR/TTBR0,
    `tlbi`, then set `SCTLR_EL1.M`. Because the map is identity, the PC, SP,
    PL011, and DTB keep their addresses across the M=0→1 transition, so the
    console must keep working immediately after — which the smoke check asserts
    (`MMU enabled` reaches the PL011 *post*-enable). Scope: this is the MMU
    *mechanism*; the full `hal::Mmu` trait impl (map/unmap + address-space
    lifecycle, behind `MapFlags`) needs the frame allocator + a process model and
    lands with B-HAL.4/.5 — this is the substrate they sit on. Validated:
    `cargo xtask qemu-aarch64` PASS (now also asserts `MMU enabled`); x86 build +
    clippy (both arches) + fmt clean.
  - **B-HAL.3.5 — GIC + generic-timer stubs.** Minimal GICv2/3 init + `CNTP_*`
    enough to exist — establishing the `Irq`/`Timer` trait shapes against real
    ARM hardware (the contracts the x86 side is then re-fitted to).
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

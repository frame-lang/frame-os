# Portability

Frame OS has two distinct portability stories that often get confused. This document covers both:

1. **Source-language portability** — Rust now, with a C port preserved as a future option.
2. **Host-platform portability** — Linux, macOS, and Windows as build hosts; multiple architectures as runtime targets.

The two are related (some decisions affect both) but the goals differ enough that they deserve separate treatment.

## Part 1: Rust-first, C-port-later

Frame OS is implemented in Rust today. A future port to C is preserved as a viable option through specific design rules, even though no C implementation is being actively maintained.

### Why care about a C port at all

Rust is the right language for the project today. The reasons are concrete and were discussed at length when this project was scoped: Frame's state-enum representation lights up under Rust's `match` exhaustiveness, `no_std` is a real designed-in feature whereas "freestanding C" is folklore, the build pipeline is one command, and the `unsafe` boundary makes the Frame-vs-native-code split visually obvious.

Why preserve the C port as a future possibility? Two reasons:

**Certification credibility.** If Frame OS ever heads toward a certified-RTOS direction (safety-critical embedded, medical devices, automotive), the qualified compiler ecosystem is dominantly C-based. Wind River Diab, Green Hills MULTI, IAR — these are the toolchains certification bodies are most familiar with. Rust certification is improving (Ferrocene exists) but still less established. Having a clean C port path keeps that door open.

**The Frame argument for C is genuinely interesting.** C is the worst case for state-machine-implicit OS code — large legacy kernels with sprawling integer `state` fields and conditional logic everywhere. If Frame helps make state machines explicit in C, that's a more dramatic improvement than in Rust (where Rust's type system already enforces some of what Frame enforces). A C version of Frame OS would be a stronger argument for Frame than a Rust version, in the same way that a person quitting smoking is more impressive than a person who never started.

### Design rules to preserve the C port

These are constraints on how Rust code in Frame OS is written. They're not optimal Rust idioms; they're Rust written with one eye on translation to C.

**Avoid trait-based polymorphism in hot paths.** Use enum-and-`match` instead. The C port has no traits; enum dispatch translates cleanly to C's tagged-union pattern with `switch`. Trait objects (`Box<dyn Trait>`) translate to function pointer tables, which is workable but adds friction; static traits (generic parameters with trait bounds) translate to multiple specialized C functions, which scales poorly.

**Avoid `Drop` for resource cleanup in kernel paths.** Use explicit `cleanup()` calls. C has no destructors. Code that depends on RAII for correctness either gets duplicated logic in the C port or breaks subtly. This is also a defense against panics-in-drop in the Rust version, which are their own source of trouble.

**Prefer simple, manually-sized collections over `Vec<T>` and `String`.** Use `[T; N]`, `heapless::Vec`, and fixed-size arrays where possible. Dynamic allocation in kernel code is risky in any language; preferring static sizing makes the C port mechanical rather than requiring an allocator strategy decision.

**Lifetimes are an implementation detail, not an architectural feature.** If a Frame OS subsystem only works because of a clever lifetime gymnastics in Rust, that's a signal the design is too clever. The C port would have to encode the same invariant manually, which usually means it doesn't get encoded at all. Aim for designs where memory ownership is plain: a resource has one owner, the owner is identifiable in the type system, transfers happen explicitly.

**Generics are fine but use them sparingly.** Frame OS doesn't need extensive generic programming; most of its code is concrete types with specific behavior. Generic types that are used in two or three places translate fine to C with hand-written specializations. Generic types used in thirty places become a porting headache.

**Match operator overloading carefully.** C has no operator overloading; Rust code that relies on `+`, `-`, `==` doing non-trivial things for kernel types won't translate. Stick to operators on primitive types and write methods (`a.add(b)`) for non-trivial cases.

**The framepiler does the work for Frame systems.** Frame source compiles to both Rust and C; that's a framepiler property, not something the project has to engineer. The portability rules above apply to the *native Rust code around the Frame systems*, not to the Frame systems themselves.

### What we're explicitly *not* doing

Three things that might seem like they'd help the C port but actually wouldn't:

**Not writing Rust that looks like C.** Idiomatic Rust is fine; the constraints above are about avoiding *specific* features that have no C equivalent. Within those constraints, we write Rust as Rust developers do — `match` on enums, iterators where they fit, explicit error handling with `Result<>` where it doesn't fight the Frame model.

**Not maintaining two implementations in parallel.** The C port is a future artifact, not a current one. Trying to keep two implementations in sync would double the maintenance burden and slow the project to a crawl. The rules above keep the *path* to a C port viable; actually walking that path is a separate project.

**Not avoiding the standard library in places it makes sense.** The hosted-mode shell uses `std::process::Command`, `std::fs`, etc. The C port of the hosted shell would use POSIX equivalents (`fork`+`exec`, `open`/`read`/`write`). These map cleanly; we don't need to anticipate the port by using more primitive Rust APIs.

### Where the C port would diverge

Even with careful design, some parts of the C port will look different from the Rust version:

- Error handling. Rust's `Result<T, E>` becomes C's "return int errno or zero, write result through pointer." The shape is different even when the semantics match.
- Iteration. Rust iterators become explicit loops in C, sometimes with helper macros.
- Memory ownership. Rust's borrow checker proves things at compile time; in C, the same invariants are documented in comments and enforced by code review.
- Some Frame language features. The framepiler's C target doesn't support `async`, doesn't support inheritance (which we're not using anyway), and represents call-scoped data (`@@:data.key`) using a more constrained data structure than the Rust target. Persistence (`@@[persist]`) also has per-target nuances — the blob type, save/load method names, and serialization library differ. Frame OS systems are designed to avoid `@@:data` and `@@[persist]` in kernel paths, so these per-target differences don't bite us; the C port inherits the same restraint.

These divergences are expected. The goal of the portability rules isn't to make the Rust and C versions look identical; it's to make the C port a tractable engineering task rather than a rewrite.

## Part 2: Multi-host and multi-architecture support

Frame OS supports three build host operating systems and multiple runtime architectures. The matrix is:

|                  | Linux x64 | Linux ARM | macOS Intel | macOS AS | Windows + WSL2 | Windows native |
|------------------|:---------:|:---------:|:-----------:|:--------:|:--------------:|:--------------:|
| Build hosted shell| ✓         | ✓         | ✓           | ✓        | ✓              | ✓              |
| Run hosted shell  | ✓         | ✓         | ✓           | ✓        | ✓              | ✓              |
| Build kernel for QEMU | ✓     | ✓         | ✓           | ✓        | ✓              | ✗ (use WSL2)   |
| Run kernel in QEMU| ✓         | ✓         | ✓           | ✓        | ✓              | ✓ (with QEMU)  |
| Build for Pico    | ✓         | ✓         | ✓           | ✓        | ✓              | ✓              |
| Build for Pi 4    | ✓         | ✓         | ✓           | ✓        | ✓              | ✓              |

### Linux: the reference platform

Linux x86_64 is the canonical development environment. CI runs against it. Documentation defaults to Linux command examples. When something works everywhere else but not on Linux, that's a bug we'll fix; when it works on Linux but not elsewhere, that's a portability issue we'll investigate but may declare out of scope.

Prerequisites: `rustup` with the bare-metal targets installed (`x86_64-unknown-none`, `aarch64-unknown-none`, `thumbv6m-none-eabi`), QEMU (`qemu-system-x86`, `qemu-system-arm`), the framepiler binary. All installable via standard package managers.

### macOS: first-class

macOS as a development host works well, with a few specific footnotes:

**Apple Silicon vs. Intel matters.** On Apple Silicon, QEMU emulating x86_64 uses TCG (software emulation) and is slow — maybe 50ms boot times instead of 5ms. QEMU emulating AArch64 uses Hypervisor.framework via the `hvf` accelerator and runs at near-native speed. On Intel Macs, the opposite is true. Plan demonstrations and CI accordingly.

**No GRUB on macOS as a host.** GRUB doesn't build natively. This is the proximate reason we chose Limine — it builds on Mac without ceremony, so the build pipeline doesn't fork by host.

**`brew install qemu` works.** Pico flashing via `elf2uf2-rs` works. The Rust toolchain works. There are no Mac-specific gotchas beyond the QEMU speed asymmetry.

**The hosted shell runs at full native speed on Apple Silicon.** This might actually be the project's most accessible demo artifact — single binary, no QEMU required, opens in any terminal, instant startup.

### Windows: WSL2 first-class, native best-effort

Windows is the most awkward of the three hosts. Being honest about this beats overpromising.

**WSL2 is the recommended path.** WSL2 gives you a real Linux environment running on Windows hardware. From Frame OS's perspective, WSL2 is Linux. The build instructions for Linux apply unchanged. Most Windows developers doing systems work already use WSL2 anyway.

**Native Windows works for the hosted shell.** `cargo run` on PowerShell produces a working Frame OS shell. The line editor (`rustyline`) supports Windows. External command execution works (with appropriate path adjustments — `cmd.exe`, not `bash`).

**Native Windows for bare-metal builds is more limited.** The cross-compilation toolchain works. QEMU works (downloadable from qemu.org for Windows). But several auxiliary tools historically used in OS development workflows — bootable ISO construction with `xorriso`, certain build-system patterns — assume a Unix environment. We've kept the canonical build path inside Cargo and `xtask` specifically to minimize this dependency, but expect occasional friction when interacting with the broader Rust/OS-dev ecosystem.

**Practical recommendation in docs:** "If you're on Windows and don't already have WSL2, install it. The bare-metal track will be much smoother." This is honest, accurate, and matches what most Windows developers in this space do anyway.

### Architecture targets

**QEMU x86_64** is the primary development target for bare-metal Frame OS. Reasons: fastest iteration loop, most mature toolchain support, most googleable error messages when something goes wrong. The kernel is developed against x86_64 first; other targets are ports.

**Raspberry Pi Pico** (RP2040, Cortex-M0+) is the microcontroller-class target. No MMU, 264KB of RAM, programs flashed into flash. This is a Tier-1-only target — no user-mode isolation, no ELF loading, no virtual memory. The Frame OS Pico variant is essentially a small RTOS with statically-defined tasks. Different deliverable, same Frame systems for the parts that overlap (scheduler, drivers, tasks).

**Raspberry Pi 4/5** (Cortex-A72/A76, AArch64) is the application-processor target. Real MMU, gigabytes of RAM, capable of running the full Tier-3 kernel. This is the most ambitious physical-hardware target.

**Real Mac hardware is not a runtime target.** Apple Silicon boot reverse engineering is Asahi-Linux-scale work — multi-year, requires deep expertise in undocumented Apple boot protocols. Out of scope. Intel Macs (pre-2020) could theoretically boot Frame OS via EFI, but their installed base is shrinking each year and the demo value of "Frame OS boots on a Mac" is modest enough that we're not investing in it.

**SMP and multi-core are out of scope for now.** All bare-metal targets run as single-core. Adding SMP support is a significant undertaking and isn't necessary for the project's central argument about state-machine-explicit OS design.

### What runs where

To make the architecture/target picture concrete:

| Target | Tier supported | Status | Primary purpose |
|--------|----------------|--------|-----------------|
| Hosted shell on Linux | n/a | Planned for H0-H3 | Reference development environment |
| Hosted shell on macOS | n/a | Planned for H0-H3 | Mac developer accessibility |
| Hosted shell on Windows | n/a | Planned for H0-H3 | Windows developer accessibility |
| Bare-metal in QEMU x86_64 | Tier 1, 2, 3 | Planned for B0-B4 | Primary kernel development |
| Bare-metal in QEMU AArch64 | Tier 1, 2, 3 | Future port after B3 | Mac developer dogfood |
| Pi Pico | Tier 1 only | Future port after B1 | Microcontroller demonstration |
| Pi 4/5 | Tier 1, 2, 3 | Future port after B3 | Real hardware demonstration |

The "Future port" entries are deliberately not committed milestones. They become real work after the primary track reaches the corresponding stage.

## How the two portability stories interact

A few design decisions are driven by *both* stories simultaneously, and it's worth being explicit about which constraints they're serving:

**No shell scripts in the canonical build path.** Driven by Windows-native support. Also helps the C port, since a C port would presumably want the same `cargo xtask` orchestration available rather than rewriting the build system.

**Avoid Rust `Drop` for kernel resources.** Driven by the C port constraint. Also has a Rust-only benefit: explicit cleanup is easier to audit in panic-prone unsafe code than relying on drop ordering.

**No platform-specific code in Frame systems.** Driven by all of the above — the Frame systems should be portable both across host OSes (relevant for the hosted shell) and across runtime architectures (relevant for the kernel). Host-specific or architecture-specific code lives in native annexes that surround the Frame systems.

**Limine as bootloader.** Driven by macOS host support. Also a Rust/C portability win because Limine's protocol is much simpler than multiboot2, so a C port wouldn't need to reimplement complex boot-parameter parsing.

These intersections aren't coincidence — they reflect a general principle that good portability decisions tend to align with good architectural decisions. If a choice serves portability *and* clarity *and* simplicity simultaneously, that's a sign it's the right choice for reasons beyond just the immediate need.

# Frame OS

A small operating system organized around explicit state machines, built to showcase and validate the [Frame](https://github.com/frame-lang/framepiler) language for systems work.

Frame OS is not trying to replace Linux. It's trying to demonstrate, in working code, what an OS looks like when its lifecycle-shaped components — schedulers, process state, syscall dispatch, drivers, parsers — are written as explicit state machines instead of as `switch` statements over an integer `state` field. Same problems, more visible structure.

## What this project is

Frame OS ships in two flavors that share most of their source:

**Hosted mode** — Frame OS as a state-machine shell that runs as a normal application on Linux, macOS, and Windows. `cargo run` launches it. It looks like a small Unix shell, parses commands, runs builtins and external programs, handles signals. The interesting part isn't what it does; it's that every piece of behavior is a Frame state machine whose graph you can render with `framec -l graphviz`.

**Bare-metal mode** — Frame OS as a real kernel that boots in QEMU and on real hardware (Pi Pico, Pi 4/5). It manages tasks, drives a serial console, dispatches syscalls, and loads programs. The Frame systems describe the kernel's control flow; native Rust handles the unsafe primitives (page tables, context switches, register pokes).

Same `.frs` source files compile to both. The state machines for `Shell` and `Parser` are reused between modes (with different action implementations — `std::process::Command` vs. the kernel's task interface). Hosted-only systems include `JobControl` and `Job`. Bare-metal-only systems include `Kernel`, `Scheduler`, `Task` / `Process`, `ProcessTable`, `SyscallDispatcher`, `ElfLoader`, and the drivers.

## Why state machines

Every OS already has state machines in it; they're just not visible in the code. A Linux task's `task->state` is an int. A TCP connection's state lives across hundreds of lines of conditional logic. Writing this stuff explicitly as state machines — with the events, states, transitions, and forwarded errors all named — does three concrete things:

1. **Adding a state is a localized change.** A new `$Suspended` state on `Process` is a state declaration and a few transitions, not a hunt-and-peck through every file that touches process state.
2. **Exhaustiveness is enforced by the compiler.** With the Rust target, the framepiler emits `match` on a state enum. Adding a state causes compile errors everywhere the new variant isn't handled. The bug class "I added a state but forgot to handle it in the scheduler" disappears.
3. **The diagram is real documentation.** `framec -l graphviz scheduler.frs | dot -Tsvg` produces an authoritative diagram of what the code actually does. It can't drift from the implementation; it *is* the implementation.

Frame OS exists to make this case in code rather than in slides.

## Quick start

```bash
# Clone
git clone <repo>
cd frame-os

# Install framec (Frame's own toolchain — required prerequisite)
cargo install framec

# Install Rust bare-metal targets and any other host tools
cargo xtask install-tools

# Build the hosted-mode shell
cargo build --bin frame-os-shell

# Run hosted-mode Frame OS
cargo run --bin frame-os-shell

# Run bare-metal Frame OS in QEMU
cargo xtask qemu

# Flash bare-metal Frame OS to a Pi Pico
cargo xtask pico-flash --port /dev/ttyACM0
```

`framec` must be installed before any `cargo build` step that touches Frame source — the build scripts shell out to it. `cargo xtask install-tools` handles the rest (Rust targets, optional QEMU on systems where it's straightforward to install).

**Dev container (recommended for bare-metal work).** The bare-metal track builds and runs inside a Linux container — source stays on the host, builds/tests/QEMU run in Docker. This gives dev/CI parity and a working Linux TAP path for the networking tests (which macOS can't provide). See [`docker/README.md`](docker/README.md):

```sh
docker build -t frameos-dev docker/
docker/run.sh "cargo xtask qemu-test"
```

See [`docs/roadmap.md`](docs/roadmap.md) for the milestone-by-milestone plan and what's actually working today.

## Supported platforms

### Build hosts

| Host | Status | Notes |
|------|--------|-------|
| Linux x86_64 | First-class | Canonical reference; CI runs here |
| Linux aarch64 | First-class | Apple Silicon Linux, ARM dev boards |
| macOS aarch64 (Apple Silicon) | First-class | QEMU via Hypervisor.framework |
| macOS x86_64 (Intel) | First-class | Slower QEMU but functional |
| Windows + WSL2 | First-class | WSL2 environment is effectively Linux |
| Windows native (PowerShell) | Best-effort | The hosted-mode shell works; bare-metal builds require WSL2 |

### Runtime targets

**Hosted-mode runtime:** any platform where you can `cargo run` — same as the build host list above. The Frame OS shell is a single executable.

**Bare-metal runtime:** QEMU x86_64 (primary development target), Raspberry Pi Pico (Tier-1 microcontroller variant), Raspberry Pi 4/5 (Tier-3 application-processor target). Real Mac hardware is *not* a bare-metal runtime target — Apple Silicon boot reverse engineering is out of scope. See [`docs/portability.md`](docs/portability.md) for details on each.

## Project structure

```
frame-os/
├── README.md           — this file
├── LICENSE-MIT
├── LICENSE-APACHE
├── docs/
│   ├── vision.md       — what this is for, why it exists, success criteria
│   ├── architecture.md — system design, Frame vs native Rust split, module breakdown
│   ├── portability.md  — Rust-first / C-port-later design rules, multi-host story
│   ├── roadmap.md      — milestone breakdown for both tracks
│   ├── testing.md      — testing approach and conventions across all levels
│   └── systems/        — per-system reference docs (one per Frame system)
│       ├── README.md   — index of all systems
│       └── _template.md — required structure for new per-system docs
├── frame/              — Frame source files (.frs)
├── kernel/             — bare-metal kernel crate
├── shell/              — hosted-mode shell crate
├── shared/             — Frame-generated code shared between kernel and shell
└── xtask/              — cross-platform build orchestration
```

## License

Dual-licensed under Apache 2.0 OR MIT, at your option. See `LICENSE-APACHE` and `LICENSE-MIT`.

This is the standard licensing convention in the Rust ecosystem. Pick whichever applies to your use case.

## Status

Pre-alpha. The repository is in active design. The doc set in `docs/` describes the intended architecture; the code is being written against it incrementally. See `docs/roadmap.md` for what's done and what's next.

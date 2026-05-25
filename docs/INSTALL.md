# Installing & testing Frame OS

Frame OS has two build tracks with different platform stories:

- **Hosted mode** (`frame-os-shell`) — a normal Rust application. Builds, runs,
  and is **CI-verified on Linux, macOS, and Windows**.
- **Bare-metal mode** (`frame-os-kernel`) — an x86-64 UEFI kernel that boots in
  QEMU. Built and smoke-tested **on Linux** (natively in CI; via the dev
  container on macOS/Windows). There is no real-hardware / Raspberry Pi path yet
  — those are planned future ports (see [`portability.md`](portability.md)).

Everything below mirrors what CI actually runs (`.github/workflows/ci.yml`), so
a green local run matches a green CI run.

## Prerequisites (all platforms)

1. **Rust** (stable) with `clippy` + `rustfmt`:
   ```sh
   rustup toolchain install stable --component clippy rustfmt
   ```
2. **framec** — Frame's transpiler. Every `cargo build` that touches a `.frs`
   shells out to it, so it must be on `PATH` first:
   ```sh
   cargo install framec
   ```
3. **Bare-metal targets** (only needed for the kernel) — `cargo xtask
   install-tools` adds the `x86_64-unknown-none` target (plus the AArch64 /
   thumb targets reserved for the planned Pi ports). It does **not** install
   QEMU/OVMF/GraphViz — use your package manager for those (below).

## Hosted mode — Linux, macOS, Windows

Verified on all three in CI. From the repo root:

```sh
cargo build  --bin frame-os-shell     # build
cargo run    --bin frame-os-shell     # interactive shell
```

The full hosted check (what CI's `test` job runs on each OS):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --exclude frame-os-kernel --all-targets -- -D warnings
cargo build  --workspace --exclude frame-os-kernel
cargo test   --workspace --exclude frame-os-kernel
```

(`--exclude frame-os-kernel` because the kernel is bare-metal-only — it can't
build for the host; see the kernel track below.)

## Bare-metal mode

The kernel targets **x86-64 under UEFI** and runs in QEMU.

### Linux (native — the primary path)

System packages (Debian/Ubuntu names; CI installs exactly these):

```sh
sudo apt-get install qemu-system-x86 ovmf mtools   # QEMU + UEFI firmware + FAT tooling
sudo apt-get install graphviz                       # only for diagram regeneration
```

Then:

```sh
cargo build -p frame-os-kernel --target x86_64-unknown-none   # cross-build
cargo xtask qemu                                              # boot the DEMO kernel (runs B0–B7 self-tests, halts)
cargo xtask qemu-interactive                                  # boot to the `frameos$` shell and type commands yourself
cargo xtask qemu-test                                         # headless boot + the full smoke suite (CI)
cargo xtask check-diagrams                                    # state-graph drift check (needs graphviz)
```

**Driving the OS by hand.** `cargo xtask qemu-interactive` drops you at the
`frameos$` prompt (Ctrl-A x quits QEMU). Try:

```
/bin/hello                                    # a Rust ELF
/bin/tcc -v                                   # the on-device C compiler
tcc -B/usr/lib/tcc -static /hello.c -o /out.elf  &&  /out.elf   # compile + run C on-device
buildc /hi.c                                  # compile→link→run via the BuildDriver FSM
/bin/fhello                                   # the V1.0 capstone: one Frame system → Rust
buildc /fhello.c                              # the same Frame system → C, built by the on-device tcc
exit
```

On first run, `xtask` fetches the pinned Limine bootloader binaries into
`target/limine` (cached afterward). OVMF firmware is auto-discovered across the
common locations, including Ubuntu 24.04's `OVMF_CODE_4M.fd` layout.

### macOS and Windows (via the dev container)

macOS can't provide the Linux TAP device the networking tests need, and the
OVMF firmware paths aren't wired for native macOS/Windows QEMU — so the
bare-metal track runs inside a **Linux dev container** (dev/CI parity, pinned
framec baked in). See [`../docker/README.md`](../docker/README.md).

```sh
docker build -t frameos-dev docker/          # one-time
docker/run.sh "cargo xtask qemu-test"        # build + smoke suite in the container
docker/run.sh "cargo build -p frame-os-kernel --target x86_64-unknown-none"
docker/run.sh shell                          # interactive shell in the container
TAP=1 docker/run.sh "cargo xtask qemu-tap"   # inbound-L2 networking tests (needs NET_ADMIN)
```

Source stays on the host (bind-mounted at `/work`); build artifacts live in a
named volume, so the container's `target/` never clobbers a host-native one.

To **drive the shell interactively** from macOS/Windows you need a TTY, so use
the interactive container form (`docker/run.sh shell` is `-it`) and run the
command inside:

```sh
docker/run.sh shell                  # interactive container
# then, at the container prompt:
cargo xtask qemu-interactive         # boots to the frameos$ prompt; type away
```

## Platform support at a glance

| | Hosted build/run | Hosted tests | Kernel cross-build | QEMU smoke suite |
|---|---|---|---|---|
| **Linux** | native | native (CI) | native (CI) | native (CI) |
| **macOS** | native (CI) | native (CI) | native | via dev container |
| **Windows** | native (CI) | native (CI) | native | via WSL2 / dev container |

"CI" = exercised by `.github/workflows/ci.yml` on every push.

## Troubleshooting

- **`framec: command not found` during a build.** Install it first
  (`cargo install framec`); the `.frs` build scripts require it on `PATH`.
- **`could not locate OVMF UEFI firmware`.** Install the `ovmf` package
  (Ubuntu/Debian) or `edk2-ovmf` (Fedora/Arch); on macOS use the dev container.
- **QEMU/networking on macOS or Windows.** Use the dev container — native
  macOS/Windows bare-metal isn't a supported path.

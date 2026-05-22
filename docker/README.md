# Dev container

Frame OS develops on macOS but **builds and runs inside a Linux container**. The
source lives on the Mac (edited normally, committed normally); the container
provides the Linux toolchain + QEMU. This gives:

- **dev/CI parity** — CI is Linux, and now so is the local build/test environment;
- **a working TAP path** — Linux TAP (`/dev/net/tun`) for the inbound-L2 networking
  tests, which macOS can't provide without a kext or root `vmnet`;
- **the typed-context framec** — built from source in the container (it isn't on
  crates.io yet), so the kernel's struct enter params compile.

## One-time

```sh
docker build -t frameos-dev docker/
```

## Run anything

`docker/run.sh "<command>"` runs the command in the container with the Mac repo
bind-mounted at `/work` and build artifacts in named volumes (so they neither
clobber the Mac's macOS-built `target/` nor pay the bind-mount IO penalty):

```sh
docker/run.sh "cargo build -p frame-os-kernel --target x86_64-unknown-none"
docker/run.sh "cargo clippy --workspace --exclude frame-os-kernel -- -D warnings"
docker/run.sh "cargo test -p frame-os-kernel-tests"
docker/run.sh "cargo xtask qemu-test"
docker/run.sh "FRAMEOS_SMOKE_FILTER=tcp_ cargo xtask qemu-test"   # one group
docker/run.sh shell                                               # interactive
```

Each invocation first (re)builds framec from the bind-mounted source
(`$FRAMEC_SRC`, default `~/projects/framec`) into a cached volume and puts it on
PATH — incremental, so it's fast after the first run.

## Mounts / volumes

| Mount | Purpose |
|---|---|
| `~/projects/frame-os` → `/work` | the repo (source edited on the Mac) |
| `~/projects/framec` → `/framec` (ro) | framec source (built to the binary) |
| `frameos-target` (volume) → `/target` | `CARGO_TARGET_DIR` (kept off the bind mount) |
| `frameos-framec-target` (volume) | framec's build cache |
| `frameos-cargo-registry` (volume) | the cargo registry cache |

Override the framec location with `FRAMEC_SRC=/path docker/run.sh …`.

## TAP / inbound networking

The networking tests that need a real inbound L2 peer (answering `ping`, IP
reassembly) require a TAP device + `NET_ADMIN`. Set `TAP=1`:

```sh
TAP=1 docker/run.sh "cargo xtask qemu-tap"   # B5-3: ping the guest over tap0
```

which adds `--cap-add=NET_ADMIN --device /dev/net/tun`. (Without it, the
container has no TAP and the build/QEMU-slirp tests run unprivileged as usual.)

`cargo xtask qemu-tap` brings up `tap0` (host side `10.0.2.1/24`), boots the
kernel with `-netdev tap`, `ping`s the guest at `10.0.2.15`, and asserts both a
reply and `[icmp] answered ping` in the serial capture — i.e. the *guest's*
inbound ARP + ICMP responders answered a real L2 peer (which slirp can't be).

## Notes

- QEMU runs under TCG (we never use KVM), so it works in the container the same
  way it did on the Mac — same performance class (x86 emulated on arm64).
- The Mac's `target/` (macOS artifacts) is now stale and unused; the container
  builds into the `frameos-target` volume instead.

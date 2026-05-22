#!/usr/bin/env bash
# Run a command inside the Frame OS dev container against the Mac-side source.
#
#   docker/run.sh "cargo build -p frame-os-kernel --target x86_64-unknown-none"
#   docker/run.sh "cargo xtask qemu-test"
#   docker/run.sh shell        # interactive shell
#
# Source lives on the Mac (bind-mounted at /work); build artifacts live in
# named volumes (fast, and they don't clobber the Mac's macOS-built target/).
# Set TAP=1 to grant the networking caps for the TAP/inbound-L2 tests.
#
# (No `set -u`: macOS ships bash 3.2, where expanding an empty array under
# `set -u` is a hard error.)
set -eo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FRAMEC_SRC="${FRAMEC_SRC:-$HOME/projects/framec}"
IMAGE="${FRAMEOS_IMAGE:-frameos-dev}"

mounts=(
    -v "$ROOT:/work"
    -v "$FRAMEC_SRC:/framec:ro"
    -v frameos-target:/target
    -v frameos-framec-target:/framec-target
    -v frameos-cargo-registry:/usr/local/cargo/registry
    -e CARGO_TARGET_DIR=/target
    -w /work
)

# The networking tests need a TAP device + NET_ADMIN.
caps=()
if [ "${TAP:-0}" = "1" ]; then
    caps=(--cap-add=NET_ADMIN --device /dev/net/tun)
fi

if [ "${1:-}" = "shell" ]; then
    exec docker run --rm -it "${mounts[@]}" "${caps[@]}" "$IMAGE" \
        bash -lc 'ensure-framec >/dev/null 2>&1 || ensure-framec; exec bash'
fi

# Build framec (cached) then run the requested command.
exec docker run --rm "${mounts[@]}" "${caps[@]}" "$IMAGE" \
    bash -lc "ensure-framec >/dev/null && $*"

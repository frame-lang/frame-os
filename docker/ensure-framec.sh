#!/usr/bin/env bash
# Build framec from the bind-mounted source (/framec) into a cached target dir
# and install it on PATH. The typed-context framec isn't on crates.io yet, so
# the dev container builds it from source; the /framec-target volume caches the
# build so this is fast (incremental) after the first run.
set -euo pipefail

if [ ! -d /framec ]; then
    echo "ensure-framec: /framec not mounted (bind the framec source repo)" >&2
    exit 1
fi

cargo build --release --manifest-path /framec/Cargo.toml --target-dir /framec-target
install -m 0755 /framec-target/release/framec /usr/local/bin/framec

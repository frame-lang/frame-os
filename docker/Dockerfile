# Frame OS dev container.
#
# The Mac is macOS/arm64, where Linux TAP networking and parts of the toolchain
# are awkward; we develop the *source* on the Mac (bind-mounted in) and run all
# builds/tests/QEMU inside this Linux container. This also gives dev/CI parity
# (CI is Linux) and makes a real TAP device available for the inbound-L2 tests
# (run with --cap-add=NET_ADMIN --device /dev/net/tun).
#
# framec is pinned to a published crates.io release (4.2.1) and baked into the
# image. frame-os does NOT depend on a local framec/transpiler checkout — builds
# are reproducible and unaffected by work-in-progress in any framec source tree.

FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

# Toolchain: C build deps (for framec/native crates), QEMU + UEFI firmware +
# FAT tools (the ESP image), GraphViz (diagrams), and iproute2/ping for the TAP
# networking tests.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential curl ca-certificates git pkg-config libssl-dev \
        qemu-system-x86 ovmf mtools dosfstools graphviz \
        iproute2 iputils-ping \
    && rm -rf /var/lib/apt/lists/*

# Rust (stable) + the bare-metal target + lint/format components.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal \
    && rustup component add clippy rustfmt \
    && rustup target add x86_64-unknown-none

# Pin framec to the 4.2.1 crates.io release. Baked in once at image build time,
# so frame-os builds invoke a fixed `framec` binary and never recompile the
# transpiler from a (possibly in-flux) local source tree.
RUN cargo install framec --version 4.2.1 --locked

WORKDIR /work

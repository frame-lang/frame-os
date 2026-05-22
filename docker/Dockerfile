# Frame OS dev container.
#
# The Mac is macOS/arm64, where Linux TAP networking and parts of the toolchain
# are awkward; we develop the *source* on the Mac (bind-mounted in) and run all
# builds/tests/QEMU inside this Linux container. This also gives dev/CI parity
# (CI is Linux) and makes a real TAP device available for the inbound-L2 tests
# (run with --cap-add=NET_ADMIN --device /dev/net/tun).
#
# framec is NOT baked in: the typed-context build isn't on crates.io yet, so it
# is built from the bind-mounted framec source at run time (see ensure-framec).

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

COPY ensure-framec.sh /usr/local/bin/ensure-framec
RUN chmod +x /usr/local/bin/ensure-framec

WORKDIR /work

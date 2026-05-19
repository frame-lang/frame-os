# Contributing to Frame OS

Frame OS is a small project with a focused purpose: demonstrate that an OS organized around explicit state machines is clearer, more auditable, and more maintainable than the same OS written conventionally. Contributions are welcome, with a few specifics.

## Read first

Before opening an issue or PR, please read:

- [`docs/vision.md`](docs/vision.md) — what the project is for, what's out of scope
- [`docs/architecture.md`](docs/architecture.md) — how the code is organized
- [`docs/roadmap.md`](docs/roadmap.md) — the milestone we're working toward
- [`docs/testing.md`](docs/testing.md) — what test coverage is expected for a change

If you're proposing a new Frame system, also read [`docs/systems/_template.md`](docs/systems/_template.md) — the structure each per-system doc follows.

## Local setup

```bash
# Install framec (the Frame transpiler)
cargo install framec

# Install other required tools and Rust targets
cargo xtask install-tools

# Build and test
cargo test --workspace
```

QEMU and GraphViz are needed for some tests; install them via your package manager (`brew install qemu graphviz`, `apt install qemu-system graphviz`, etc.). The QEMU smoke tests don't run yet (B0 hasn't landed); the diagram check requires GraphViz once any `.svg` is committed.

## What a good PR looks like

For changes to Frame systems:
- Update the `.frs` source in `frame/`
- Update or write the per-system doc in `docs/systems/<system>.md`
- Run `cargo xtask regen-diagrams` and commit the updated `.svg`
- Update or add tests; the per-system doc's Testing section must reflect reality
- All `cargo test --workspace`, `cargo clippy`, and `cargo fmt -- --check` pass

For changes to native code:
- Standard Rust conventions
- `cargo test` covers the change
- If the change is in the kernel, consider whether the QEMU smoke test set needs an addition

For documentation-only changes: just the doc edit and a clear PR message.

## Tone and scope

The project values honest scoping. If a change is hard or partial, the PR description says so. We'd rather ship a small, well-documented thing than a large speculative one.

If you're unsure whether a contribution fits, open an issue first to discuss.

## License

By contributing, you agree your contribution is licensed under both MIT and Apache 2.0 (the project's dual license).

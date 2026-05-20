# Instructions for Claude

This file orients Claude Code (and any future Claude session) to the project's conventions, build process, and quality expectations. Read this before making changes.

## What this project is

Frame OS is a small operating system organized around explicit state machines, built to showcase the [Frame](https://github.com/frame-lang/framepiler) language for systems work. It is NOT a serious deployment target. It is a demonstration artifact.

The full context lives in `docs/vision.md`. The architecture lives in `docs/architecture.md`. The milestone plan lives in `docs/roadmap.md`. The testing approach lives in `docs/testing.md`. Read these before substantive changes.

The current milestone is **H0** — the minimum viable hosted-mode shell. See `docs/roadmap.md` for what's next.

## Build commands

```bash
# Prerequisites (one time)
cargo install framec
cargo xtask install-tools

# Day-to-day (hosted shell)
cargo build                                  # builds default-members (excludes kernel)
cargo test --workspace --exclude frame-os-kernel
cargo run --bin frame-os-shell               # launch the shell

# Bare-metal kernel
cargo build -p frame-os-kernel --target x86_64-unknown-none
cargo xtask qemu                             # build kernel + boot in QEMU via Limine UEFI
                                             # (needs `brew install qemu mtools` on macOS)

# Quality gates (must pass before a PR)
cargo fmt --all -- --check
cargo clippy --workspace --exclude frame-os-kernel --all-targets -- -D warnings
cargo xtask check-diagrams                   # diagram drift check
```

The kernel crate is excluded from workspace-wide commands because it requires the bare-metal target `x86_64-unknown-none` and won't link against the host environment. Build it explicitly with `cargo build -p frame-os-kernel --target x86_64-unknown-none`, or via `cargo xtask qemu` which handles the build + Limine bootloader + ESP image assembly + QEMU launch in one step.

## Frame language syntax

This project uses the current Frame attribute syntax (post-RFC-0013):

- **`@@[target("rust")]`** at the top of every `.frs` file. Bare `@@target` is hard-cut (E804).
- **`@@[main]`** to mark a file's primary system if more than one is defined.
- **`@@[persist(<type>)]`** with companion `@@[save(<name>)]` / `@@[load(<name>)]` for serializable systems. Bare `@@[persist]` is hard-cut (E814).
- **HSM forwarding is explicit** (RFC-0019). A child state must declare `=> $^` (in a handler or as a state-level default) for ancestor `$>`/`<$` to run. The cascade is gone.
- **Return values use `@@:(expr)`, `@@:return = expr`, or `@@:return(expr)`.** Bare `return` is native; it exits the handler without setting the return value (W415).

If you're unsure about syntax, the authoritative reference is `docs/glossary.md` and `docs/frame_language.md` in the Frame language docs (provided as project knowledge).

## Project structure

```
frame-os/
├── frame/              — Frame source files (.frs), one per system
├── shell/              — hosted-mode binary
├── kernel/             — bare-metal kernel (placeholder until B0)
├── shared/             — types shared between shell and kernel (empty until B2)
├── xtask/              — internal build orchestration
└── docs/
    ├── vision.md       — purpose, audience, success criteria
    ├── architecture.md — Frame vs. native split, kernel layer cake
    ├── portability.md  — Rust/C port rules, multi-host story
    ├── roadmap.md      — H0-H3 and B0-B4 milestones
    ├── testing.md      — what's tested at each level
    └── systems/
        ├── README.md   — index of all Frame systems
        ├── _template.md — required structure for per-system docs
        └── <name>.md   — one per implemented Frame system
```

## Adding a Frame system

When adding a new `.frs` file at `frame/<name>.frs`:

1. Add `("<name>", "<name>.frs")` to `FRAME_SYSTEMS` in `shell/build.rs` (or `kernel/build.rs` for bare-metal systems)
2. Add `include!(concat!(env!("OUT_DIR"), "/<name>.rs"));` to the appropriate `frame_systems.rs`
3. Write the system, following the conventions in existing `.frs` files
4. Write behavioral tests (Level 3) in `tests/<name>_behavior.rs`
5. Write a snapshot test (Level 2) in `tests/state_graphs.rs`
6. Run `cargo xtask regen-diagrams` and commit the new `.svg`
7. Write the per-system doc at `docs/systems/<name>.md`, following `docs/systems/_template.md`
8. Update `docs/systems/README.md` to mark the system "In progress" or "Documented"

The per-system doc is required, not optional. See `docs/systems/shell.md` for a worked example.

## What NOT to do

- **Don't bypass the per-system doc requirement.** Every Frame system has one, with all required sections filled in.
- **Don't add Frame systems that aren't justified.** The "Why a state machine" section of the per-system doc has to answer the question honestly. If the system is borderline, the doc should say so (see `KernelTimer` in `docs/architecture.md` for an example).
- **Don't commit generated artifacts to source.** The `OUT_DIR` Rust files are build outputs, not source. The `.svg` diagrams ARE committed (they're documentation, not transient).
- **Don't add features outside the roadmap.** If a milestone needs to grow, update `docs/roadmap.md` first with reasoning. The vision doc commits to honesty about scope; the roadmap honors that.
- **Don't break the C-port-readiness rules.** See `docs/portability.md` for the specific constraints: no `Drop` for kernel resources, no `Box<dyn Trait>` in hot paths, prefer `heapless`/fixed-size collections over `Vec`/`String` in `no_std`, etc. The rules are written down so changes against them are deliberate.

## Style

- Documentation tone is matter-of-fact, technical, no marketing language. Avoid "powerful", "elegant", "robust" — say what something does.
- Per-system docs: use the template's section ordering. Skip sections marked OPTIONAL only if truly empty.
- Frame source: comment generously around the system declaration but inside `machine:` and `interface:` blocks let the code speak.
- Rust source: standard `rustfmt` (the CI checks this).
- Commit messages: present tense, imperative ("add Parser system" not "added Parser system"). Reference the milestone (e.g., "H1: add Parser system").

## Working on a milestone

The current milestone (H0) and what comes next are in `docs/roadmap.md`. Before starting H1 work:

1. Re-read the relevant roadmap entry — it lists the Frame systems, native dependencies, test infrastructure additions, and success criteria
2. Sketch the state graphs of any new systems on paper or in a comment block before writing the `.frs` files
3. Land changes incrementally — one Frame system at a time, each with its tests and doc, before moving to the next

## When in doubt

If a design question comes up that isn't covered by the docs, ask. The docs are the canonical source; pushing back against them is welcome if they're wrong, but silently diverging from them is not.

If a syntax question comes up about Frame itself, consult the Frame language reference (in project knowledge) before guessing. Frame's syntax has evolved (`@@target` → `@@[target("...")]`, RFC-0019 explicit forwarding) and stale habits can produce code that doesn't compile.

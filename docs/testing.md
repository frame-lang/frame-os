# Testing

Frame OS's tests are in-tree, organized per crate, and run via standard `cargo test`. This doc describes what's tested at each level, where the tests live, and what conventions Frame OS uses.

## Principles

**Tests live beside the code they test.** Standard Rust convention: unit tests in `#[cfg(test)] mod tests` blocks beside the implementation, integration tests in each crate's `tests/` directory, doctests inline in documentation. Frame OS does not deviate from this without a specific reason.

**Test infrastructure is part of the project, not a separate artifact.** No second repository to clone, no separate setup. A new contributor runs `cargo test --workspace` and gets feedback on everything that's automatically testable. The cost is a slightly larger main repo; the benefit is that the project stays clone-and-run.

**`cargo test` is the canonical entry point.** Anything that doesn't run under `cargo test` (real hardware tests, manual visual checks) is documented as such and gated behind `#[ignore]` or `cargo xtask` subcommands. There's no shadow test-running pipeline.

**Test the boundary, not the implementation detail.** Frame systems are tested through their interface methods. State transitions are verified by calling events and observing post-conditions (state name, domain values). The internals of dispatch are framec's territory, not Frame OS's — we test that *our* state machines do what *we* specified, not that framec's generated code dispatches correctly.

**Snapshot tests for anything that's "the generator's output should match this text."** Generated Rust code, generated state graphs, expected serial output from QEMU smoke tests — all benefit from the `cargo insta review` workflow. Same approach Frame's own project uses (RFC-0027).

## What's Tested at Each Level

### Level 1: Frame source compiles

**What:** Every `.frs` file in `frame/` is valid Frame and produces compilable Rust.

**Where:** Implicit in `cargo build`. The build scripts invoke `framec` on each `.frs` file; framec's errors halt the build. There's no separate test for this — if `cargo build` succeeds, every `.frs` is at least syntactically valid Frame and produces Rust that compiles.

**How to extend:** New Frame systems just need to land in `frame/` with `build.rs` configured to find them. No test machinery to update.

### Level 2: Generated state graph matches expectation

**What:** `framec -l graphviz <system>.frs` produces the state graph the design committed to. If a state is added or a transition changes, the test catches the drift.

**Where:** Per-crate `tests/state_graphs.rs`, using `insta` snapshot tests.

**Conventions:**
- One snapshot per Frame system
- Snapshot file lives in `tests/snapshots/<crate>__state_graphs__<system_name>.snap`
- Updating a snapshot is deliberate: `cargo insta review` shows the diff and accepts only after human review
- Snapshot accepts are committed in the same PR as the source change that caused them

**Why:** State graphs are part of the design. A silent state-graph change is a silent design change. The snapshot test makes design changes explicit in code review.

### Level 3: Generated state machine behavior (unit)

**What:** Each Frame system's interface methods produce the expected state transitions and side effects.

**Where:** `#[cfg(test)] mod tests` in the module that wraps each Frame system. Tests instantiate the system, fire events, assert state.

**Conventions:**
- Test names follow `<event>_in_<state>_transitions_to_<new_state>` or `<event>_in_<state>_<expected_outcome>`
- Use `@@:system.state` to read current state name in assertions
- Cover at least one test per state-event pair the design committed to

**Example shape:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_in_booting_transitions_to_init_memory() {
        let mut kernel = Kernel::__create();
        kernel.boot();
        assert_eq!(kernel.state(), "InitMemory");
    }

    #[test]
    fn panic_in_init_memory_transitions_to_halted() {
        let mut kernel = Kernel::__create();
        kernel.boot();
        kernel.kernel_panic("test");
        assert_eq!(kernel.state(), "Halted");
    }
}
```

### Level 4: Multi-system integration

**What:** Frame systems composed together behave correctly. The `Kernel` invoking `Scheduler.tick()` actually advances the scheduler; the scheduler invoking a `Task`'s interface method routes correctly.

**Where:** Each crate's `tests/` directory. Integration tests have access to the crate's public API but not its private internals.

**Conventions:**
- One test file per integration scenario (`tests/scheduler_task_interaction.rs`, not `tests/everything.rs`)
- Tests construct the systems via their factories, then drive scenarios via interface calls
- Avoid testing through native actions where possible; test through Frame interface methods

### Level 5: Native Rust units

**What:** The non-Frame parts of the kernel (`memory`, `arch`, `interrupt`, `elf` modules) behave correctly in isolation.

**Where:** `#[cfg(test)] mod tests` beside each native module.

**Conventions:** Standard Rust testing. Pure functions are easiest; functions with side effects (page table manipulation) may need a mock or a feature flag for testability. The `arch` module's lowest-level primitives — port I/O, MSR writes, inline assembly — are not unit-tested; they're tested via QEMU smoke tests at Level 7.

### Level 6: Hosted shell end-to-end

**What:** Running `cargo run --bin frame-os-shell` produces the right behavior — prompt appears, commands work, signals route correctly.

**Where:** `shell/tests/e2e.rs`, using `assert_cmd` and `predicates` crates.

**Conventions:**
- Tests spawn the shell as a subprocess
- Input is written to stdin; output is captured from stdout
- Each test exercises one user-visible behavior (typing `exit` exits; typing `pwd` prints the cwd)
- Tests that depend on platform features (Unix signals) use `#[cfg(unix)]`

### Level 7: Bare-metal kernel in QEMU

**What:** The kernel boots in QEMU, executes a test scenario, and writes its result to a known serial port. The test runner on the host reads that output and asserts.

**Where:** `kernel/tests/qemu_smoke.rs`, using the bootimage-style pattern that the Rust OS-dev community has converged on.

**Mechanics:**
- The kernel itself contains a `#[cfg(test)]` test runner that writes pass/fail markers to the serial port and exits via QEMU's `isa-debug-exit` device
- The host-side test runner invokes QEMU with the test kernel ELF, captures serial output, and parses the markers
- Each test is a `#[test_case]` function inside the kernel that runs in the QEMU environment

**Conventions:**
- Test names mirror the milestone they validate (`boot_prints_banner_b0`, `scheduler_runs_three_tasks_b1`)
- Tests are kept small — each one boots the kernel from scratch
- Long-running scenarios (multi-step shell interactions) are integration tests at Level 4 against a non-QEMU build, not QEMU tests at Level 7

**Tool dependencies:** QEMU must be installed on the host. The `xtask` provides `cargo xtask install-tools` to handle this where possible; otherwise it prints clear instructions per platform.

### Level 8: Real hardware

**What:** The kernel actually runs on a Pi Pico or Pi 4.

**Where:** `kernel/tests/hardware.rs`, gated with `#[ignore]`.

**Conventions:**
- Each hardware test has a comment block at the top describing the required hardware setup (which board, which serial connection, which GPIO pins exercised)
- Tests are run manually: `cargo test -- --ignored --test-threads=1`
- The test name encodes the target: `pico_blinker_toggles_gpio_25`, `pi4_serial_echo_at_115200`
- Failures are reported with serial output captured to a file for review

**This level is intentionally low-effort.** Hardware testing is a small, manual surface — not an automated CI gate. Frame OS isn't trying to be a continuous-deployment-on-real-hardware project; it's trying to demonstrate that the kernel runs on real boards when it needs to.

### Level 9: C-port-readiness lints

**What:** Code in `frame/`, `kernel/`, and `shell/` doesn't use Rust features that would block a future C port (see [`portability.md`](portability.md) for the rule set).

**Where:** `xtask/src/lint_c_port.rs`, invoked via `cargo xtask lint-c-port`. Plus a curated `clippy.toml` config that catches what clippy can catch.

**Conventions:**
- Forbid `Drop` impls in kernel code (use explicit `cleanup()` instead)
- Forbid `Box<dyn Trait>` in kernel data structures (use enum dispatch)
- Forbid `Vec<T>` and `String` in `no_std` paths (use `heapless` types)
- Warn on heavy trait usage in modules tagged for C portability

**Status:** Most of these are aspirational rules until written. The lint isn't blocking on day one — it's a Level-9 quality check that lands when the codebase is large enough for the rules to matter.

### Level 10: Diagram drift

**What:** The `.svg` files committed in `docs/systems/` match what `framec -l graphviz` would produce today.

**Where:** `xtask/src/check_diagrams.rs`, invoked via `cargo xtask check-diagrams`.

**Mechanics:**
- For each `.frs` file in `frame/`, run `framec -l graphviz | dot -Tsvg` to produce the current SVG
- Compare byte-for-byte (or canonicalized form) against the committed `.svg` next to its per-system doc
- Drift produces a clear error message: "diagrams/scheduler.svg is out of date — run `cargo xtask regen-diagrams` and commit"

**Why a separate check:** This isn't a `cargo test` thing because the artifact is in `docs/`, not in test fixtures. It runs in CI alongside other checks.

## What Gets Run When

A coherent policy for which tests run at which trigger:

**Every `cargo test --workspace`:** Levels 2, 3, 4, 5, 6, 7 (the QEMU smoke tests run in this set, since QEMU is fast). Total target: under 60 seconds for the full set. Levels 8, 9, 10 do not run by default.

**Every push to CI:** Same as `cargo test --workspace`, plus `cargo xtask check-diagrams` (Level 10) and `cargo xtask lint-c-port` (Level 9). Pre-merge gate.

**Manually, before a release or major milestone:** Levels 1–10, including hardware tests on whatever boards are wired up. No automation; the maintainer runs them and confirms.

**On dependency or framec updates:** Same as CI, with extra attention to snapshot test failures (Level 2 may surface generator changes).

## Conventions for Frame Systems

The systems template includes a "Testing" section. For each Frame system, the per-system doc should answer:

1. **State graph snapshot:** does the system have a snapshot test for its generated state graph? (Should always be "yes" once Level 2 lands.)
2. **Behavioral tests:** which state-event pairs have explicit tests, and which are left implicit?
3. **Integration tests:** which other systems is this one exercised against, in which integration test files?
4. **Hardware coverage:** if the system has bare-metal-specific behavior, which hardware tests cover it?

A system in "Documented" status should have all four answered. A "Planned" system can leave the answers blank.

## Tools

Frame OS tests rely on these crates and tools, all of which are standard in the Rust ecosystem:

| Tool | Purpose | Required at |
|---|---|---|
| `cargo test` | Standard test runner | Always |
| `insta` | Snapshot testing | Level 2, Level 7 (expected output) |
| `assert_cmd` + `predicates` | Process-level E2E tests | Level 6 |
| `bootimage` (or equivalent) | QEMU-bootable kernel image creation | Level 7 |
| QEMU | Bare-metal kernel host | Level 7 |
| `cargo clippy` | Lint pass | Level 9 (partial) |
| `dot` (GraphViz) | Diagram rendering | Level 10 |

Hardware testing requires actual hardware. No automation library substitutes; the maintainer runs the tests by hand.

## What's Not Tested

Honest about scope:

- **Frame language semantics** — not our job to test framec. If `framec` has a bug, we file it against framec, not work around it.
- **QEMU correctness** — same. We trust QEMU to faithfully execute x86_64. If a kernel behaves differently in QEMU than on real hardware, that's a portability issue we investigate manually.
- **Rust compiler correctness** — same.
- **Long-running stability** — Frame OS is not a production OS. Soak tests, fuzz tests, and stability tests aren't on the roadmap.
- **Performance** — no benchmarks. If performance matters for a specific subsystem, that subsystem gets its own benchmark in `benches/`, but the project does not have a performance promise.
- **Security** — Frame OS has no security model. There's nothing to test.

If any of these become relevant later, they get their own doc — they don't sneak in under "testing."

## Open questions

- **When does `cargo xtask check-diagrams` run?** Pre-commit hook, CI-only, or both? Pre-commit is friendlier to contributors but adds setup cost. Defer until the first `.svg` is committed.
- **Should the Level 7 QEMU smoke tests use the `bootimage` crate or a custom `xtask`-based runner?** `bootimage` is the most-traveled path but adds a dependency. A custom runner is small but reinvents a wheel. Decide when B0 lands.
- **Coverage targets?** No line-coverage target is set today. The risk of "high coverage but low signal" is real for a state-machine-heavy codebase where the interesting thing is *which* transitions are exercised, not *how many lines* are touched. Defer.

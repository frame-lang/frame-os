# Roadmap

Frame OS evolves through two parallel tracks. Each track has a sequence of milestones. Some milestones in one track depend on Frame systems developed in the other; the dependencies are noted below.

The two tracks:

- **H-track (hosted)** — the Frame OS shell running as a normal application on Linux, macOS, and Windows.
- **B-track (bare-metal)** — the Frame OS kernel running in QEMU and on real hardware.

The H-track is simpler and finishes faster. It's the natural starting point because it surfaces Frame's value at small scale, exercises the shared Frame systems (`Shell`, `Parser`) before they have to work in a kernel context, and produces a demo artifact that runs on any developer's laptop within `cargo run`.

The B-track is the headline project and where Frame's argument is strongest. It depends on some H-track work (the `Shell` and `Parser` systems are shared) but can be developed in parallel once that shared layer stabilizes.

## Milestone exit-criteria convention

Each milestone below has an **Exit criteria** table mapping every committed behavior to one or more validating tests. A milestone is "done" iff:

1. Every row's named test(s) exist in the repo at the path indicated
2. Every named test passes on the full CI matrix (Linux x86_64, macOS aarch64, Windows — see [`.github/workflows/ci.yml`](../.github/workflows/ci.yml))
3. The full quality-gate suite passes: `cargo build --workspace`, `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask check-diagrams`
4. The milestone's Frame systems each have a per-system doc in [`docs/systems/`](systems/) following [`docs/systems/_template.md`](systems/_template.md), with the doc's Testing section listing the tests below

Criteria flagged **Manual** are verified by the maintainer on at least one host platform and noted in the milestone's "Status" line. A manual criterion is an honest exception, not a default — automation is the goal.

The test-naming convention follows H0: `<event>_in_<state>_<expected>` for behavioral tests, `<user_visible_behavior>` for E2E tests, `<system_name>_state_graph_snapshot` for snapshot tests. Each new Frame system adds a corresponding snapshot test in `<crate>/tests/state_graphs.rs`.

## Track H: Hosted-mode shell

### H0 — minimum viable shell

**Scope:** Frame OS shell binary builds and runs on Linux, macOS, and Windows. Prompt appears, `exit` works, Ctrl-C exits gracefully. No other commands. Line editing via `rustyline`. The test infrastructure described in [`testing.md`](testing.md) is bootstrapped — `cargo test --workspace` runs and produces reasonable output even though the test set is small.

**Frame systems:** `Shell` (minimal — `$Prompting → $Exiting` on either `line("exit"/"quit")` or `interrupt()`).

**Native dependencies:** `rustyline` for line editing and Ctrl-C / Ctrl-D handling at the prompt. `signal-hook` and `ctrlc` are deferred to H2, where Ctrl-C must additionally kill a running external child — rustyline alone covers the H0 scope (it intercepts Ctrl-C and Ctrl-D during `readline()` and surfaces them as `ReadlineError::Interrupted` / `Eof`, which the host loop maps to the Shell's `interrupt()` event).

**Test infrastructure bootstrapped at H0:**
- Workspace `cargo test` runs successfully across all crates
- `insta` snapshot tests configured; one snapshot exists for `Shell`'s state graph (Level 2)
- Behavioral tests for `Shell` covering every committed state-event pair (Level 3)
- `assert_cmd`-based E2E tests that spawn the shell, drive it via stdin, assert on stdout and exit code (Level 6)
- `cargo xtask check-diagrams` exists and verifies the committed `shell.svg`

#### Exit criteria

A criterion is "done" iff the named test asserts it and passes on the CI matrix. Manual-only criteria are flagged explicitly. Criteria are *conjunctive* — all must pass for the milestone to be complete.

| # | Exit criterion | Validating test(s) |
|---|---|---|
| H0-1 | `cargo run --bin frame-os-shell` produces a prompt on Linux, macOS, and Windows | E2E `prints_prompt` (Level 6, [`shell/tests/e2e.rs`](../shell/tests/e2e.rs)) running on the CI matrix in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) |
| H0-2 | The startup banner prints | E2E `prints_banner_on_startup` |
| H0-3 | Typing `exit` exits with code 0 and prints `goodbye` | E2E `exit_command_exits_cleanly`; behavioral `exit_command_transitions_to_exiting` |
| H0-4 | Typing `quit` exits with code 0 and prints `goodbye` | E2E `quit_command_exits_cleanly`; behavioral `quit_command_transitions_to_exiting` |
| H0-5 | Closing stdin (Ctrl-D / EOF) exits with code 0 and prints `goodbye` | E2E `eof_exits_cleanly`; behavioral `interrupt_in_prompting_transitions_to_exiting` |
| H0-6 | Ctrl-C at the prompt exits cleanly (Frame `interrupt()` event → `$Exiting`) | Behavioral `interrupt_in_prompting_transitions_to_exiting`, `interrupt_in_exiting_is_idempotent`, `interrupt_after_unknown_commands_still_exits` |
| H0-7 | Ctrl-C does not leave the terminal in a broken state (cursor visible, line discipline restored) | **Manual** — verified by running `cargo run --bin frame-os-shell` interactively, pressing Ctrl-C, confirming the shell prompt that follows on the user's terminal works normally. Rustyline's `Drop` implementation restores tcsetattr state; no automated test |
| H0-8 | Unknown commands print a clear "unknown command" message and stay in `$Prompting` | E2E `unknown_command_prints_message`; behavioral `unknown_command_does_not_exit` |
| H0-9 | Empty input does not produce noise (no "unknown command" output) and stays in `$Prompting` | E2E `empty_lines_dont_crash`; behavioral `empty_line_does_not_exit`, `whitespace_only_line_does_not_exit` |
| H0-10 | Multiple inputs work in sequence before exit | E2E `multiple_commands_before_exit`; behavioral `many_unknown_commands_before_exit` |
| H0-11 | The committed state diagram (`docs/systems/shell.svg`) matches the source `.frs` | `cargo xtask check-diagrams` (Level 10) |
| H0-12 | The generated state graph is captured as a snapshot (drift caught in code review) | Snapshot `shell_state_graph_snapshot` (Level 2, [`shell/tests/state_graphs.rs`](../shell/tests/state_graphs.rs)) |
| H0-13 | Per-system documentation for `Shell` exists, follows the template, and is current | [`docs/systems/shell.md`](systems/shell.md) — review check, not an automated gate |
| H0-14 | All CI quality gates pass: `cargo build`, `cargo test --workspace`, `cargo fmt -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask check-diagrams` | The full CI matrix in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) |

**Estimated effort:** A few days for the shell itself, plus several days for the test scaffolding. Call it one to two weeks combined.

**Status:** Done. All automated criteria pass; manual criterion H0-7 verified on macOS Apple Silicon.

### H1 — builtins

**Scope:** H0 plus a set of built-in commands. The `Parser` Frame system is introduced. `Shell` gains `$Parsing` and `$RunningBuiltin` states.

**Builtins implemented:**
- `cd <path>` — change current directory (updates `Shell`'s domain `cwd`)
- `pwd` — print current directory
- `ls [path]` — list directory contents
- `cat <file>` — print file contents
- `echo <args...>` — print arguments
- `history` — show command history
- `help` — list available commands
- `exit` — exit the shell (already in H0)

**Frame systems:** `Shell` (extended with `$Parsing` and `$RunningBuiltin` states), `Parser` (new — `$ReadingWord → $InWord → $InQuotedString → $ReadingWord → $Done`).

**Native dependencies:** `std::fs`, `std::env`. No new external crates.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| H1-1 | `Parser` state graph matches the committed design | Snapshot `parser_state_graph_snapshot` in `shell/tests/state_graphs.rs` |
| H1-2 | `Parser` correctly tokenizes unquoted words, quoted strings, escaped characters, and whitespace runs | Behavioral tests in `shell/tests/parser_behavior.rs` — one per committed state-event pair, plus `parses_unquoted_words`, `parses_double_quoted_string`, `parses_escaped_chars`, `parses_empty_input`, `parses_mixed_quoted_and_unquoted` |
| H1-3 | `Shell` extended state graph (with `$Parsing` and `$RunningBuiltin`) matches the committed design | Updated snapshot `shell_state_graph_snapshot` (drift caught by insta) |
| H1-4 | `Shell` transitions correctly through `$Prompting → $Parsing → $RunningBuiltin → $Prompting` | Behavioral test `line_with_known_builtin_cycles_through_parsing_and_running` in `shell/tests/shell_behavior.rs` |
| H1-5 | `cd <path>` updates `Shell.cwd` and subsequent filesystem operations respect it (not the host process's cwd) | Behavioral `cd_updates_shell_cwd`; E2E `cd_then_pwd_reflects_new_cwd` |
| H1-6 | `pwd` prints the shell's `cwd` | E2E `pwd_prints_current_directory` |
| H1-7 | `ls [path]` lists directory contents (resolved against shell `cwd`) | E2E `ls_lists_default_dir`, `ls_lists_specified_dir`, `ls_handles_missing_dir_with_error` |
| H1-8 | `cat <file>` prints file contents (resolved against shell `cwd`) | E2E `cat_prints_file_contents`, `cat_handles_missing_file_with_error` |
| H1-9 | `echo <args...>` prints arguments separated by spaces | E2E `echo_prints_args` |
| H1-10 | `history` shows the command history maintained by rustyline | E2E `history_shows_prior_commands` |
| H1-11 | `help` lists the available builtins | E2E `help_lists_all_builtins` |
| H1-12 | Unknown commands (no matching builtin) print "unknown command" and stay in `$Prompting` | E2E `unknown_command_prints_message` (carried from H0; still passes) |
| H1-13 | `Parser` and `Shell` per-system docs exist and follow the template | [`docs/systems/parser.md`](systems/parser.md), updated [`docs/systems/shell.md`](systems/shell.md) — review check |
| H1-14 | `Parser` and `Shell` SVG diagrams committed and current | `cargo xtask check-diagrams` (covers both) |
| H1-15 | All CI quality gates pass on Linux/macOS/Windows | Full CI matrix |

**Estimated effort:** A week or two.

### H2 — external command execution

**Scope:** H1 plus the ability to run external programs. `Shell` gains `$RunningExternal` state. Anything typed that isn't a builtin is treated as a host-OS command.

**Behavior:**
- `python3 -c "print(2+2)"` runs the host's Python and prints `4`
- `vim foo.txt` opens vim (the shell blocks until vim exits)
- `nonexistent-command` produces a "command not found" error from the shell, not a panic
- Ctrl-C in `$RunningExternal` kills the child process and returns to the prompt

**Frame systems:** `Shell` (extended with `$RunningExternal` state and per-state `interrupt()` behavior).

**Native dependencies:** `std::process::Command`, plus `ctrlc` (Windows) / `signal-hook` (Unix) for SIGINT delivery to the host loop while a child process is running. (H0 only needed rustyline's in-readline Ctrl-C; H2 also needs out-of-readline SIGINT.)

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| H2-1 | `Shell` extended state graph (with `$RunningExternal`) matches the committed design | Updated `shell_state_graph_snapshot` |
| H2-2 | Non-builtin input transitions `$Prompting → $RunningExternal`, executes the host command, transitions back to `$Prompting` | Behavioral `unknown_input_runs_external_command`; E2E `python_runs_arithmetic` |
| H2-3 | Successful external commands' stdout reaches the user's terminal | E2E `external_command_stdout_passes_through` |
| H2-4 | Failed external commands (non-zero exit) are surfaced cleanly without panicking the shell | Behavioral `external_command_nonzero_exit_returns_to_prompting`; E2E `external_command_exit_code_surfaced` |
| H2-5 | A command that doesn't exist on the host PATH produces a "command not found" message, not a panic | E2E `nonexistent_command_prints_not_found` |
| H2-6 | `interrupt()` in `$Prompting` clears the current line and stays in `$Prompting` (H0's "exit on Ctrl-C" behavior is overridden by H2) | Behavioral `interrupt_in_prompting_clears_line_stays_prompting`; replaces H0's `interrupt_in_prompting_transitions_to_exiting` |
| H2-7 | `interrupt()` in `$RunningExternal` kills the child process and transitions back to `$Prompting` | Behavioral `interrupt_in_running_external_kills_child_returns_to_prompting`; E2E `ctrl_c_kills_sleep_returns_to_prompt` (uses `sleep 60` as the child) |
| H2-8 | `interrupt()` in `$Exiting` remains a no-op | Behavioral `interrupt_in_exiting_is_idempotent` (carried from H0) |
| H2-9 | The Frame argument is demonstrably visible: same `interrupt()` event, different per-state behavior, no `if state == ...` branching in native code | Code review check; the diff for H2 should show `$RunningExternal { interrupt() { ... } }` added in `shell.frs`, no state-conditional `match` added to `main.rs` |
| H2-10 | Updated `shell.svg` reflects the new state and transitions | `cargo xtask check-diagrams` |
| H2-11 | Updated per-system doc for `Shell` reflects the new state, the new per-state `interrupt()` behavior, and an explicit Frame-argument paragraph for H2 | Review check; per-system doc Testing section enumerates new behavioral tests |
| H2-12 | All CI quality gates pass on Linux/macOS/Windows | Full CI matrix |

**Estimated effort:** A week.

### H3 — job control

**Scope:** H2 plus background execution and Unix-style job control. New Frame systems: `JobControl` (manager) and `Job` (per-job instance).

**Features:**
- `command &` runs in background, returns to prompt immediately
- `jobs` lists running and stopped jobs
- `fg [job]` brings a background job to the foreground
- `bg [job]` resumes a stopped job in the background
- Ctrl-Z stops the foreground job and returns to prompt (Unix only; Windows doesn't have an equivalent signal)
- `wait [job]` waits for a job to complete

**Frame systems:** `JobControl` (new, manager), `Job` (new, per-instance), `Shell` (extended with `$Suspended` and integration points).

**Native dependencies:** Unix-specific signal handling for SIGTSTP / SIGCONT via `signal-hook`. Windows omits the SIGTSTP-dependent features behind `#[cfg(unix)]`.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| H3-1 | `Job` and `JobControl` state graphs match committed design | Snapshots `job_state_graph_snapshot`, `job_control_state_graph_snapshot` |
| H3-2 | `Job` correctly transitions through its lifecycle (`$Created → $Foreground → $Stopped → $Background → $Done`) | Behavioral tests in `shell/tests/job_behavior.rs` covering each committed state-event pair |
| H3-3 | `JobControl` tracks multiple `Job` instances, exposes `fg`/`bg`/`jobs`/`wait` operations | Behavioral tests in `shell/tests/job_control_behavior.rs`; one test per operation |
| H3-4 | `command &` runs in background, shell returns to prompt immediately | E2E `background_command_returns_to_prompt_immediately` (uses `sleep 5 &`) |
| H3-5 | `jobs` lists running and stopped jobs with their `Job` state | E2E `jobs_lists_running_and_stopped` |
| H3-6 | `fg [job]` brings a background job to the foreground | E2E `fg_brings_background_to_foreground` (Unix only via `#[cfg(unix)]`) |
| H3-7 | `bg [job]` resumes a stopped job in the background | E2E `bg_resumes_stopped_job` (Unix only) |
| H3-8 | Ctrl-Z stops the foreground job (sends SIGTSTP) and returns to prompt | Behavioral `sigtstp_in_running_external_transitions_to_suspended_job`; E2E `ctrl_z_stops_foreground_returns_to_prompt` (Unix only; manual on hardware-limited CI) |
| H3-9 | `wait [job]` blocks until the specified job completes | E2E `wait_blocks_until_job_done` (Unix only) |
| H3-10 | Windows build skips SIGTSTP-dependent E2E tests via `#[cfg(unix)]`, does not fail | CI matrix: Windows job passes despite missing tests |
| H3-11 | Updated `shell.svg`, `job.svg`, `job_control.svg` committed and current | `cargo xtask check-diagrams` |
| H3-12 | Per-system docs for `Job` and `JobControl`, updated for `Shell` | Review check |
| H3-13 | All CI quality gates pass | Full CI matrix |

**Estimated effort:** Two to three weeks. This milestone is where job-control complexity has historically caused bugs in hand-written shells, so the Frame demonstration is particularly visible here.

H3 is the H-track's final committed milestone. Further H-track work (a configuration file, a tab-completion engine, scriptability) is conceivable but not committed.

## Track B: Bare-metal kernel

### B0 — boots and halts

**Scope:** Frame OS kernel boots in QEMU x86_64 via Limine, prints a banner over the serial port, halts cleanly. No scheduler, no tasks, no user programs. QEMU smoke test infrastructure (Level 7 in [`testing.md`](testing.md)) is established.

**Frame systems:** `Kernel` (with `$Booting` HSM parent over `$InitMemory`, `$InitIDT`, `$InitTimer`, `$InitConsole`, `$Halting`).

**Native components:**
- Boot stub that takes Limine's handoff
- Minimal GDT and IDT setup
- Page table initialization (identity-mapped, single 1GB page initially)
- UART driver (port-based 16550 for x86_64)
- Panic handler that halts the CPU
- Test runner inside the kernel that writes pass/fail markers to serial and exits via QEMU's `isa-debug-exit`
- Host-side QEMU test driver that invokes QEMU, captures serial output, parses markers

**Test infrastructure added at B0:**
- QEMU smoke tests (Level 7) driven by `cargo xtask qemu-test`. The smoke tests live in the xtask harness (`xtask/src/main.rs`, the `SMOKE_TESTS` table + `run_smoke_test`) rather than a `kernel/tests/qemu_smoke.rs` integration-test file. The kernel crate is `[[bin]] + #![no_std] + #![no_main]` for `x86_64-unknown-none` and can't host host-target `cargo test` integration tests, so the smoke runner is an xtask subcommand that boots QEMU, captures serial to a file, and asserts on substrings. (The original roadmap named `kernel/tests/qemu_smoke.rs`; that location doesn't work given the bare-metal crate constraints, so the harness moved to xtask. The behavior — boot, capture, assert — is unchanged.)
- `cargo xtask qemu-test` subcommand wired in (was a stub)
- State-graph snapshot tests for `Kernel` and `SerialDriver`
- Per-system docs for `Kernel` and `SerialDriver` written from the template

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B0-1 | `Kernel` state graph matches committed design (HSM with `$Booting` parent, init-phase children, `$Running`, `$Halted`) | Snapshot `kernel_state_graph_snapshot` in `kernel-tests/tests/state_graphs.rs` — **done at Step 2** |
| B0-2 | `SerialDriver` state graph matches committed design (`$Idle → $Transmitting → $Draining → $Idle`) | Snapshot `serial_driver_state_graph_snapshot` — lands at Step 3 |
| B0-3 | `Kernel` boot HSM correctly progresses through init phases | Behavioral `boot_chain_prints_all_phases_in_order` + `fresh_kernel_runs_boot_chain_to_running_not_done` in `kernel-tests/tests/kernel_behavior.rs` (host build); also `boot_hsm_runs_init_chain_b0` QEMU smoke — **done at Step 2** |
| B0-4 | `Kernel.kernel_panic()` dispatches per-state (Frame argument) | `$Running`'s variant covered by `panic_in_running_prints_runtime_message_and_halts` + `runtime_panic_uses_running_variant_not_boot_variant`. **Boot-child forwarding (`=> $^`) is not externally observable** with the synchronous boot chain — `__create()` runs the chain to `$Running` so no external event reaches a boot child. Recorded as an Open question in `docs/systems/kernel.md`; testing it directly needs a fault-injection hook or an event-stepped boot chain (design decision, deferred to when a boot phase first actually fails) |
| B0-5 | `SerialDriver` correctly transitions on `write_byte` events | Behavioral tests in `kernel/tests/serial_driver_behavior.rs` |
| B0-6 | `cargo xtask qemu` boots the kernel image in QEMU x86_64 (no automated assertion; manual smoke) | **Manual** — maintainer runs the command, observes banner on serial console, halts cleanly |
| B0-7 | `cargo xtask qemu-test` runs the kernel image in QEMU, captures serial output, and exits 0 on success / non-zero on assertion failure | `cargo xtask qemu-test` itself, exercised in CI on Linux only (QEMU is most reliable there) — **done at Step 4** |
| B0-8 | The kernel banner appears on serial output during a QEMU boot | QEMU smoke test `boot_prints_banner_b0` (Level 7, in `xtask/src/main.rs`'s `SMOKE_TESTS` table) — **done at Step 4** |
| B0-9 | The kernel halts cleanly (returns to `hlt` loop) after init | Covered indirectly: the smoke tests assert the boot chain completes (`[run] kernel running`) with no panic/triple-fault markers, and QEMU stays alive (it's SIGKILLed at timeout, not crashed). A dedicated `kernel_halts_cleanly_b0` with `isa-debug-exit` exit-code assertion lands once a `smoke-test` Cargo feature gates the kernel's `isa-debug-exit` path (deferred — see the smoke-test module comment in `xtask/src/main.rs`) |
| B0-10 | The boot sequence is the HSM, not a script of init calls (Frame argument check) | Code review: `kmain` calls `Kernel::__create()` and lets the HSM drive; no manual sequence of init steps. **Done at Step 2** — `kernel/src/main.rs` has no init-call script; the `-> $NextPhase` transitions in `frame/kernel.frs` encode the order |
| B0-11 | `Kernel` (and later `SerialDriver`) SVG diagrams committed and current | `cargo xtask check-diagrams` — `kernel.svg` **done**; `serial_driver.svg` lands at Step 3 |
| B0-12 | Per-system docs for `Kernel` (and later `SerialDriver`) exist and follow the template | Review check — `docs/systems/kernel.md` **done**; `serial_driver.md` lands at Step 3 |
| B0-13 | All CI quality gates pass, plus `cargo xtask qemu-test` on Linux | Full CI matrix + Linux-only `qemu-test` CI job — **done at Step 4** |

**Estimated effort:** Three to four weeks. The boot stub, Limine integration, *and* the QEMU test plumbing are the biggest risks. Plan for the QEMU test infrastructure to take a meaningful slice of the milestone — it's reused for every later kernel milestone, so investing in it once pays off.

**Status:** In progress.
- **Step 1 (boots and halts):** Done. Kernel boots in QEMU via Limine UEFI, prints banner to COM1 serial, halts. See commit `e8828fb`.
- **Step 2 (Kernel HSM):** Done. `frame/kernel.frs` compiles into the `no_std` kernel (framec issue #31, which had hardcoded `std::` paths, is fixed — framec now emits `alloc::`/`core::`). `kmain` calls `Kernel::__create()`, which synchronously drives the boot chain through all five init phases to `$Running`. Validated end-to-end by the `boot_hsm_runs_init_chain_b0` QEMU smoke test **and** host-target tests in the new `frame-os-kernel-tests` crate (snapshot B0-1; behavioral B0-3 + `$Running` panic variant). Per-system doc and SVG committed. **Caveat on B0-4:** boot-child panic-forwarding isn't externally observable with the synchronous boot chain (see B0-4 row + the kernel doc's Open questions); deferred, not faked.
- **Step 3 (SerialDriver FSM):** Not started. Will replace the inline `serial::*` calls in `Kernel`'s actions with a `SerialDriver` Frame system; another `no_std` Frame system, now unblocked. Brings B0-2 + B0-5.
- **Step 4 (QEMU smoke test harness):** Done. `cargo xtask qemu-test` boots the kernel headlessly, captures serial to file, asserts substrings appear and no panic markers do. Two tests: `boot_prints_banner_b0` (banner) and `boot_hsm_runs_init_chain_b0` (full HSM chain in order). Wired into CI as a Linux-only `qemu-test` job.
- B0 is **not complete** until Step 3's `SerialDriver` lands (B0-2, B0-5) with its own snapshot + behavioral tests.

### B1 — multitasking scheduler

**Scope:** B0 plus a scheduler running multiple Frame-defined tasks cooperatively, with the timer driving scheduling decisions.

**Tasks bundled into the kernel image:**
- `BlinkerTask` — toggles a "virtual LED" (prints a character every N ticks) to demonstrate periodic execution
- `ConsoleTask` — owns the serial console; other tasks send messages to it via a shared queue
- `WatchdogTask` — pets a watchdog if other tasks are healthy; would panic on a stuck task (mostly demonstrative; no real watchdog hardware in QEMU)

**Frame systems:** `Kernel` (extended with `$Running` state), `Scheduler` (new), `Task` (new — three instances at runtime), `KernelTimer` (new — or collapsed to plain Rust per the architecture-doc note).

**Native components:**
- Cooperative context switching (since tasks yield voluntarily, this is a simple stack swap, much simpler than preemptive switching with full register save/restore)
- Per-task kernel stacks
- A simple message queue for inter-task communication

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B1-1 | `Scheduler` state graph matches committed design (`$Idle → $PickingNext → $Running → $ContextSwitching`) | Snapshot `scheduler_state_graph_snapshot` |
| B1-2 | `Task` state graph matches committed design (`$Created → $Ready → $Running → $Ready` cycle; `$Blocked`, `$Terminated` declared but unexercised at B1) | Snapshot `task_state_graph_snapshot` |
| B1-3 | `KernelTimer` state graph if implemented (`$Stopped → $Calibrating → $Running`), or collapsed-to-Rust decision documented if not | Snapshot `kernel_timer_state_graph_snapshot` (if Frame system) OR per-system doc explains the deferral |
| B1-4 | `Scheduler.tick()` correctly cycles through ready tasks | Behavioral tests in `kernel/tests/scheduler_behavior.rs` covering pick-next, context-switch, idle |
| B1-5 | `Task` transitions correctly via interface events (`make_ready`, `yield_now`, `block`, `unblock`) | Behavioral tests in `kernel/tests/task_behavior.rs` — one per committed state-event pair |
| B1-6 | `Scheduler` + `Task` compose: scheduler invoked with N tasks runs each in turn | Integration test in `kernel/tests/scheduler_task_integration.rs` |
| B1-7 | Three tasks run concurrently in QEMU, each visibly producing output | QEMU smoke `three_tasks_run_concurrently_b1`; the output assertion is "each task's identifying character appears at least N times in serial output" |
| B1-8 | Tasks yield via explicit `yield_now()` calls, not preemption (no timer-driven preemption at B1) | Code review check; preemption code absent from `kernel/src/arch/`; QEMU smoke confirms scheduler doesn't preempt a tight loop |
| B1-9 | `BlinkerTask`, `ConsoleTask`, `WatchdogTask` each have per-system docs (lightweight — they're instance examples, not generic systems) | Review check; `docs/systems/blinker_task.md`, etc. |
| B1-10 | Updated diagrams for `Scheduler`, `Task`, `Kernel` (showing `$Running` child) committed and current | `cargo xtask check-diagrams` |
| B1-11 | All CI quality gates pass, plus QEMU smoke tests on Linux | Full CI matrix + Linux kernel CI |

**Estimated effort:** Four to six weeks. This is the milestone where the kernel feels like a kernel.

### B2 — interactive shell over serial

**Scope:** B1 plus a shell state machine running as a kernel task, reading from and writing to the serial console. Same `Shell` and `Parser` Frame systems as the hosted shell, with different action implementations.

**Builtins available:**
- `help` — list commands
- `tasks` — list running tasks and their states (introspection of the scheduler)
- `ticks` — show uptime in timer ticks
- `mem` — show memory statistics
- `echo` — echo arguments
- `panic` — deliberately panic the kernel (for demoing the panic handler)

**Frame systems:** `Shell` (reused from H1 — identical `.frs`), `Parser` (reused from H1 — identical `.frs`), with bare-metal action implementations supplied in the kernel crate.

**Native components:** Serial input handling (UART receive interrupt), command-line buffer in `heapless::String`.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B2-1 | The exact same `frame/shell.frs` and `frame/parser.frs` source files compile into both the hosted and kernel binaries | Cargo build success on both `shell` crate (host) and `kernel` crate (target `x86_64-unknown-none`); a `cargo xtask check-frame-source-shared` task asserts the file hashes match what both crates compile against |
| B2-2 | The bare-metal `Shell` snapshot matches the hosted one for state structure (same states, transitions) | Snapshot file is shared / cross-referenced; `kernel/tests/state_graphs.rs::shell_state_graph_matches_hosted` asserts byte equality with the hosted snapshot |
| B2-3 | Typing at the QEMU serial console produces a working REPL | QEMU smoke `serial_shell_responds_to_help_b2` sends "help\n" via QEMU serial, asserts builtin list appears |
| B2-4 | `tasks` builtin lists running kernel tasks with their `Task` state | QEMU smoke `tasks_builtin_lists_running_tasks` |
| B2-5 | `ticks` builtin prints uptime in timer ticks | QEMU smoke `ticks_builtin_increments_over_time` (read twice, second > first) |
| B2-6 | `mem` builtin prints memory statistics | QEMU smoke `mem_builtin_prints_stats` |
| B2-7 | `echo` builtin works identically to the hosted version | QEMU smoke `echo_builtin_in_kernel_matches_hosted` |
| B2-8 | `panic` builtin deliberately panics the kernel (for demoing the panic handler) | QEMU smoke `panic_builtin_triggers_panic_handler` (asserts panic message appears on serial, then kernel halts) |
| B2-9 | `Shell` per-system doc updated with a "Bare-metal action implementations" subsection documenting the kernel-side action bodies | Review check |
| B2-10 | All CI quality gates pass, plus QEMU smoke tests | Full CI matrix + Linux kernel CI |

**Estimated effort:** Two to three weeks.

### B3 — bytecode VM and dynamic programs

**Scope:** B2 plus a bytecode interpreter that can load and execute small programs sent over serial or baked into the kernel image.

**Bytecode format:**
- 32 opcodes — push, pop, add, sub, mul, div, mod, eq, ne, lt, gt, jmp, jz, jnz, call, ret, print_int, print_str, read_char, halt, plus a few others
- Stack-based VM with a 256-entry operand stack
- Programs are flat byte arrays; no separate code/data segments; no relocation
- A small assembler (Python, around 200 lines) translates text mnemonics to bytecode files

**Frame systems:** `Interpreter` (new — the fetch-decode-execute cycle as a state machine), in addition to all previous systems.

**Native components:** Bytecode loading (over serial, with simple framing), the assembler tool.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B3-1 | `Interpreter` state graph matches committed design (`$Fetching → $Decoding → $Exec<Opcode> → $Fetching`, terminal `$Halted` and `$Faulted`) | Snapshot `interpreter_state_graph_snapshot` |
| B3-2 | Each opcode handler transitions correctly: stack effects, flow control (`jmp`, `jz`, `jnz`, `call`, `ret`), arithmetic | Behavioral tests in `kernel/tests/interpreter_behavior.rs` — at least one per opcode in the 32-opcode set, covering both the success path and any failure path |
| B3-3 | `Interpreter` correctly executes a Fibonacci program | Behavioral `interpreter_runs_fibonacci_program` (uses a `.fb` byte array baked into the test); QEMU smoke `kernel_runs_fibonacci_via_run_builtin` |
| B3-4 | The assembler (Python tool) produces bytecode files that match a committed set of golden expected outputs for known input programs | Test script in `tools/assembler/tests/`; runs as a separate `xtask` subcommand |
| B3-5 | `load <name>` builtin reads bytecode over serial and stores it | QEMU smoke `load_then_run_executes_program` (composite test) |
| B3-6 | `run <name>` builtin invokes `Interpreter` on the loaded bytecode and prints its output | Same QEMU smoke as B3-5 |
| B3-7 | Stack overflow / underflow / illegal opcode produce `$Faulted` state, not a kernel panic | Behavioral `interpreter_stack_overflow_transitions_to_faulted`, `interpreter_illegal_opcode_transitions_to_faulted` |
| B3-8 | The `Interpreter` SVG visibly shows the fetch-decode-execute cycle as a closed dispatch loop (Frame argument: the VM literally is a state machine) | Visual review check; the SVG should be readable as a flowchart |
| B3-9 | Assembler tool documented in `tools/assembler/README.md` with the opcode table and bytecode format | Review check |
| B3-10 | Per-system doc for `Interpreter` | Review check; Testing section enumerates per-opcode behavioral tests |
| B3-11 | All CI quality gates pass, plus QEMU smoke tests, plus assembler tests | Full CI matrix |

**Estimated effort:** Three to four weeks. The bytecode VM is the milestone where Frame's role is most direct — the interpreter is implemented as a Frame state machine, with each opcode as a state and the fetch-decode-execute cycle as the dispatch loop.

B3 is the B-track's final committed milestone. Beyond B3, the project considers itself a success in its primary goals.

### B4 — Tier 3: paging, user mode, ELF loading (STRETCH)

**Scope:** B3 plus full Unix-shaped semantics — separate address spaces per process, user-mode execution, ELF binary loading, hardware-enforced isolation, system call dispatch via the `syscall` instruction.

**This is a stretch milestone.** Achieving B3 with strong documentation is a more valuable outcome than reaching a half-finished B4. B4 is the project's most ambitious technical achievement but explicitly optional.

**Frame systems:** `Process` (replacing `Task`), `ProcessTable` (new), `SyscallDispatcher` (new), `ElfLoader` (new), `PageFaultHandler` (new).

**Native components, substantial:**
- Full x86_64 paging (4-level page tables, per-process address spaces)
- Context switching with full register state, FS/GS base, etc.
- ELF parser (the byte-level parsing; the loader state machine is Frame)
- Custom libc and crt0 for user programs
- A custom toolchain configuration for cross-compiling user programs against Frame OS's syscall ABI

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B4-1 | `Process`, `ProcessTable`, `SyscallDispatcher`, `ElfLoader`, `PageFaultHandler` state graphs match committed designs | Snapshots `process_state_graph_snapshot`, `process_table_state_graph_snapshot`, `syscall_dispatcher_state_graph_snapshot`, `elf_loader_state_graph_snapshot`, `page_fault_handler_state_graph_snapshot` |
| B4-2 | `Process` correctly traverses its lifecycle including `$Zombie` and `$Reaped` (which `Task` at B1 didn't exercise) | Behavioral tests in `kernel/tests/process_behavior.rs` per state-event pair |
| B4-3 | `ProcessTable` correctly manages slot reservation, activation, zombie awaiting reap, and free | Behavioral tests in `kernel/tests/process_table_behavior.rs` |
| B4-4 | `SyscallDispatcher` HSM: child states forward errors to `$Active` parent's handlers via explicit `=> $^` | Behavioral `bad_arg_in_validating_forwards_to_active`, `permission_denied_in_executing_forwards_to_active`, `out_of_memory_in_executing_forwards_to_active` |
| B4-5 | `ElfLoader` correctly progresses through load phases, with `$Failed` cleaning up partial work | Behavioral tests in `kernel/tests/elf_loader_behavior.rs`, plus a deliberately-corrupt-ELF test that should land in `$Failed` |
| B4-6 | `PageFaultHandler` correctly classifies stack-grow, copy-on-write, lazy-fault, and unrecoverable cases | Behavioral tests in `kernel/tests/page_fault_handler_behavior.rs` |
| B4-7 | A user-mode hello world (compiled with `cc -ffreestanding -nostdlib -static`) runs on Frame OS | QEMU smoke `hello_world_runs_in_user_mode_b4` |
| B4-8 | The hello world is hardware-isolated: attempting to read kernel memory page-faults; faulting does not crash the kernel | QEMU smoke `user_mode_kernel_memory_read_page_faults`, `user_mode_fault_does_not_crash_kernel` |
| B4-9 | A custom libc + crt0 build is reproducible from sources in the repo | Build script in `userspace/` builds the libc and crt0 deterministically; CI exercises the build |
| B4-10 | The syscall ABI is documented (interface table: syscall number, args, return) | `docs/syscall_abi.md` (new) — review check |
| B4-11 | Per-system docs for all five new Frame systems | Review check |
| B4-12 | All CI quality gates pass, plus full QEMU smoke set including user-mode tests | Full CI matrix |

(A side-by-side write-up comparing the `Process` state graph to equivalent Linux kernel code is listed as a validation milestone in [`vision.md`](vision.md), not a technical milestone here. The kernel can be technically complete without the write-up.)

**Estimated effort:** Many months. Possibly the project's entire next phase rather than a single milestone.

## Dependency graph between milestones

The track-internal dependencies are sequential: H0 → H1 → H2 → H3, and B0 → B1 → B2 → B3 → B4.

The cross-track dependencies are:
- **B2 depends on H1 in *spirit*, not strictly.** The `Shell` and `Parser` Frame systems are first written for the hosted track where iteration is faster. By the time we reach B2 we want those systems to be stable, which means H1 should be done. H2 and H3 add features to the hosted shell that don't propagate back to the bare-metal version, so B2 doesn't depend on them.
- **B3 depends on B2.** The interpreter is exposed through the shell's `run` builtin.
- **B4 stands alone in dependency terms** but builds on all previous bare-metal work.

A reasonable execution order:

1. H0 (a few days)
2. H1 (1-2 weeks) — produces shared Frame systems
3. B0 (2-3 weeks) — kernel boots in QEMU
4. H2 (1 week) — extends hosted shell with external command execution
5. B1 (4-6 weeks) — multitasking kernel
6. B2 (2-3 weeks) — bare-metal shell using shared Frame systems
7. H3 (2-3 weeks) — completes hosted track with job control
8. B3 (3-4 weeks) — bytecode VM
9. [Project's primary scope complete. Decision point: continue to B4, or declare success at B3 and write up.]
10. B4 (months) — optional Tier 3 extension

The reordering above (B2 before H3) reflects the dependency: B2 needs H1 done, not H3. Running B2 earlier surfaces any bare-metal-specific issues with the shared Frame systems sooner, while there's still time to fix them in both tracks.

Total estimated effort to B3, summing the milestones: **roughly 4 to 6 months** of focused work, depending on pace and how much slips. Real projects always run over their estimates; the wider range allows for that. B4 if pursued adds another 6-12 months and should be treated as a separate phase of the project.

## Testing across milestones

Test coverage is a continuous concern, not a milestone of its own. Every milestone that introduces a Frame system or a major native module is expected to land with:

- A state-graph snapshot test for any new Frame system (Level 2 in [`testing.md`](testing.md))
- Behavioral tests covering the committed state-event pairs (Level 3)
- Integration tests where systems compose with each other (Level 4)
- QEMU smoke tests for any new bare-metal behavior (Level 7)
- A per-system doc following [`systems/_template.md`](systems/_template.md), with its Testing section filled in

The test infrastructure is bootstrapped at H0 (workspace `cargo test`, `insta` snapshots, `assert_cmd` E2E) and extended at B0 (QEMU smoke test runner). After that, each milestone *uses* the infrastructure rather than building it.

A milestone whose Frame systems lack the expected test coverage is not "done" even if the code works. The vision doc commits to documented systems with documented test coverage; the roadmap honors that commitment by treating tests as a milestone deliverable rather than a follow-up.

## Out of scope

For completeness, things that are *not* on this roadmap and won't be unless the project's scope is explicitly expanded later:

- Networking. No TCP/IP stack, no network drivers.
- USB. No USB host or device support.
- Filesystem on disk. The kernel image's bundled file table is the only "filesystem."
- Multi-core / SMP. All bare-metal targets are single-core.
- GUI. No framebuffer console, no graphics, serial is the only I/O.
- Process forking. B4 supports `exec()` to start fresh processes but not `fork()` to clone existing ones.
- Threads. One thread per process.
- Comprehensive signal handling. SIGKILL only.
- Power management, suspend/resume.
- Audio, video, GPU.
- Virtualization, container support.

Adding any of these would multiply the project's size without strengthening its central argument. Some may make sense as follow-on projects once the core Frame OS is established. None are committed in this roadmap.

## How the roadmap will be maintained

This file is updated as milestones are completed and as scope decisions change. Each milestone gets a "status" annotation as it progresses: `planned`, `in-progress`, `done`, or `deferred`. When a milestone is `done`, the criteria above should be verifiable by anyone who builds and runs Frame OS.

Decisions to expand or contract scope are documented here, with reasoning. If B4 is dropped, this file should explain why. If a new milestone (say, "B5 — port to Pi 4") is added, this file should explain its goals and dependencies.

The roadmap is a project artifact, not a marketing document. It should be accurate enough that someone reading it knows what the project is, what it isn't, and what's actually working today.

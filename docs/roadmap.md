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

> **Scope re-baseline (2026-05-20).** Track B was originally a Frame
> *demonstration*: cooperative scheduling, a bytecode VM, with user mode /
> processes as an optional stretch (old B4). It has been re-baselined to a
> **real-OS-class project** — preemptive multitasking, user mode +
> processes + `fork`/`exec` as core, real virtual memory, an on-disk
> filesystem, a TCP/IP networking stack, USB, and SMP. The goal is twofold:
> build something genuinely impressive (xv6-class and beyond), and
> maximally **stress-test Frame** on hard, protocol- and lifecycle-heavy
> subsystems — TCP especially. The bytecode VM (old B3) is removed from the
> core path; real ELF user programs replace it. Several items formerly
> "out of scope" (networking, USB, on-disk FS, SMP, user mode, `fork`) are
> now committed milestones (B3–B7).
>
> Each milestone deliberately pairs a **native substrate** (the unsafe
> plumbing where Frame doesn't help) with a **Frame payload** (the
> lifecycle/protocol showcase), and names the **framec capability it is
> expected to stress** — so this roadmap doubles as the Frame stress-test
> plan. Near-term milestones (B1) are specified precisely; far ones
> (B5–B7) name the Frame systems and expected framec gates but finalize
> exact test paths when the milestone begins (as B0's tests did when they
> moved from `kernel/tests/` to the xtask harness). The deepest framec
> gates cluster at **B4** (the deferred-event queue, born from the first
> device-completion interrupt), **B5** (timed transitions, orthogonal
> regions, history, scale — TCP), and **B7** (`Send`+`Sync` codegen).

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
| B0-2 | `SerialDriver` state graph matches committed design | Snapshot `serial_driver_state_graph_snapshot` in `kernel-tests/tests/state_graphs.rs` — **done at Step 3.** Design revised from the originally-specced `$Idle → $Transmitting → $Draining → $Idle` to a minimal `$Uninitialized → $Ready` init-gate: QEMU serial is synchronous, so transmit/drain states would have no behavior behind them at B0. They become real on an interrupt-driven hardware track and get added then. See `docs/systems/serial_driver.md` "Why a state machine" |
| B0-3 | `Kernel` boot HSM correctly progresses through init phases | Behavioral `boot_chain_prints_all_phases_in_order` + `fresh_kernel_runs_boot_chain_to_running_not_done` in `kernel-tests/tests/kernel_behavior.rs` (host build); also `boot_hsm_runs_init_chain_b0` QEMU smoke — **done at Step 2** |
| B0-4 | `Kernel.kernel_panic()` dispatches per-state (Frame argument) | `$Running`'s variant covered by `panic_in_running_prints_runtime_message_and_halts` + `runtime_panic_uses_running_variant_not_boot_variant`. **Boot-child forwarding (`=> $^`) is not externally observable** with the synchronous boot chain — `__create()` runs the chain to `$Running` so no external event reaches a boot child. Recorded as an Open question in `docs/systems/kernel.md`; testing it directly needs a fault-injection hook or an event-stepped boot chain (design decision, deferred to when a boot phase first actually fails) |
| B0-5 | `SerialDriver` correctly gates writes on its init state | Behavioral tests in `kernel-tests/tests/serial_driver_behavior.rs` — `write_before_init_is_dropped`, `init_transitions_to_ready`, `write_line_after_init_emits_text_and_newline`, etc. — **done at Step 3** |
| B0-6 | `cargo xtask qemu` boots the kernel image in QEMU x86_64 (no automated assertion; manual smoke) | **Manual** — maintainer runs the command, observes banner on serial console, halts cleanly |
| B0-7 | `cargo xtask qemu-test` runs the kernel image in QEMU, captures serial output, and exits 0 on success / non-zero on assertion failure | `cargo xtask qemu-test` itself, exercised in CI on Linux only (QEMU is most reliable there) — **done at Step 4** |
| B0-8 | The kernel banner appears on serial output during a QEMU boot | QEMU smoke test `boot_prints_banner_b0` (Level 7, in `xtask/src/main.rs`'s `SMOKE_TESTS` table) — **done at Step 4** |
| B0-9 | The kernel halts cleanly (returns to `hlt` loop) after init | Covered indirectly: the smoke tests assert the boot chain completes (`[run] kernel running`) with no panic/triple-fault markers, and QEMU stays alive (it's SIGKILLed at timeout, not crashed). A dedicated `kernel_halts_cleanly_b0` with `isa-debug-exit` exit-code assertion lands once a `smoke-test` Cargo feature gates the kernel's `isa-debug-exit` path (deferred — see the smoke-test module comment in `xtask/src/main.rs`) |
| B0-10 | The boot sequence is the HSM, not a script of init calls (Frame argument check) | Code review: `kmain` calls `Kernel::__create()` and lets the HSM drive; no manual sequence of init steps. **Done at Step 2** — `kernel/src/main.rs` has no init-call script; the `-> $NextPhase` transitions in `frame/kernel.frs` encode the order |
| B0-11 | `Kernel` and `SerialDriver` SVG diagrams committed and current | `cargo xtask check-diagrams` — both **done** (`kernel.svg`, `serial_driver.svg`) |
| B0-12 | Per-system docs for `Kernel` and `SerialDriver` exist and follow the template | Review check — both **done** (`docs/systems/kernel.md`, `serial_driver.md`) |
| B0-13 | All CI quality gates pass, plus `cargo xtask qemu-test` on Linux | Full CI matrix + Linux-only `qemu-test` CI job — **done at Step 4** |

**Estimated effort:** Three to four weeks. The boot stub, Limine integration, *and* the QEMU test plumbing are the biggest risks. Plan for the QEMU test infrastructure to take a meaningful slice of the milestone — it's reused for every later kernel milestone, so investing in it once pays off.

**Status:** Functionally complete (all four steps done; see per-step notes and the B0-* rows for the few documented deferrals).
- **Step 1 (boots and halts):** Done. Kernel boots in QEMU via Limine UEFI, prints banner to COM1 serial, halts. See commit `e8828fb`.
- **Step 2 (Kernel HSM):** Done. `frame/kernel.frs` compiles into the `no_std` kernel (framec issue #31, which had hardcoded `std::` paths, is fixed — framec now emits `alloc::`/`core::`). `kmain` calls `Kernel::__create()`, which synchronously drives the boot chain through all five init phases to `$Running`. Validated end-to-end by the `boot_hsm_runs_init_chain_b0` QEMU smoke test **and** host-target tests in the new `frame-os-kernel-tests` crate (snapshot B0-1; behavioral B0-3 + `$Running` panic variant). Per-system doc and SVG committed. **Caveat on B0-4:** boot-child panic-forwarding isn't externally observable with the synchronous boot chain (see B0-4 row + the kernel doc's Open questions); deferred, not faked.
- **Step 3 (SerialDriver FSM):** Done. `frame/serial_driver.frs` — a minimal `$Uninitialized → $Ready` init-gate (design revised from the speculative transmit/drain graph; see B0-2). Held in `Kernel`'s `console` domain; `$InitConsole` runs `console.init()`, and `$LaunchInit`/`$Running` route output through it (early-boot + panic/halt stay raw). Snapshot + 7 behavioral tests in `kernel-tests`; per-system doc + SVG committed. Proves the `Kernel`→child composition and the "shared `.frs`, different native `serial` actions per target" pattern (kernel COM1 vs host capture).
- **Step 4 (QEMU smoke test harness):** Done. `cargo xtask qemu-test` boots the kernel headlessly, captures serial to file, asserts substrings appear and no panic markers do. Two tests: `boot_prints_banner_b0` (banner) and `boot_hsm_runs_init_chain_b0` (full HSM chain in order). Wired into CI as a Linux-only `qemu-test` job.
- **B0 is functionally complete.** All four steps done; exit criteria B0-1 through B0-13 are met or explicitly accounted for (B0-4's boot-child forwarding is a documented, deferred design item; B0-6 is the manual smoke; B0-9's dedicated isa-debug-exit test is deferred behind a Cargo feature). Remaining B-track work is B1+ (scheduler), not B0.

### B1 — preemptive multitasking

**Scope:** B0 plus a **preemptive** scheduler running multiple kernel threads. A periodic timer interrupt drives context switches — a thread in a tight loop is preempted, not relying on voluntary yield.

**The native/Frame split (and why the deferred-event queue is *not* here).** Preemption is mostly native: the switch *must* happen inside the timer ISR (a tight-loop thread never reaches any other safe point), so the ISR saves full register state, picks the next thread from a **native ready-queue**, and swaps stacks — it never calls a Frame system (Frame dispatch is non-reentrant). The Frame `Scheduler` and `Task` are touched only from **normal context** (admit, block, unblock, exit), behind a short ISR-safe lock on the shared ready-queue. Consequently the **deferred-event queue moves to B4** — its first hard requirement is a *device-completion interrupt* that must deliver an event into a possibly-in-flight Frame system; B1's preemption doesn't need it. (Correction to the original B1 framing; see `docs/plans/b1.md`.)

**Frame systems (deliberately minimal — the honest B1 forms):**
- `Scheduler` — `$Idle` (no runnable threads → the main loop `hlt`s) / `$Active` (≥1 runnable). *Not* the speculative `$Idle → $PickingNext → $Running → $ContextSwitching`: picking and switching are native ISR work, so the only genuinely-different-behavior states are halt-vs-run. Grows real states at B3 (blocking/waiting/zombie). Same "model the invariant that exists" call as SerialDriver.
- `Task` — `$Created → $Ready ⇄ $Blocked → $Terminated`. **No `$Running`:** "currently on the CPU" flips every tick and would fire from the ISR (forbidden) — it's native (`current_thread`), not a Frame state. `Task` models the coarse lifecycle that changes in normal context.

**Native components:**
- IDT + exception handlers (faults print + halt, not silent triple-fault); 8259 PIC remap + PIT channel 0 periodic (~100 Hz). Reuse Limine's GDT; TSS deferred to B3 (no ring switch yet).
- **Preemptive** context switch — save/restore the *complete* register state from interrupt context, per-task 16 KiB static kernel stacks, fresh-task stack-frame crafting.
- Native ready-queue + `current_thread`, behind a short interrupt-safe lock (the first taste of kernel concurrency: ISR vs normal context).

**framec gate expected:** modest at B1 — mainly a check on whether the `Scheduler` FSM earns its keep at pure round-robin (if it's only `$Idle`/`$Active`, that's accepted per the B1 design decision; reassessed at B3). The deep gates (the queue, `no-alloc`) move to B4/B5/B7.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B1-1 | `Scheduler` (`$Idle`/`$Active`) and `Task` (`$Created→$Ready⇄$Blocked→$Terminated`) state graphs match committed designs | Snapshots `scheduler_state_graph_snapshot`, `task_state_graph_snapshot` (`kernel-tests`) |
| B1-2 | `Task` transitions correctly per committed state-event pair (no `$Running`) | Behavioral tests in `kernel-tests/tests/task_behavior.rs` (host) |
| B1-3 | `Scheduler` flips `$Idle`↔`$Active` correctly on `task_ready`/`task_unready` and reports `is_idle` | Behavioral tests in `kernel-tests/tests/scheduler_behavior.rs` (host) |
| B1-4 | A thread that **never yields is preempted** (distinguishes from cooperative) | QEMU smoke `preemption_b1` — two non-yielding threads print interleaved (`...121212...`), only possible via timer preemption — **done at Step 3c** |
| B1-5 | Multiple kernel threads run concurrently, each visibly producing output | QEMU smoke `preemption_b1` (both `1` and `2` appear) — **done at Step 3c** |
| B1-6 | The scheduler halts in `$Idle` when nothing is runnable | QEMU smoke `preemption_b1`: both workers exit → the Frame `Scheduler` reaches `$Idle` (`is_idle()` read from the kernel's idle loop drives the halt) — **done.** Also the cooperative `context_switch_ping_pong_b1` and `interrupts_and_timer_b1` |
| B1-7 | Diagrams + per-system docs for `Scheduler` and `Task` | `cargo xtask check-diagrams`; `docs/systems/scheduler.md`, `docs/systems/task.md` — **done** |
| B1-8 | All CI gates pass, plus QEMU smoke on Linux | Full CI matrix + `qemu-test` (5/5) — **done** |

**Status:** Done. Preemptive multitasking works on bare metal (`cccf131`): the timer ISR full-frame-switches between non-yielding threads, threads exit, and the Frame `Scheduler` (`$Idle`/`$Active`) drives the kernel's idle-halt under interrupt-off critical sections. Steps: 1 (Frame layer, host-tested, `6996be7`), 2 (cooperative switch, `162f3e5`), 3a/3b (IDT + PIC/PIT timer, `a783c71`), 3c (preemption, `cccf131`), completion (load-bearing Scheduler + idle-halt + docs).
- **Honest scope:** `Task` is host-validated but not wired into the kernel — it's load-bearing as `Process` at B3 (decorative at B1, so omitted per discipline). `$InitIDT`/`$InitTimer` still print as stubs (native init runs in `kmain`); wiring them into the HSM phases is a tracked refinement, not a B1 exit criterion.

**Estimated effort:** Large. The preemptive context switch (saving full state from interrupt context and resuming a different thread) is the classic hard part; this is where the kernel first feels like a kernel. The Frame payload is small and honest — the substance of the milestone is native.

### B2 — virtual memory & address spaces

**Scope:** B1 plus real memory management. A physical frame allocator and 4-level paging, with the kernel in its own address space and the machinery to construct per-process address spaces (consumed at B3). Demand paging and copy-on-write fault handling.

**Frame systems:**
- `PageFaultHandler` — HSM `$Classifying → $StackGrow | $CopyOnWrite | $LazyFault | $Fatal`; the parent catches the unrecoverable case and routes to process kill via `=> $^`.

**Native components:** physical frame allocator (bitmap or buddy); 4-level page tables with map/unmap/translate; address-space construction and teardown; the page-fault interrupt entry that frames the fault as an event into `PageFaultHandler`.

**framec gate expected:** *HSM forwarding* (fault → fatal → kill via `=> $^`); *transition guards* (the fault-classification predicates — present/write/user bits → which child state).

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B2-1 | `PageFaultHandler` state graph matches committed design | Snapshot `page_fault_handler_state_graph_snapshot` (`kernel-tests`) — **done at Step 3** (`$Classifying → $LazyFault \| $Fatal` under `$FaultActive`; `$StackGrow`/`$CopyOnWrite` deferred to B3/B4) |
| B2-2 | Fault classification is correct: lazy-fault recovers, OOM/non-lazy → fatal | Behavioral tests in `kernel-tests/tests/page_fault_handler_behavior.rs` (5, via the `vm` test-double) — **done at Step 3.** (stack-grow / COW arrive with B3/B4) |
| B2-3 | Physical frame allocator: alloc / free / realloc | `frames` self-test; QEMU smoke `frame_allocator_b2` — **done at Step 1** |
| B2-4 | Paging: map → write → translate → unmap | QEMU smoke `paging_b2` (write cross-checked via HHDM) — **done at Step 2** |
| B2-5 | A demand-paged region faults in; an unmapped access → `$Fatal` without crashing | QEMU smoke `page_fault_demand_b2`, `page_fault_fatal_b2` — **done at Step 3** |
| B2-6 | Diagrams + per-system doc for `PageFaultHandler` | `cargo xtask check-diagrams`; `docs/systems/page_fault_handler.md` — **done at Step 3** |
| B2-7 | All CI gates pass, plus QEMU smoke on Linux | Full CI matrix + `qemu-test` (9/9) — **done** |

**Estimated effort:** Large; mostly native (paging is unsafe-Rust-heavy). The Frame payload is concentrated in `PageFaultHandler`.

**Status:** Done. Frame allocator (`7385208`), 4-level paging (`3835155`), `#PF` + `PageFaultHandler` HSM (`f25b5d0`), per-process address spaces (`3363eb1`), and `$InitMemory` made real (this step). B2-1 through B2-7 met; 10/10 QEMU smoke.
- **`$InitMemory` is now real** — the boot HSM phase brings up the frame allocator (`crate::frames::init()`) instead of printing a stub; the same "shared `.frs`, different native actions per target" pattern means `kernel-tests` supplies a no-op `frames::init()` double so the host behavioral tests still drive the boot chain.
- **Remaining cross-cutting refinement (not a B2 criterion):** `$InitIDT`/`$InitTimer` still print stubs (native IDT/PIC/PIT init runs in `kmain`); folding those into their HSM phases is a small follow-up. B3 (user mode) now has everything it needs — paging, address spaces, the `#PF` handler — and is next.

### B3 — user mode, processes, syscalls, ELF, fork/exec

**Scope:** B2 plus the user/kernel boundary — the defining feature of a real OS. Ring-3 execution, the `syscall`/`sysret` fast path, per-process address spaces, ELF binary loading, and `fork`/`exec`. Basic signals (at least `SIGKILL`, `SIGSEGV` from a fatal fault, `SIGCHLD` on child exit). This is the milestone that crosses the xv6 bar.

**Frame systems:**
- `Process` — HSM `$Created → $Ready → $Running → $Blocked → $Zombie → $Reaped` (replaces `Task`; state-dependent `kill()` per the architecture doc).
- `ProcessTable` — slot lifecycle: reserve → activate → zombie-awaiting-reap → free.
- `SyscallDispatcher` — HSM `$Validating → $Executing` under a `$Active` parent that catches `bad-arg`/`permission-denied`/`out-of-memory` via `=> $^`.
- `ElfLoader` — `$ReadingHeader → $ValidatingHeader → $MappingSegments → $BuildingStack → $Done`; `$Failed` sink cleans up partial work.

**Native components:** ring-3 entry, TSS, `syscall`/`sysret` MSR setup, full register save/restore at the boundary, ELF byte parsing, the syscall ABI, a minimal libc + crt0 for user programs, `fork` (address-space copy / COW) and `exec`.

**framec gate expected:** *scale* — one `Process` instance per process (does the `Rc`/`Vec`/`BTreeMap` machinery hold up at dozens–hundreds of instances?); *HSM depth + forwarding* (`SyscallDispatcher`); the `ElfLoader` `$Failed` partial-cleanup funnel.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B3-1 | `Process`, `ProcessTable`, `SyscallDispatcher`, `ElfLoader` state graphs match committed designs | Snapshots (`kernel-tests`) — **`SyscallDispatcher` (Step 2), `Process`/`ProcessTable` (Step 3), `ElfLoader` (Step 4) all done** (`*_state_graph_snapshot`) |
| B3-2 | `Process` traverses its full lifecycle incl `$Zombie`/`$Reaped`; `kill()` is state-dependent | Behavioral `kernel-tests/tests/process_behavior.rs` (host, 11 tests) + `process_table_behavior.rs` (8) — **done at Step 3.** `kill()` is funneled to the `$Alive` parent via `=> $^` and tested from each live state. (No `$Running`: "on the CPU" is native scheduler state — same call as `Task`; see Step 3 notes.) |
| B3-3 | `SyscallDispatcher` forwards errors to `$Active` via `=> $^` | Behavioral `syscall_dispatcher_behavior.rs` (host, 5 tests incl `unknown_syscall_is_rejected_via_parent`) — **done at Step 2.** Multiple error *classes* (bad-arg/perm/OOM) arrive with the richer ABI at B4; B3 funnels the one `ENOSYS` path through `$Active.reject` |
| B3-4 | `ElfLoader` loads a valid ELF; a corrupt ELF lands in `$Failed` with cleanup | Behavioral `elf_loader_behavior.rs` (host, 6 tests: valid → `$Done`; truncated/bad-magic/wrong-machine/non-exec → `$Failed`) + QEMU `ring3_syscall_b3` loads the real baked ELF — **done at Step 4a** |
| B3-5 | A user-mode hello-world runs (ring 3, via `exec`) | QEMU smoke `hello_world_runs_in_user_mode_b3` |
| B3-6 | Hardware isolation: a user read of kernel memory page-faults and does **not** crash the kernel | QEMU smoke `user_fault_does_not_crash_kernel_b3` (ring-3 read of the kernel half → `$Killing` → process killed, kernel survives) + behavioral PFH user-fault tests — **done at Step 4b** |
| B3-7 | `fork` + `exec` spawns a child that runs independently; parent reaps via wait | **Done** — QEMU smoke `fork_concurrency_b3` (5b), `exec_b3` (5c, child execs `hello`), `wait_reap_b3` (5d, parent blocks in `wait`, reaps child with status, frees its address space) |
| B3-8 | The syscall ABI is documented | **Done** — [`docs/syscall_abi.md`](syscall_abi.md) (convention, syscalls 0–4, process model, basic signals) |
| B3-9 | Per-system docs for the four new systems; diagrams current | **Done** — `syscall_dispatcher.md`, `process.md`, `process_table.md`, `elf_loader.md` (+ SVGs); `cargo xtask check-diagrams` clean |
| B3-10 | All CI gates pass, plus QEMU smoke on Linux | **Done** — fmt, kernel + host clippy `-D warnings`, host tests, check-diagrams, `qemu-test` (16/16) |

**Estimated effort:** Very large. Ring transitions, the syscall boundary, `fork`/COW, and ELF loading are each substantial. This is the xv6-class core.

**Status:** Done. The milestone that crosses the xv6 bar: ring 3, the `syscall`/`sysret` boundary, per-process address spaces, ELF loading, hardware isolation, and **preemptive multitasking with `fork`/`exec`/`wait`** all work on bare metal. Commits `a324908` (Step 2) → `435f9e7` (5c) → 5d. 16/16 QEMU smoke. The four B3 Frame systems (`SyscallDispatcher`, `Process`, `ProcessTable`, `ElfLoader`) are implemented, host-tested, and load-bearing; signals are native bookkeeping (`docs/syscall_abi.md`).
- **Step 1 (user/kernel boundary):** Done. Our own GDT + TSS (`gdt.rs`, far-return reload via `retfq`, `ltr`), the `syscall`/`sysret` MSR fast path (STAR/LSTAR/FMASK + EFER.SCE), full register save/restore at the boundary, and ring-3 entry/exit (`iretq` down, `longjmp` back up). A hand-assembled user blob writes two bytes and `exit(42)`s through the syscall path. Validated by QEMU smoke `gdt_loaded_b3` and `ring3_syscall_b3`.
- **Step 2 (`SyscallDispatcher` HSM):** Done. `frame/syscall_dispatcher.frs` — `$Validating → $Executing` under a `$Active` parent, with the unknown-syscall path forwarding `self.reject(ENOSYS)` to `$Active.reject` via `=> $^` (the HSM forwarding showcase; relies on lang-reference §9.5 self-event-send). Compiled into the kernel (the ring-3 demo routes every syscall through a global `SyscallDispatcher`) and host-tested in `kernel-tests` (snapshot B3-1 + 5 behavioral B3-3). Per-system doc + SVG committed.
- **Smoke-harness hardening (Step 2):** the kernel's `halt_forever()` now writes QEMU's `isa-debug-exit` (port 0xf4) so a healthy boot exits the VM promptly instead of racing the timeout; each smoke test gets a fresh OVMF NVRAM copy (a SIGKILL-corrupted vars file can no longer cascade into the next boot); and a `task_unready` scheduler race in `sched::exit_current` (mark-dead and notify must be one critical section) was fixed. Result: 12/12 QEMU smoke, reliably.
- **Step 3 (`Process` + `ProcessTable`):** Done. `frame/process.frs` — `$Created → $Ready ⇄ $Blocked → $Zombie → $Reaped`, with `kill()` funneled to a `$Alive` parent via `=> $^` (load-bearing, traversed from each live state). `frame/process_table.frs` — a `JobControl`-style manager holding `Vec<Process>`, `$HasCapacity ⇄ $Full` under a `$Managing` parent that owns the by-pid operations. **Decisions:** `Process` omits `$Running` (matches `Task`/`Scheduler` — "on the CPU" is native ISR state, not a Frame transition; `architecture.md` annotated); `ProcessTable` is manager-of-instances, not the per-slot `$Free→$Reserved→…` machine (those duplicate the `Process` lifecycle and `$Reserved` is a Step-5 fork concern). Wired load-bearing: the single ring-3 program is spawned (`$Ready`) → runs → `exit` syscall → `$Zombie` → reaped (`$Reaped`, slot freed). Host-tested (snapshots B3-1; 11 + 8 behavioral B3-2) and validated end-to-end by `ring3_syscall_b3`'s `[proc]` markers. Per-system docs + SVGs committed.
- **Step 4a (`ElfLoader` + a real user program):** Done. A new standalone, workspace-excluded `user/` crate (freestanding Rust, raw syscalls, custom linker script → static `ET_EXEC` at `0x1000_0000`) compiles to an ELF that `kernel/build.rs` bakes into the kernel image. `frame/elf_loader.frs` — a flat phase pipeline `$ReadingHeader → $ValidatingHeader → $MappingSegments → $BuildingStack → $Done`, every phase routing failure to one `$Failed` sink that rolls back partial mappings (the `$Failed`-funnel showcase; cleanup written once). `crate::elf` does the ELF64 parsing + PT_LOAD mapping. The hand-asm `USER_BLOB` is gone: the demo now loads + runs the real ELF (`hello from ELF`, exit 42) through the full `Process` lifecycle. **Also fixed a real syscall-ABI bug** the blob had masked: the entry stub only preserved `rcx`/`r11`, but the `call syscall_dispatch` clobbers the caller-saved set — the compiled program kept loop state in registers across the syscall and looped forever. The stub now preserves all user registers except `rax`/`rcx`/`r11`, per the ABI. Host-tested (snapshot B3-1; 6 behavioral B3-4) and validated by `ring3_syscall_b3`'s `[elf]` marker + the ELF's own output.
- **Step 4b (hardware isolation):** Done. A second baked user program (`faulter`) reads a kernel-half address from ring 3 → `#PF` with the U/S bit set. `PageFaultHandler` gained `$Killing` + `recover()`, and its long-declared `$FaultActive` `=> $^` funnel is **finally load-bearing**: both recovery children self-send `unrecoverable()`, forwarded `=> $^` to `$FaultActive.unrecoverable`, which decides once — ring-3 fault → `$Killing` (kill the `Process`, longjmp back to the kernel), kernel fault → `$Fatal` (halt). The faulter is killed (reaped, exit -1) and the kernel keeps running to the deliberate kernel-fault demo. Host-tested (9 PFH behavioral incl. user-kill + recover; updated snapshot) and validated by `user_fault_does_not_crash_kernel_b3`. **B3-6 met.**
- **Step 5a (the multitasking core — user processes as scheduled entities):** Done. The decisive rearchitecture: user programs are no longer run one-at-a-time synchronously (`enter_user` → run → longjmp). Each is now a real scheduled `Process` with its **own address space (PML4)** and **own ring-0 kernel stack**; the native scheduler (`sched.rs`) switches **CR3 + `TSS.RSP0`** on every context switch, and a process first enters ring 3 via the scheduler's synthetic `iretq` frame (`spawn_user`). It is **preemptible in ring 3** (IF=1; the timer ISR saves/restores its full trap frame across the per-process kernel stack), and it leaves the CPU by marking itself dead + yielding (`exit`/fatal-fault → no longjmp). The syscall stub switches to the current process's kernel stack (`CURRENT_KSTACK`, owned by the scheduler). The kernel higher-half is mirrored into every PML4, so the `mov cr3` in the ISR keeps the executing code + stack mapped. Demo: `hello` (clean exit) and `faulter` (killed via the scheduler) each run as scheduled processes; validated by `ring3_syscall_b3` + `user_fault_does_not_crash_kernel_b3` (13/13). **Found + fixed a real bug:** a user process exits via the syscall path (IF=0), so the dead-task park (`hlt`) hung until `exit_current` was made to explicitly enable interrupts before parking. (Address-space/frame teardown on reap is deferred to Step 5d.)
- **Step 5b (`fork` — concurrent processes):** Done. The syscall stub was reworked to build a **full trap frame** (15 GPRs + the iretq frame, same layout as the timer ISR) and return via `iretq`, so the kernel always has a process's complete user state. `fork` (syscall 2) eager-copies the caller's address space (`paging::fork_address_space` — fresh frames, copied contents, shared kernel higher-half), copies its trap frame with `rax` forced to 0 for the child, and admits the child to the scheduler (`sched::spawn_user_from_frame`); the parent's `fork()` returns the child pid, the child's returns 0. The `forker` demo forks and parent/child print interleaved (`PPCCPPCC…`) — **two user processes running concurrently in separate address spaces**, validated by `fork_concurrency_b3`. **Found + fixed a real bug:** the `exit` syscall diverged *inside* the SyscallDispatcher handler, leaving the shared dispatcher stuck in `$Executing` and corrupting the next process's syscalls; `exit` now records a pending exit and `syscall_dispatch` honors it after the dispatcher returns to `$Validating`. (Child reaping is Step 5d, so the parent's reap leaves the child as a lingering zombie — `table count 1`.)
- **Step 5c (`exec`):** Done. `exec(prog_id)` (syscall 3) replaces the calling process's image: it loads the selected baked program into a fresh address space (`ElfLoader`), points the *same* process at it (`sched::exec_into` updates the TCB's PML4 + switches CR3), and resets the trap frame to the new program's entry/stack — so the syscall's `iretq` returns into the new program. The process keeps its pid + kernel stack. The `spawner` demo is the canonical shell-launch: it `fork`s, the child `exec`s `hello` (becoming hello — prints "hello from ELF" + exits), the parent prints 'S'. Validated by `exec_b3` (15/15). (No filesystem yet, so programs are selected by id; B4 loads from disk. Old-image teardown deferred to Step 5d.)
- **Step 5d (`wait` + signals + teardown — B3 complete):** Done. `wait` (syscall 4) is the one place a syscall **blocks**: a parent with living-but-not-exited children calls `sched::block_current` (mark its TCB `Blocked` + yield; a blocked task stays "alive" in the Frame Scheduler's count so the boot context won't go `$Idle` while a parent waits), and resumes when a child's exit (**SIGCHLD** — `sched::exit_current` wakes a blocked parent) makes it runnable. On wake it reaps the child: `Process → $Reaped`, the scheduler slot is freed, and the child's address space is torn down (`paging::free_address_space`). Like the exit/fork bugs before it, the **blocking happens in `syscall_dispatch` after the dispatcher returns to `$Validating`** (a `PENDING_WAIT` flag), not inside the handler — otherwise the shared dispatcher would stall the child's syscalls. The `waiter` demo forks a child, blocks in `wait`, the child runs (`cccc`) + `exit(7)`, and the parent reaps it with status 7 — validated by `wait_reap_b3` (16/16). **Signals** are native bookkeeping (SIGKILL = `Process.kill`, SIGSEGV = the `$Killing` path, SIGCHLD = the wake), documented in `docs/syscall_abi.md` (B3-8). **B3-7 met; B3 complete.**

### B4 — block device & filesystem

**Scope:** B3 plus persistent storage. A block device driver, a buffer cache, and a real (if minimal) on-disk filesystem with inodes, directories, and a VFS layer. **The shell returns here as a *userspace program*** — the H-track `Shell`/`Parser` `.frs` compiled for user mode (not a kernel task), loading programs from disk via `fork`/`exec`. This is the strongest form of the "same Frame source, host and kernel" demonstration.

**Frame systems:**
- `BlockRequest` — I/O request lifecycle `$Queued → $InFlight → $Complete | $Error`.
- `OpenFile` — `$Open → $Reading/$Writing → $Closed`.
- `Mount` — filesystem mount/unmount lifecycle.
- `Shell`/`Parser` — reused from the H-track, now as a userspace program (bare-metal/userspace action implementations).

**Native components:** virtio-blk (or AHCI) driver + DMA; buffer/page cache; the on-disk FS format (inodes, dirents, free-block bitmap); VFS dispatch.

**framec gate expected:** **the deferred-event queue is born here** — the block device's *completion interrupt* must deliver an event into a possibly-in-flight Frame I/O system, which is the first hard requirement for the `post`/`drain` split (the ISR `post`s a completion; the kernel main loop `drain`s it; the Frame system is never dispatched from interrupt context). Built from this concrete need (interrupt-safe, ideally no-alloc), it becomes the reference for the same pattern at B5 (NIC) and B7 (cross-core). Also: per-inode serialization for concurrent FS operations.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B4-1 | `BlockRequest`, `OpenFile`, `Mount` state graphs match committed designs | Snapshots (`kernel-tests`) — **all three done** (`BlockRequest` Step 1, `Mount` Step 2, `OpenFile` Step 3) |
| B4-2 | I/O request + file + mount lifecycles correct | Behavioral tests (host) — **all three done** (`BlockRequest` 6, `Mount` 5, `OpenFile` 6) |
| B4-3 | create → write → read → delete round-trips; data survives across operations | **Done at Step 2** — QEMU smoke `fs_file_roundtrip_b4` (mounts the mkfs'd disk, reads a baked file, then create/write/read/delete) |
| B4-4 | `mount`/`unmount` work; the FS persists across a reboot of the same disk image | **Done at Step 2** — QEMU smoke `fs_persists_across_reboot_b4` (two boots on one disk: boot 1 writes a marker, boot 2 reads it back) |
| B4-5 | The userspace shell `cat`s a file loaded from disk and runs a program from disk | QEMU smoke `userspace_shell_runs_program_from_disk_b4` — **done at Step 4a** (the shell here is a *scripted, hand-written* raw-syscall program; reusing the `Shell`/`Parser` `.frs` in ring 3 is B4-6/Step 4b) |
| B4-6 | `Shell`/`Parser` per-system docs gain a "userspace action implementations" note; same `.frs` builds for host and userspace | **`Parser` done at Step 4b** — same `frame/parser.frs` compiles for ring 3, proven by QEMU smoke `userspace_frame_parser_reuse_b4`; docs updated. **`Shell` `.frs` reuse still pending** (needs userspace actions + an input device). |
| B4-7 | Diagrams + per-system docs; all CI gates + QEMU smoke | **Done** — all 16 diagrams in sync (`cargo xtask check-diagrams`); per-system docs for the B4 systems written; CI runs fmt + clippy + host tests + kernel cross-build + `qemu-test` (22 smoke) + diagram drift (also fixed: the `kernel-build`/`qemu-test` jobs now install framec, which their build scripts require). |

**Estimated effort:** Very large.

**Status:** In progress.
- **Step 1 (virtio-blk + post/drain + `BlockRequest`):** Done. A legacy virtio-blk driver (`virtio_blk.rs`): PCI discovery (`pci.rs`), feature negotiation, a single virtqueue in contiguous DMA frames (`frames::alloc_contiguous`), and the completion IRQ on vector 43 (slave PIC IRQ 11). **The post/drain deferred-event pattern is born:** the IRQ handler (`on_irq`) only *posts* (acks the device ISR + sets a flag — no Frame dispatch); the kernel *drains* from normal context, reading the used ring and driving the `BlockRequest` Frame system (`$Queued → $InFlight → $Complete | $Error`) to completion. The first **async-interrupt → Frame** boundary (the timer ISR is pure-native; `#PF` is synchronous). Demo: write a pattern to a sector, read it back, verify. Host-tested (snapshot B4-1; 6 behavioral B4-2) and validated by `blk_roundtrip_b4` (17/17 QEMU smoke). The harness gained a per-invocation virtio-blk disk. Per-system doc + SVG committed.
- **Step 2 (on-disk FS + buffer cache + `Mount`):** Done. A minimal xv6-style inode FS — superblock, free-block bitmap, inode table, single-level root directory of dirents — with the byte layout defined once in `frame-os-shared::fs` and used by *both* the kernel driver (`fs.rs`) and the host `mkfs` (`xtask build_fs_image`). A small write-through buffer cache sits between the FS and virtio-blk. The `Mount` HSM (`$Unmounted → $Mounting → $Mounted → $Unmounting`) gates reads on `is_mounted()`. The kernel mounts the mkfs'd disk, reads a baked `motd`, and runs a create → write → read → delete round-trip (`fs_file_roundtrip_b4`, B4-3). **B4-4 (persistence) is a genuine two-boot test:** the harness boots twice on one disk (fresh NVRAM each, shared disk) — boot 1 writes a marker file, boot 2 reads it back and confirms `persistence verified across reboot` (`fs_persists_across_reboot_b4`). Host-tested (snapshots B4-1; 5 `Mount` behavioral B4-2). 19/19 QEMU smoke. Per-system doc + SVG committed.
- **Step 3 (VFS + `OpenFile` + path lookup):** Done. `fs::namei` resolves an absolute path by walking directories from the root (`dir_lookup` per component), so nested paths like `/bin/info` work; `fs::read_at` does positioned reads. `kernel/src/vfs.rs` is the open-file table: `open_read(path)` resolves + creates an `OpenFile`, `read(fd)` advances a per-fd offset (gated on `is_reading()`), `close(fd)` frees the slot. The `OpenFile` HSM (`$Open → $Reading | $Writing → $Closed`) makes the access mode the state — a stray write on a read-fd is gated out. `mkfs` gained one-level nested directories (it now bakes `/bin/info`). Demo opens `/motd` + the nested `/bin/info` by path and shows a closed fd is inert (`vfs_path_lookup_b4`). Host-tested (snapshot B4-1; 6 `OpenFile` behavioral B4-2). 20/20 QEMU smoke. Per-system doc + SVG committed.
- **Step 4a (file-I/O syscalls + exec-from-disk + a scripted userspace shell):** Done. New syscalls `open` (5), `read` (6), `close` (7) — a 3rd argument (`rdx`) carries `read`'s length, read from the trap frame so the `SyscallDispatcher` Frame system stays unchanged — and a second `exec` form (8) that loads an ELF **from disk by path** (`fs::namei` + `fs::read_file` into a scratch buffer), refactored to share `exec_image()` with the B3 baked-program `exec`. A scripted ring-3 program (`user/src/shell.rs`, **hand-written raw-syscall Rust**, no Frame yet) `cat`s `/motd` via open/read/close, then `exec`s `/bin/hello` *by path* — the on-disk ELF replaces its image and runs to `exit(42)`. The harness bakes the real `hello` ELF onto the disk at `/bin/hello`. Validated by `userspace_shell_runs_program_from_disk_b4` (**21/21** QEMU smoke). **B4-5 met.** See [`docs/syscall_abi.md`](syscall_abi.md).
- **Step 4b (reuse `parser.frs` in ring 3 — the "one source, two targets" proof):** Done. The *same* `frame/parser.frs` the hosted shell compiles now also compiles for `x86_64-unknown-none`: the `user/` crate gained a `build.rs` that runs framec on it, a `frame_systems.rs` that re-exports the `alloc` prelude names the generated `no_std` code expects, and a 64 KiB `linked_list_allocator` heap (`mem.rs`) for the `String`/`Vec`/`Rc`/`BTreeMap` it allocates. A new ring-3 program (`user/src/frameshell.rs`) drives the `Parser` exactly as the host does (`consume(c)` per char, then `finalize()` + `tokens()`) to tokenize baked command lines and dispatch on the first token. It cats a **quoted** path (`cat "/motd"`) — which only resolves because the `$InQuotedString` state runs in ring 3 to strip the quotes — then execs `/bin/hello` by the parsed token. Validated by `userspace_frame_parser_reuse_b4` (**22/22** QEMU smoke). **B4-6 met for `Parser`.** The pure `Parser` had no native actions to port; the `Shell` `.frs` is a bigger lift (std-action rewrite + an input device) and remains pending.
- **Step 4c/B4-7 (diagrams + docs + CI gates):** Done. All 16 state-graph SVGs are in sync (`cargo xtask check-diagrams`); the docs were swept for drift; and the CI `kernel-build` + `qemu-test` jobs now install framec (their build scripts run it — a pre-existing gap caught while closing B4-7). CI gates: fmt, clippy (`-D warnings`), host build + test, kernel cross-build, 22-test QEMU smoke, and diagram drift.
- **Remaining:** only the `Shell` `.frs` userspace reuse (the second half of B4-6) — deferred until a real input device (keyboard / serial-RX) lands, since a userspace `Shell` machine needs both its `std`-only actions re-implemented for ring 3 *and* an input loop to drive. Everything else in B4 is complete.

### B5 — networking (the headline)

**Scope:** B4 plus a TCP/IP stack — the most impressive milestone and the deepest Frame stress test. A NIC driver, ARP, IPv4, ICMP (ping), UDP, and TCP, with **TCP modeled as the Frame state machine it canonically is**.

**Frame systems:**
- `ArpResolver`, `IpReassembly`, `UdpSocket`.
- **`TcpConnection`** — the full RFC-793 state machine: `$Closed → $SynSent/$SynReceived → $Established → $FinWait1/$FinWait2/$Closing/$TimeWait/$CloseWait/$LastAck → $Closed`, with retransmit, delayed-ACK, and simultaneous-open/close edge cases. One instance per connection.

**Native components:** virtio-net driver + DMA rings; checksum handling; socket buffers; the timer wheel feeding TCP's timers (TCP timers map to Frame enter/exit handlers + state variables, fired by the native wheel through the B4 `post`/`drain` boundary — Frame has no `after(ms)` primitive and doesn't need one; see [`plans/b5.md`](plans/b5.md)).

**Test transport:** start with **QEMU user-mode networking (slirp)** — no root, CI-friendly. **TAP** is the production path (full L2 + true ICMP) and is the deferred upgrade: add it once the slirp-based smoke tests pass, since it needs host/CI privilege + setup. (Decided 2026-05-21.)

**framec gates expected (the deepest in the whole roadmap):**
- **Timed transitions / `after(ms)`** — TCP is full of timers (retransmit, `TIME_WAIT` 2·MSL, delayed-ACK, keepalive). If Frame has no native timed-transition primitive, this is where it is needed most.
- **Orthogonal / parallel regions** — a connection's send and receive paths have largely independent state.
- **History states** and **guards** (sequence-number / window predicates).
- **Scale** — many concurrent connections, each an instance.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B5-1 | `TcpConnection` state graph matches the RFC-793 diagram | Snapshot `tcp_connection_state_graph_snapshot`; review against RFC-793 |
| B5-2 | Per-transition behavior incl the hard edges: simultaneous open/close, retransmit, `TIME_WAIT` expiry | Behavioral tests (host) per transition |
| B5-3 | The kernel answers `ping` (ICMP echo) | QEMU smoke `kernel_answers_ping_b5` (external client over QEMU user-net) |
| B5-4 | A full TCP handshake + a trivial request/response + clean close, against an external client | QEMU smoke `tcp_echo_or_http_b5` |
| B5-5 | ARP resolution and IP reassembly correct | Behavioral + smoke |
| B5-6 | Per-system docs incl a `TcpConnection`-vs-RFC-793 comparison write-up | Review |
| B5-7 | Diagrams + all CI gates + QEMU smoke | `cargo xtask check-diagrams`; full CI + `qemu-test` |

**Estimated effort:** Very large. This is the milestone the "stress-test Frame" thesis is pointed at — if Frame expresses a correct TCP FSM cleanly, that is the headline result.

**Status:** In progress. See [`plans/b5.md`](plans/b5.md).
- **Step 1 (virtio-net + RX/TX + post/drain for frames):** Done. A legacy virtio-net driver (`virtio_net.rs`): PCI discovery (reuses `pci.rs`), feature negotiation (MAC only), two virtqueues (RX = queue 0, TX = queue 1) in contiguous DMA frames, a pre-posted RX buffer pool, and the device IRQ wired at runtime from its PCI `interrupt_line` (it shares IRQ 11 with virtio-blk, which is idle by the time networking runs; `pic::eoi_for`/`unmask_irq` handle either PIC line). **The post/drain pattern is reused verbatim from B4:** the RX IRQ `on_irq` only *posts* (acks the device ISR + flags), the kernel *drains* the RX used ring (`poll_rx`) from normal context — no Frame dispatch in the ISR. Transport is QEMU user-net (slirp); TAP is the deferred production path.
- **Step 2a (ARP as a Frame system — `ArpResolver`):** Done. The first networking Frame system: `$Incomplete → $Resolved`, with a retransmit timer **armed in the enter handler** and `-> $Failed` at the retry cap — the project's first use of the "timer armed on state entry, fired by a native deadline through the receive loop" pattern (the B5 plan's answer to TCP's timers, small-scale). Frame owns the lifecycle + retry budget (`attempts`/`max_attempts`); native (`net.rs`) owns the Ethernet/ARP encode+decode, the resolved MAC bytes, and the deadline. Demo (`net::run_demo`): bring up the NIC, then resolve the slirp gateway (10.0.2.2) through `ArpResolver` and print its MAC. Host-tested (snapshot `arp_resolver_state_graph`; 6 behavioral) and validated by `arp_resolves_gateway_b5` (**23/23** QEMU smoke). Per-system doc + SVG committed.
- **Step 2b (IPv4 + ICMP echo):** Done. Native IPv4 + ICMP encode/parse + the RFC-1071 internet checksum (`net.rs`). After resolving the gateway MAC, the kernel sends an ICMP echo request to 10.0.2.2 (Ethernet → IPv4 → ICMP, both checksums) and matches the reply. Validated by `kernel_pings_gateway_b5` (**24/24** QEMU smoke). This is the ICMP *client* path; answering inbound pings (the responder, B5-3) lands with **TAP**, where inbound ICMP can actually reach the guest (slirp NAT won't route it). No new Frame system — pure native protocol work layered on the resolved gateway.
- **Step 3a (RX pipeline as a Frame system — `RxPipeline`):** Done. The marquee data-pipeline: a received frame's parsed `RxDescriptor` (ethertype + IP protocol) flows down a classify→dispatch graph (`$Idle → $Classifying → ($Arp | $Ipv4 → $Icmp | $Udp)`) as an **enter parameter**, while the frame bytes stay in a native buffer (`net::RX_FRAME`) — the "thread the descriptor, keep the payload native" recipe, on real packets, and the first system to thread a parsed *struct* (needs the new typed-context framec). The gateway ARP resolution and the ICMP ping now drive their received frames through `RxPipeline` (`net::pump`). Host-tested (snapshot + 6 behavioral) and validated by `arp_resolves_gateway_b5` + `kernel_pings_gateway_b5` (24/24 QEMU). Per-system doc + SVG committed.
- **Remaining:** Step 3b — UDP + `UdpSocket` (the `$Udp` leaf delivering to a bound socket; a slirp DHCP round-trip for a deterministic inbound UDP datagram), then the TCP headline — `TcpConnection` (Step 4). The ICMP responder (B5-3) rides along with the TAP transport upgrade.

### B6 — USB

**Scope:** B5 plus a USB stack: an xHCI host-controller driver and device enumeration, demonstrating Frame on a deep hardware protocol.

**Frame systems:**
- `UsbEnumeration` — `$Powered → $Reset → $AddressAssigned → $Configured`.
- `UsbTransfer` — control/bulk/interrupt transfer lifecycles.
- `HubPort` — per-port connect/reset/enable state.

**Native components:** xHCI controller (command/event/transfer rings, DCBAA, scratchpad buffers); USB descriptor parsing.

**framec gates expected:** deep protocol HSMs; orthogonal regions (multiple ports concurrently); timed transitions (reset/settle timing).

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B6-1 | `UsbEnumeration`, `UsbTransfer`, `HubPort` state graphs match committed designs | Snapshots (`kernel-tests`) |
| B6-2 | Enumeration + transfer lifecycles correct | Behavioral tests (host) |
| B6-3 | A QEMU virtual USB device (e.g. keyboard or mass-storage) enumerates to `$Configured` and completes a transfer | QEMU smoke `usb_device_enumerates_b6` |
| B6-4 | Per-system docs; diagrams; all CI gates + QEMU smoke | Review; `cargo xtask check-diagrams`; full CI + `qemu-test` |

**Estimated effort:** Very large.

### B7 — SMP

**Scope:** B6 plus symmetric multiprocessing. Bring up the application processors, run the scheduler across all cores, and make the kernel safe under true concurrency. The hardest milestone, and the one that most tests the deferred-event queue's concurrency story.

**Frame systems:** minimal *new* Frame logic — locking and per-CPU data are native. The point is that the **existing** systems (`Scheduler`, `Process`, `TcpConnection`, …) remain correct when their Ports receive `post`s from other cores.

**Native components:** AP startup (INIT/SIPI); per-CPU data (GS-base); IPIs; TLB shootdown; spinlocks/sleep-locks + documented lock ordering; the cross-core `post` path on the deferred-event queue.

**framec gate expected:** **`Send` + `Sync` codegen** — a Frame system whose Port receives cross-core posts needs its event type and queue thread-safe; framec may need an `Arc`-based / `Send`-able codegen mode. This is the concurrency gate flagged in early analysis, now hit for real, in the meanest possible setting.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B7-1 | Multiple cores each run threads/processes | QEMU smoke `smp_cores_run_concurrently_b7` (`-smp N`) |
| B7-2 | A Frame system is safely driven from another core via cross-core `post` (no data race) | QEMU smoke + (where feasible) a sanitizer build |
| B7-3 | Lock ordering documented; no deadlock under a stress workload | Review + a stress smoke test |
| B7-4 | The deferred-event queue's `Send`/`Sync` story validated; framec codegen mode (if added) documented | Review; framec gate write-up |
| B7-5 | All CI gates + QEMU smoke (incl `-smp`) on Linux | Full CI matrix + `qemu-test` |

**Estimated effort:** Very large; the final boss. SMP correctness (locking, TLB shootdown, memory ordering) is where real kernels spend their hardest debugging.

## Dependency graph between milestones

The H-track (H0 → H1 → H2 → H3) is **complete**. The B-track is strictly
sequential and each milestone builds on the last:

```
B0 ──► B1 ──► B2 ──► B3 ──► B4 ──► B5 ──► B6 ──► B7
done  preempt  VM   user/ block/  net   USB   SMP
             space  proc  FS    (TCP)
```

- **B1 → B2:** preemption + the deferred-event queue exist before virtual memory; VM doesn't need preemption but preemption surfaces the queue, which later milestones rely on.
- **B2 → B3:** user-mode processes need per-process address spaces (B2's paging) and fault handling.
- **B3 → B4:** the userspace shell and "load a program from disk" need both user mode (B3) and a filesystem (B4); the FS driver also benefits from preemption (B1) to overlap I/O.
- **B4 → B5 → B6:** networking and USB are device stacks layered on the interrupt + DMA infrastructure that's matured by B4.
- **B7 (SMP) last:** it re-validates every prior Frame system under true concurrency, so it comes after they exist and are correct single-core.

The H-track's `Shell`/`Parser` Frame systems are **reused** at B4 as a
userspace program — the same `.frs`, different (userspace) action
implementations. That cross-track sharing is a deliberate demonstration,
not a dependency that blocks B-track progress.

**Estimated effort.** This is now a multi-year, real-OS-class project, not
a months-long demonstration. Each of B1–B7 is a "very large" milestone in
its own right (preemption, paging, the user/kernel boundary, a filesystem,
a TCP/IP stack, USB, SMP are each the kind of thing that anchors a
semester course or a small team for months). There is **no time pressure**
on this roadmap — correctness, documentation, and Frame stress-test value
are the goals, not speed. Milestones are taken one at a time, each landing
green (all gates + QEMU smoke) before the next begins, exactly as B0 did.

## Testing across milestones

Test coverage is a continuous concern, not a milestone of its own. Every milestone that introduces a Frame system or a major native module is expected to land with:

- A state-graph snapshot test for any new Frame system (Level 2 in [`testing.md`](testing.md))
- Behavioral tests covering the committed state-event pairs (Level 3)
- Integration tests where systems compose with each other (Level 4)
- QEMU smoke tests for any new bare-metal behavior (Level 7)
- A per-system doc following [`systems/_template.md`](systems/_template.md), with its Testing section filled in

The test infrastructure is bootstrapped at H0 (workspace `cargo test`, `insta` snapshots, `assert_cmd` E2E) and extended at B0 (QEMU smoke test runner). After that, each milestone *uses* the infrastructure rather than building it.

A milestone whose Frame systems lack the expected test coverage is not "done" even if the code works. The vision doc commits to documented systems with documented test coverage; the roadmap honors that commitment by treating tests as a milestone deliverable rather than a follow-up.

## Now in scope (formerly excluded)

The re-baseline pulled several items that were previously out of scope into
committed milestones:

- **User mode + processes + `fork`/`exec`** — core at B3 (was a B4 stretch / `fork` was excluded).
- **Virtual memory / paging** — core at B2.
- **On-disk filesystem** — core at B4 (was excluded; "bundled file table only").
- **Networking / TCP/IP** — core at B5 (was excluded). The headline Frame stress test.
- **USB** — core at B6 (was excluded).
- **Multi-core / SMP** — core at B7 (was excluded).

## Out of scope

Still *not* on this roadmap unless scope is expanded later:

- **GUI / framebuffer graphics.** Serial (and possibly a later text console) only; no windowing, no compositor.
- **Audio, video, GPU acceleration.**
- **Multiple threads within one process.** The process is the unit of concurrency (one thread per process). Kernel-internal concurrency is the scheduler's threads and, at B7, multiple cores.
- **Dynamic linking / shared libraries.** Static user binaries only.
- **Full POSIX signal set.** A basic subset (`SIGKILL`, `SIGSEGV`, `SIGCHLD`) at B3; not the complete signal/handler/mask/`sigaction` machinery.
- **Virtualization / container support.**
- **Power management / ACPI sleep states** beyond what firmware does at boot. (A device/power state machine could be a *future* Frame showcase, but it is not committed.)
- **Architectures beyond x86_64** for now. AArch64 / Raspberry Pi is a plausible later track, not a committed milestone.

These are excluded to keep an already-large project bounded; some may make sense as follow-on work once B7 is reached.

## How the roadmap will be maintained

This file is updated as milestones are completed and as scope decisions change. Each milestone gets a "status" annotation as it progresses: `planned`, `in-progress`, `done`, or `deferred`. When a milestone is `done`, the criteria above should be verifiable by anyone who builds and runs Frame OS.

Decisions to expand or contract scope are documented here, with reasoning — as the 2026-05-20 re-baseline note at the top of Track B does. If a committed milestone (e.g. B6/B7) is dropped, this file should explain why. If a new milestone (say, "B8 — AArch64 / Raspberry Pi port") is added, this file should explain its goals and dependencies.

The roadmap is a project artifact, not a marketing document. It should be accurate enough that someone reading it knows what the project is, what it isn't, and what's actually working today.

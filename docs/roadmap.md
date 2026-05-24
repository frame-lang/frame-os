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
| B5-1 | `TcpConnection` state graph matches the RFC-793 diagram | **Done** — `tcp_connection_state_graph_snapshot` + the RFC-793↔Frame table in `systems/tcp_connection.md` |
| B5-2 | Per-transition behavior incl the hard edges: simultaneous open/close, retransmit, `TIME_WAIT` expiry | **Done** — 16 behavioral tests in `tcp_connection_behavior.rs` (both opens, both closes, simultaneous open/close, RST funnel, SYN/SYN-ACK retransmit, TIME_WAIT timeout) |
| B5-3 | The kernel answers `ping` (ICMP echo) | **Done** — the kernel *sends* ping + matches the reply (`kernel_pings_gateway_b5`), and now *answers* a real inbound `ping 10.0.2.15` over a host TAP link: `cargo xtask qemu-tap` brings up `tap0`, pings the guest, and asserts the reply + `[icmp] answered ping` (Step 5, TAP transport) |
| B5-4 | A full TCP handshake + a trivial request/response + clean close, against an external client | **Done** — `tcp_echo_b5` (handshake → echo → clean close vs. the host's real TCP stack) + `tcp_active_open_b5` (active open) |
| B5-5 | ARP resolution and IP reassembly correct | **Done** — ARP resolution (`ArpResolver`, `arp_resolves_gateway_b5`) + inbound ARP responder over TAP; **IP reassembly** via the `IpReassembly` Frame system + native outbound fragmentation, validated by a real `ping -s 4000` round-trip over TAP (`qemu-tap`: 3 fragments reassembled → whole echo request answered → >MTU reply re-fragmented outbound). (Single-flight, coverage-bitmap-correct; not full RFC-815 multi-datagram hole management — deferred, no workload needs it.) |
| B5-6 | Per-system docs incl a `TcpConnection`-vs-RFC-793 comparison write-up | **Done** — `systems/tcp_connection.md` (RFC-793↔Frame mapping table) + per-system docs for all 5 B5 systems |
| B5-7 | Diagrams + all CI gates + QEMU smoke | **Local: green** (`check-diagrams` clean; 28/28 `qemu-test`; host suite passes). **crates.io CI: blocked** — the kernel now uses typed *struct* enter params (`ElfLoader`, `RxPipeline`), which need the new typed-context framec; CI's `cargo install framec` (4.2.0, String-form enter args) can't build it until that framec is published |

**Estimated effort:** Very large. This is the milestone the "stress-test Frame" thesis is pointed at — if Frame expresses a correct TCP FSM cleanly, that is the headline result.

**Status:** In progress. See [`plans/b5.md`](plans/b5.md).
- **Step 1 (virtio-net + RX/TX + post/drain for frames):** Done. A legacy virtio-net driver (`virtio_net.rs`): PCI discovery (reuses `pci.rs`), feature negotiation (MAC only), two virtqueues (RX = queue 0, TX = queue 1) in contiguous DMA frames, a pre-posted RX buffer pool, and the device IRQ wired at runtime from its PCI `interrupt_line` (it shares IRQ 11 with virtio-blk, which is idle by the time networking runs; `pic::eoi_for`/`unmask_irq` handle either PIC line). **The post/drain pattern is reused verbatim from B4:** the RX IRQ `on_irq` only *posts* (acks the device ISR + flags), the kernel *drains* the RX used ring (`poll_rx`) from normal context — no Frame dispatch in the ISR. Transport is QEMU user-net (slirp); TAP is the deferred production path.
- **Step 2a (ARP as a Frame system — `ArpResolver`):** Done. The first networking Frame system: `$Incomplete → $Resolved`, with a retransmit timer **armed in the enter handler** and `-> $Failed` at the retry cap — the project's first use of the "timer armed on state entry, fired by a native deadline through the receive loop" pattern (the B5 plan's answer to TCP's timers, small-scale). Frame owns the lifecycle + retry budget (`attempts`/`max_attempts`); native (`net.rs`) owns the Ethernet/ARP encode+decode, the resolved MAC bytes, and the deadline. Demo (`net::run_demo`): bring up the NIC, then resolve the slirp gateway (10.0.2.2) through `ArpResolver` and print its MAC. Host-tested (snapshot `arp_resolver_state_graph`; 6 behavioral) and validated by `arp_resolves_gateway_b5` (**23/23** QEMU smoke). Per-system doc + SVG committed.
- **Step 2b (IPv4 + ICMP echo):** Done. Native IPv4 + ICMP encode/parse + the RFC-1071 internet checksum (`net.rs`). After resolving the gateway MAC, the kernel sends an ICMP echo request to 10.0.2.2 (Ethernet → IPv4 → ICMP, both checksums) and matches the reply. Validated by `kernel_pings_gateway_b5` (**24/24** QEMU smoke). This is the ICMP *client* path; answering inbound pings (the responder, B5-3) lands with **TAP**, where inbound ICMP can actually reach the guest (slirp NAT won't route it). No new Frame system — pure native protocol work layered on the resolved gateway.
- **Step 3a (RX pipeline as a Frame system — `RxPipeline`):** Done. The marquee data-pipeline: a received frame's parsed `RxDescriptor` (ethertype + IP protocol) flows down a classify→dispatch graph (`$Idle → $Classifying → ($Arp | $Ipv4 → $Icmp | $Udp)`) as an **enter parameter**, while the frame bytes stay in a native buffer (`net::RX_FRAME`) — the "thread the descriptor, keep the payload native" recipe, on real packets, and the first system to thread a parsed *struct* (needs the new typed-context framec). The gateway ARP resolution and the ICMP ping now drive their received frames through `RxPipeline` (`net::pump`). Host-tested (snapshot + 6 behavioral) and validated by `arp_resolves_gateway_b5` + `kernel_pings_gateway_b5` (24/24 QEMU). Per-system doc + SVG committed.
- **Step 3b (UDP + `UdpSocket`):** Done. Native UDP encode/parse + a DHCP DISCOVER→OFFER round-trip against slirp's (always-present) DHCP server. The kernel binds a `UdpSocket` (`$Unbound → $Bound`, `recv()` gated to `$Bound`) on :68 and DISCOVERs; the OFFER is classified by `RxPipeline` (IPv4 → UDP) and delivered to the bound socket (`on_udp` → `recv()`), latching the offered IP (10.0.2.15). Host-tested (snapshot + 5 behavioral) and validated by `dhcp_offer_b5` (**25/25** QEMU). Per-system doc + SVG committed.
- **Step 4a (`TcpConnection` FSM — the RFC-793 state machine):** Done. The deepest Frame system: all 11 RFC-793 states (`$Closed`/`$Listen`/`$SynSent`/`$SynReceived`/`$Established`/`$FinWait1`/`$FinWait2`/`$Closing`/`$TimeWait`/`$CloseWait`/`$LastAck`) under an `$Open` parent that funnels `rst()`/`abort()` to `-> $Closed` via `=> $^`. Segments are the per-state event (a `TcpSegment` threaded typed); guards are native `if` on flags; retransmit + 2·MSL timers are armed in enter handlers. Native `crate::tcp` owns segment parse/encode + checksum (pseudo-header) + the connection's seq state + the senders; the `RxPipeline` gained a `$Tcp` leaf. **Host-validated against RFC-793: 15 behavioral tests** (both opens, both closes, simultaneous open/close, the RST funnel, retransmit) **+ the state-graph snapshot (B5-1/B5-2).** Wired live in `$Listen` on :7 (`tcp_listen_b5`, **26/26** QEMU) — the live handshake/echo/close land at 4b–4d. Per-system doc (incl. an RFC-793 ↔ Frame table) + SVG committed.
- **Step 4b (live passive handshake):** Done. The kernel passive-opens a `TcpConnection` on :7 and serves (`net::tcp_serve` pumps inbound segments through the `RxPipeline` `$Tcp` leaf into the FSM); the smoke harness connects through slirp `hostfwd=tcp::15580-:7` (a new harness TCP probe, connect-with-retry), driving the 3-way handshake so the FSM reaches `$Established` against the **host's real TCP stack** — `send_syn_ack`/`send_ack` + seq arithmetic + checksums validated live. Validated by `tcp_handshake_b5` (**26/26** QEMU; `tcp_listen_b5` renamed). The harness gained a `FRAMEOS_SMOKE_FILTER` env-var to run one test.
- **Step 4c (data echo):** Done. `$Established` echoes the client's bytes back (the demo's echo "app"; the data segment piggybacks the ACK). The harness sends a request and **reads it back** (`tcp_echo_b5`, gated in the harness), validating the outbound data path's seq + pseudo-header checksum against the host's TCP. Added server connection-recycling (drop a dead/idle slirp connection → re-listen → accept the live one) and a serial-gated probe (the harness waits for `[tcp] listening` before connecting, making exactly one connection). **27/27** QEMU.
- **Step 4d (clean close + TIME_WAIT timer wheel) — B5-4 met:** Done. After the echo the kernel actively closes; the native timer wheel (`tcp::drain_timers`, wired into the serve loop — the post/drain pattern) fires `$TimeWait`'s 2·MSL timeout → `$Closed`. `tcp_echo_b5` now covers the **full handshake + request/response + clean close** against the host's TCP stack (asserts `[tcp] established`/`echoed`/`closed`). **27/27** QEMU. So **B5-4 is met** — a complete TCP exchange driven by the RFC-793 Frame FSM, with timers expressed as enter-handler-armed + native-wheel-fired (the "Frame has no `after(ms)`" answer, proven on TCP).
- **Step 4e (active open + retransmit):** Done. The kernel connects *out* to 10.0.2.100:9 — slirp `guestfwd` forwards it to a host listener (the harness binds it before QEMU starts, since QEMU opens the guestfwd target at startup); reached via the already-resolved gateway MAC (slirp uses one MAC for all its virtual addresses). `$SynSent` → `$Established`, validated by `tcp_active_open_b5` (**28/28** QEMU). Added a SYN-ACK retransmit behavioral test (16 total). guestfwd is only attached for the active test (it must connect at startup); the kernel's active-open on other boots RSTs fast (~1.5s cap). So **both passive and active opens are now live against a real peer.**
- **Step 4f (wrap-up):** Done. B5-1/B5-2 (TcpConnection snapshot + 16 behavioral) and B5-6 (RFC-793↔Frame write-up + per-system docs for all 5 B5 systems) confirmed; diagrams in sync; 28/28 QEMU. B5-7's *local* gates are green; the *crates.io* CI is blocked on publishing the typed-context framec (the kernel now uses struct enter params). The headline result stands: **a real TCP connection — passive (handshake → echo → clean close, B5-4) and active open — driven entirely by the RFC-793 `TcpConnection` Frame FSM against the host's real TCP stack, with timers via the enter-handler + native-wheel idiom.**
- **Step 5 (TAP transport — inbound responders, B5-3) — done.** The slirp client demos can't be pinged *from* the host (slirp NAT won't route inbound ICMP to the guest). A real host TAP link can. Added the kernel's inbound responders to the `RxPipeline` leaves — `$Arp` answers who-has-10.0.2.15 (`send_arp_reply`, logs `[arp] answered who-has 10.0.2.15`) and `$Icmp` answers echo-request-to-us (`send_icmp_echo_reply`, logs `[icmp] answered ping`) — so they fire on any inbound frame, no new state. Over slirp they never trigger (no peer ARPs/pings us); over TAP they do. `run_demo` reaches an inbound-serve window because, with no slirp gateway, ARP-gateway resolution fails and falls into `serve_inbound()` (a bounded ~10s RX pump). New xtask subcommand `cargo xtask qemu-tap` (Linux + NET_ADMIN + `/dev/net/tun`; run as `TAP=1 docker/run.sh "cargo xtask qemu-tap"`): brings up `tap0` (host `10.0.2.1/24`), boots QEMU with `-netdev tap`, `ping`s `10.0.2.15` in a retry loop, and asserts the reply **and** `[icmp] answered ping` in serial — proving the *guest's* responder replied. **Validated in the dev container.** **Also fixed a latent xtask bug:** all artifact paths (kernel ELF, ESP image, Limine, QEMU scratch) hardcoded `<workspace>/target`, but cargo writes to `CARGO_TARGET_DIR` (`/target` in the container) — so `build_kernel` read back a *stale* cross-built ELF. Routed every path through a new `target_dir()` helper that honors `CARGO_TARGET_DIR`; the smoke suite now tests the current kernel (re-validated 28/28).

- **Step 6 (IP reassembly — B5-5) — done.** The last B5 functional gap. A `ping -s 4000` over TAP fragments at the 1500-byte MTU, so the kernel must reassemble the inbound fragments before it can answer. New `IpReassembly` Frame system (`frame/ip_reassembly.frs`): `$Idle → $Reassembling → ($Complete | $Expired)`, threading each fragment's parsed `Fragment` (offset/len/more/ident) into `$Reassembling` as an **enter parameter**, with the *self-transition* `-> (frag) $Reassembling` re-running the "store + am-I-complete?" entry action once per fragment (the second descriptor-threading pipeline after `RxPipeline`, now on a *collect-until-whole* loop). Native `kernel/src/ip_reasm.rs` owns the reassembly buffer + a per-byte **coverage bitmap** (so completion is a real "all bytes present" check, correct for out-of-order/overlapping fragments — single-flight, not full RFC-815) + the datagram reconstruction; `net::tx_ipv4_fragmented` mirrors it outbound (the >MTU echo reply is split into MTU-sized fragments). **Found + fixed a real driver bug:** `virtio_net::tx_frame` was fire-and-forget over a single TX buffer, so the 3 back-to-back reply fragments clobbered each other — now it waits for TX completion (bounded spin on the used ring). **Found a framec codegen bug:** a state named `$Empty` collides with framec's synthesized reserved `Empty` context-enum variant (renamed the state to `$Idle`; recorded in `frame_assessment.md`). Validated by `cargo xtask qemu-tap`'s `ping -s 4000` (`[ip] reassembled 4008 bytes from 3 fragments` + a successful round-trip), 7 host behavioral tests + a snapshot, and 28/28 smoke (no regression from the TX-wait change). **B5-5 met.**

**B5 status: complete (bar the framec-release gate).** The core (NIC + ARP + IPv4/ICMP + UDP + the full TCP exchange) is done and validated; the kernel answers a real inbound `ping` (B5-3, Step 5) and reassembles a fragmented one (B5-5, Step 6) over TAP. **Deferred to the framec release:** crates.io CI (B5-7). The "stress-test Frame" findings are recorded in [`frame_assessment.md`](frame_assessment.md).

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
| B6-1 | `UsbEnumeration`, `UsbTransfer`, `HubPort` state graphs match committed designs | **Done** — `hub_port`/`usb_enumeration`/`usb_transfer` state-graph snapshots (`kernel-tests/tests/state_graphs.rs`) |
| B6-2 | Enumeration + transfer lifecycles correct | **Done** — behavioral tests: `hub_port_behavior` (8), `usb_enumeration_behavior` (9), `usb_transfer_behavior` (4) |
| B6-3 | A QEMU virtual USB device (e.g. keyboard or mass-storage) enumerates to `$Configured` and completes a transfer | **Done** — the qemu-xhci `usb-kbd` enumerates to `$Configured` (`usb_enumerates_b6`) **and** completes a real interrupt-IN transfer of a HID key report (`usb_transfer_b6`, keypress injected via the QEMU monitor) |
| B6-4 | Per-system docs; diagrams; all CI gates + QEMU smoke | **Done (local)** — per-system docs + SVGs for all three USB systems; `cargo xtask check-diagrams` clean; **32/32** `qemu-test`; host suite green. (crates.io CI shares B5-7's framec-publish gate.) |

**Estimated effort:** Very large.

#### Steps

- **Step 1 (native xHCI controller bring-up) — done.** The native foundation, no Frame system yet. `kernel/src/xhci.rs`: discover the controller by PCI class (`0C/03/30`, via the new `pci::find_by_class` + `bar_mem` 64-bit-BAR helpers), **map its MMIO window** (the BAR sits in QEMU's high PCIe hole, which Limine's HHDM does *not* map — so `paging::map` the register window uncached, the first explicit MMIO mapping in the kernel), reset the controller (`USBCMD.HCRST`), stand up the structures the spec requires before Run — DCBAA (+ scratchpad buffers if requested), command ring (with a Link TRB), event ring + a one-entry ERST wired into interrupter 0 — set `USBCMD.R/S`, and report any device on a port (`PORTSC.CCS`). QEMU now boots with `-device qemu-xhci -device usb-kbd`; the kernel logs `xHCI running` + `device connected on port 5`. Validated by `usb_controller_b6` (**29/29** QEMU smoke; no regression). The USB *lifecycle* (port reset, enumeration, transfers) is driven by Frame systems in Steps 2–4.
- **Step 2 (`HubPort` Frame system — port connect/reset/enable) — done.** The first USB Frame system: `$Disconnected → $Connected → $Resetting → $Enabled`, with `disconnect()` funneled to `$Disconnected` from any attached state through an `$Attached` parent (`=> $^` — the `Process.$Alive` / `TcpConnection.$Open` pattern, applied to hot-plug). The port reset is a **timed transition**: `$Resetting`'s enter handler asserts `PORTSC.PR` + arms a settle deadline (`xhci::begin_port_reset`, the B5 enter-armed/native-fired idiom), and `xhci::run_port_lifecycle()` dispatches `reset_complete()` (controller reports the port enabled) or `timeout()`. The 1-based port threads through the FSM domain. Native (`xhci.rs`) owns the PORTSC pokes (set Port Reset with RW1C care, ack the change bits) + the deadline. Drives the qemu-xhci usb-kbd's port (port 5) from connect to enabled — serial: `[usb] resetting port 5` → `[usb] port 5 enabled`. Host-tested (snapshot + 8 behavioral incl. the disconnect funnel from each attached state) and validated by `usb_port_reset_b6` (**30/30** QEMU). Per-system doc + SVG committed. The enabled port is ready for enumeration (Step 3).
- **Step 3 (`UsbEnumeration` Frame system — full enumeration to `$Configured`) — done.** The device-enumeration lifecycle: `$Powered → $SlotEnabled → $AddressAssigned → $DeviceDescribed → $Configured`, with `fail()` funneled to `$Failed` from any active stage through an `$Enumerating` parent (`=> $^`). Each state's enter handler issues the **next xHCI command or control transfer, non-blocking**, and `xhci::run_enumeration()` dequeues the completion events off the event ring and dispatches the milestone event — the same "enter kicks the async step, completion event advances the FSM" shape as TCP, now over xHCI rings. The assigned **slot id threads through the FSM domain**.
  - **3a/3b (command ring):** Enable Slot (`$Powered`) + Address Device (`$SlotEnabled`), driven by Command Completion Events. Drove the meatiest native work: the **command ring** (TRB enqueue + cycle bit + Link-TRB wrap), the **event ring** (cycle-state dequeue + ERDP), **doorbells**, and the **Input Context** (Input Control + Slot + EP0 contexts, `ctx_64`-aware) + the output device context in the DCBAA.
  - **3c (EP0 control transfers):** GET_DESCRIPTOR (`$AddressAssigned`) + SET_CONFIGURATION (`$DeviceDescribed`), driven by Transfer Events — Setup/Data/Status TRBs on a per-device EP0 transfer ring. Reads + logs the device descriptor (idVendor/idProduct), then selects configuration 1.
  - Enumerates the real qemu-xhci usb-kbd end to end — serial: `[usb] slot 1 enabled` → `[usb] device addressed (slot 1)` → `[usb] device descriptor: idVendor 0627 idProduct 0001` → `[usb] device configured (slot 1)`. Host-tested (snapshot + 9 behavioral incl. the fail funnel from each stage) and validated by `usb_enumerates_b6` (**31/31** QEMU). Per-system doc + SVG committed.
- **Step 4 (`UsbTransfer` Frame system — a real transfer; B6-3 met) — done.** The generic transfer lifecycle: `$Idle → $InFlight → ($Complete | $Failed)`. `$InFlight`'s enter handler *queues* the transfer (non-blocking — a Normal TRB on the endpoint ring + a doorbell), and `xhci::run_transfer()` dispatches `complete()`/`fail()` on the controller's Transfer Event; `$Complete` reads the result. Wired to the keyboard's **interrupt-IN** endpoint: native prep issues a **Configure Endpoint** command (add EP1-IN + its transfer ring), then the FSM queues an interrupt-IN read. The transfer completes when a key report arrives — the automated test injects a keypress via the **QEMU monitor** (`sendkey a`, serial-gated on `[usb] waiting for key report`, the same shape as the TCP/TAP probes), and the kernel reads back the HID boot report — serial: `[usb] interrupt endpoint configured (EP1 IN)` → `[usb] HID report: modifiers 0 keycode 0x04` (HID usage for 'a') → `[usb] key transfer complete`. Host-tested (snapshot + 4 behavioral) and validated by `usb_transfer_b6` (**32/32** QEMU). Per-system doc + SVG committed. **B6-3 met** — the device enumerates to `$Configured` *and* completes a transfer.

**B6 status: complete (bar the framec-release gate).** A full USB path — xHCI controller bring-up → port reset (`HubPort`) → enumeration to `$Configured` (`UsbEnumeration`) → a real interrupt-IN HID transfer (`UsbTransfer`) — on the real qemu-xhci controller + usb-kbd, with three Frame systems driving the lifecycle over native command/event/transfer rings + contexts. All four exit criteria met locally; crates.io CI shares B5-7's framec-publish gate. The first deep hardware protocol done the Frame way.

### B7 — SMP

**Scope:** B6 plus symmetric multiprocessing. Bring up the application processors, run the scheduler across all cores, and make the kernel safe under true concurrency. The hardest milestone, and the one that most tests the deferred-event queue's concurrency story.

**Frame systems:** minimal *new* Frame logic — locking and per-CPU data are native. The point is that the **existing** systems (`Scheduler`, `Process`, `TcpConnection`, …) remain correct when their Ports receive `post`s from other cores.

**Native components:** AP startup (INIT/SIPI); per-CPU data (GS-base); IPIs; TLB shootdown; spinlocks/sleep-locks + documented lock ordering; the cross-core `post` path on the deferred-event queue.

**framec gate expected:** **`Send` + `Sync` codegen** — a Frame system whose Port receives cross-core posts needs its event type and queue thread-safe; framec may need an `Arc`-based / `Send`-able codegen mode. This is the concurrency gate flagged in early analysis, now hit for real, in the meanest possible setting.

#### Exit criteria

| # | Exit criterion | Validating test(s) |
|---|---|---|
| B7-1 | Multiple cores each run threads/processes | **Done** — `smp_preempt_b7`: each AP runs its own LAPIC-timer-preempted busy loop (`-smp 4`), reporting timer ticks + work units. Each core runs a real time-sliced thread. (Per-CPU run *queues* / multiple threads per core is a further refinement.) |
| B7-2 | A Frame system is safely driven from another core via cross-core `post` (no data race) | **Done** — `smp_cross_core_post_b7`: `EventCounter` driven from all 4 cores via a `SpinLock` MPSC queue drained on the owner core; exact count (800) proves once-only dispatch, no race. Instance pinned to one core (a local), only `Send` data crosses — see Step 3 |
| B7-3 | Lock ordering documented; no deadlock under a stress workload | **Mostly done** — lock ordering documented in `spin.rs` (all leaves so far); the 4-core × 50 000 counter hammer (`smp_concurrent_b7`) + the TLB-shootdown ack barrier (`smp_tlb_shootdown_b7`) are stress workloads that complete without deadlock. Deeper stress revisited as nested locks appear |
| B7-4 | The deferred-event queue's `Send`/`Sync` story validated; framec codegen mode (if added) documented | **Done** — validated that **no** framec `Send`/`Sync` codegen mode is needed: the post/drain architecture keeps the Frame instance single-owner and only `Send` data crosses cores (`smp_cross_core_post_b7`). Write-up in `frame_assessment.md` |
| B7-5 | All CI gates + QEMU smoke (incl `-smp`) on Linux | **Done (local)** — the full **37/37** `qemu-test` suite runs under `-smp 4` in the Linux dev container; host suite + clippy + diagrams green. (crates.io CI shares B5-7's framec-publish gate.) |

**Estimated effort:** Very large; the final boss. SMP correctness (locking, TLB shootdown, memory ordering) is where real kernels spend their hardest debugging.

**Concurrency model (decided 2026-05-22):** fine-grained per-CPU from the start — per-CPU run state + per-structure locks with a documented lock ordering, not a Big-Kernel-Lock staging.

#### Steps

- **Step 1 (AP startup + per-CPU data) — done.** Native foundation, no Frame system. QEMU now boots with `-smp 4`. The kernel declares a Limine **MP request** and, after the boot HSM, launches each application processor at `ap_entry` (writing the CPU's `goto_address`, stashing its index in `extra`); each AP sets up its **per-CPU block + GS base** (`kernel/src/percpu.rs` — `IA32_GS_BASE` MSR → a `PerCpu` whose first field `cpu_index` is read via `gs:[0]`, the standard x86_64 per-CPU mechanism), reports online (an atomic), and parks (`cli; hlt` — the timer/PIC route to the BSP, so APs idle until they run the scheduler in Step 2). The BSP sets its own per-CPU block (index 0) and waits for the APs. Serial: `[smp] cores online: 4 of 4 (BSP lapic 0, this cpu 0)` — the `this cpu 0` proving the GS-base read works. Validated by `smp_cores_online_b7` (B0–B6 unaffected under `-smp 4`).
- **Step 2 (the locking foundation — IRQ-safe `SpinLock` + a cross-core stress) — done.** Native, no Frame system. `kernel/src/spin.rs`: a test-and-set `SpinLock<T>` that is **IRQ-safe** — acquiring it saves+clears the interrupt flag on the core and the guard restores it on drop, so a lock taken from both mainline and an ISR on the same core can't self-deadlock (the timer ISR can't fire mid-critical-section), while cross-core contention is resolved by the spin. Documented **lock ordering** (every lock is currently a leaf). Proven by a deliberate stress: all four cores hammer a shared `SpinLock<u64>` 50 000× each *concurrently* (`hammer_counter`), and the BSP checks the total is **exactly** 4 × 50 000 = 200 000 — an exact total means every increment was serialized with no lost update, i.e. the lock is correct under true parallelism. Serial: `[smp] shared counter: 200000 (expected 200000)` → `cross-core lock: ok (no lost updates)`. Validated by `smp_concurrent_b7` (**34/34** QEMU).
- **Step 3 (the cross-core `post` — the Frame reckoning; B7-2 met) — done.** The headline SMP question: *can a Frame system be driven from a different core than the one that owns its instance, given framec's generated code is neither `Send` nor `Sync`?* **Answer: yes, with no framec change** — the post/drain architecture already isolates the instance to one core. A tiny `EventCounter` Frame system (`$Counting → $Closed`) is the vehicle: its instance is a **local** owned by the BSP (`crate::crosscore::run_drain_demo`), never moved/touched by another core; the other cores only enqueue plain `Copy`/`Send` event data (`PostedEvent::Tick`) into a `SpinLock`-protected **MPSC ring**; the BSP drains the ring and dispatches `tick` to its local instance — the cross-core generalization of B4/B5's "ISRs only post, the kernel drains." All 4 cores post 200 ticks each → the BSP's counter reaches **exactly 800** (every cross-core post dispatched once, none lost/duplicated), and a `tick` posted after `close()` is **dropped** by `$Closed` (the FSM gates cross-core posts by state, like local ones). Serial: `[smp] cross-core post: counter 800 (expected 800)` → `cross-core post -> Frame dispatch: ok` → `post-close tick ignored ($Closed gates it): ok`. Host-tested (snapshot + 4 behavioral) and validated by `smp_cross_core_post_b7` (**35/35** QEMU). So **B7-2 is met** and the long-flagged framec `Send`/`Sync` gate is *sidestepped by the architecture* — recorded as a headline finding in `frame_assessment.md`.
- **Step 4 (per-CPU LAPIC timer preemption; B7-1 met) — done.** Native, no Frame system. The legacy PIT only interrupts the BSP, so each AP runs its own **LAPIC timer** to be preempted. `kernel/src/lapic.rs`: map the LAPIC MMIO page (uncached; per-core — every core hits its own LAPIC at the same address), software-enable it, program a periodic timer on vector `0x40`. New IDT gate + naked ISR (`isr_lapic_timer` → per-CPU tick + LAPIC EOI) and a present spurious-vector (`0xFF`) gate; `gdt::load_on_ap` + `interrupts::load_idt_on_ap` so an AP loads our GDT/IDT (GDT *before* per-CPU init, since reloading `gs` zeros the GS base). Each AP then `sti`s and runs a busy loop preempted by its timer until it has been ticked `TARGET_TICKS` times — proving the core runs a real, *time-sliced* thread. Serial: `[smp] core 1: 5 timer ticks, 47216 work units` (… cores 2/3 similar, varying because they run independently) → `per-core preemption: ok (each AP timer-sliced)`. Validated by `smp_preempt_b7` (**36/36** QEMU; B0–B6 unaffected). **B7-1 met** (each core runs + is preempted; per-CPU run-*queues* / many threads per core is a further refinement).
- **Step 5 (TLB shootdown via IPI) — done.** B7's last named native component. When one core unmaps a page the others may still cache the translation, so the initiator must IPI them to `invlpg` it — and wait for every core to ack before reusing the page (the **shootdown barrier**). `lapic::send_ipi_all_but_self` (ICR, all-excluding-self shorthand) + a new IPI vector `0x41` / naked ISR (`invlpg` the shootdown VA + ack + LAPIC EOI) + `interrupts::shootdown(va)` (set VA, reset acks, send IPI). The APs idle *interrupt-enabled* (`sti; hlt`) after the preempt phase so they service the IPI. Demo: the BSP maps a test page, unmaps it (flushing its own TLB), IPIs the 3 APs, and waits for all 3 acks. Serial: `[smp] TLB shootdown: 3 of 3 cores flushed` → `TLB shootdown ack barrier: ok (safe to reuse page)`. Validated by `smp_tlb_shootdown_b7` (**37/37** QEMU).

**B7 status: substantially complete.** B7-1 (cores run + are preempted), B7-2 (cross-core `post` driving a Frame system), and the native components (AP startup, per-CPU data, the IRQ-safe `SpinLock` + documented lock ordering, the LAPIC timer, IPIs, TLB shootdown) are all done and validated; the smoke suite runs under `-smp 4` (B7-5). The headline Frame finding — **the post/drain architecture gives cross-core safety with no framec `Send`/`Sync` change** (B7-4's write-up) — is in [`frame_assessment.md`](frame_assessment.md). **Remaining (refinements, not blockers):** a real per-CPU run-queue scheduler (drive the existing `Scheduler`/`Process` Frame systems per core — expected to reuse the cross-core-post pattern), and a deeper deadlock/ordering stress (B7-3) as nested locks appear. **All seven B-track milestones (B0–B7) are now functionally demonstrated.** A retrospective synthesis is in [`capstone.md`](capstone.md).

## Post-B7 refinement track

The committed B0–B7 milestones are functionally complete. Forward work is
*refinement* — deepening what exists rather than adding new milestones. These are
**not strictly sequential**; they're ordered roughly by value to the project's
core purpose (stress-testing Frame on systems problems). Each lands green (gates +
QEMU smoke) like a milestone step, and each gets its assessment note.

The guiding question for prioritization: *does this teach us something new about
Frame, or is it native completeness?* The Frame-relevant ones come first.

### R1 — per-CPU run-queue scheduler (Frame-relevant)

Today the APs run a single timer-preempted loop (B7-1). The refinement: a real
**per-CPU run queue** so each core schedules multiple kernel threads/processes,
driving the *existing* `Scheduler`/`Process` Frame systems **per core**. The
prediction (from the B7 cross-core-post finding) is that this needs **no framec
change**: each core owns its scheduler/process instances; cross-core wakeups and
load-balancing arrive as `post`s into the target core's queue.

- **R1a (per-core Scheduler FSMs driven cross-core) — done.** Each AP owns its
  own `Scheduler` Frame instance (`kernel/src/ksched.rs`); the BSP `post`s
  `task_ready`/`task_unready` events into each core's MPSC queue, and that core
  drains them into *its* Scheduler — which goes `$Idle → $Active` (peak 3
  runnable) `→ $Idle`. Each instance stays pinned to its core; only the
  `SchedPost` data crosses; per-core results come back via atomics (the BSP never
  touches an AP's instance). Serial: `[smp] core N Frame scheduler: peak 3
  runnable, ended idle=true` → `per-core Frame schedulers driven cross-core: ok`.
  Validated by `smp_percpu_sched_b7` (**38/38** QEMU). **Confirms the B7
  cross-core-post finding holds under N real `Scheduler` instances** — the
  prediction held: no framec change. This is the scheduling *coordination /
  run-mode* layer per core.
- **R1b (per-core context-switched execution) — done.** Each AP now runs a real
  per-core run queue (`kernel/src/pcsched.rs`): it spawns kernel-thread workers and
  *time-slices* them under its own LAPIC timer, which became a full-frame
  context-switching ISR (`isr_lapic_timer` → `lapic_schedule` → per-core
  `pcsched::schedule`, pure native). Spawning/exiting each worker drives that
  core's own `Scheduler` Frame instance ($Idle→$Active→$Idle), every dispatch in a
  per-core interrupts-off critical section — the same native/Frame discipline as
  the BSP's `sched.rs`, replicated per core. Result: each of the 3 APs runs all 3
  workers to completion with **~42 context switches** and ends `$Idle`, and the BSP
  measures **~57 heap allocs** for the whole phase (3 cores × 6 dispatches ≈ **3.2
  allocs/dispatch** for the parameterless `Scheduler` events — lower than R2a's ~6
  for param-carrying TCP events, exactly as expected). Serial: `[r1b] core N:
  sliced 3 threads, 42 switches, ended idle=true` → `[r1b] per-core
  context-switched execution: ok`. Validated by `smp_pcsched_r1b` (**41/41** QEMU).
  **Answers the validation question:** per-event allocation behind the shared heap
  spinlock holds up with N cores scheduling concurrently — no corruption, no
  deadlock, all instances correct. **Scope:** kernel threads (ring 0) only;
  per-core *user processes* (ring-3-on-APs + per-CPU TSS.RSP0) are a separate
  native lift deferred to R5 — not needed for the Frame-relevant question.
  Recorded in `frame_assessment.md`.

### R2 — networking at scale: a TCP connection table (Frame-relevant)

B5 proved one `TcpConnection`. The open question from `frame_assessment.md`: does
**per-event allocation + per-instance dispatch** hold up with *many* concurrent
connections?

- **R2a (per-event allocation measurement) — done.** Added a counting wrapper to
  the bare-metal allocator (`allocator::alloc_count`) and an in-kernel stress
  (`tcp::scale_stress`): 16 `TcpConnection` instances created on the real heap,
  each driven through a full 7-event server lifecycle (112 dispatches), measuring
  heap allocations. Result: **656 allocs ≈ 6 per dispatch**, all 16 instances
  correct, **no OOM** on the 256 KiB heap. Serial: `[tcp] scale: 16 conns, 112
  dispatches, 656 heap allocs` → `6 allocs/dispatch, closed 16/16 connections`.
  Validated by `tcp_scale_alloc_b5` (**39/39** QEMU). **This finally quantifies
  the assessment's standing claim** — ~6 allocs/event is a non-issue for
  control-plane lifecycles (a connection is ~7 events) but confirms, numerically,
  why a per-*segment* data path is the wrong place for Frame (→ the no-alloc path,
  R4). Recorded in `frame_assessment.md` (sharpens finding #3).
- **R2b (live multi-connection server) — done.** Refactored `tcp.rs` from a single
  global connection to a real connection *table* (`Conn` slots, 4 listening server
  ports :7–:10 plus one client slot). Inbound segments are resolved by 4-tuple to a
  slot; a `CURRENT` ambient indirection lets the **unchanged** `TcpConnection` FSM's
  actions operate on the resolved slot, so N concurrent FSM instances each carry
  their own seq state and dispatch independently. The harness opens **4 simultaneous
  connections** (`TcpProbe::Multi`, hostfwd :8/:9/:10), echoes on each, and the
  kernel reports `[tcp] served 4 connections`. Per the R2a prediction, the
  allocation number is unchanged — this exercised the full receive→resolve→dispatch
  path at N live connections rather than direct-drive. Validated by
  `tcp_multi_conn_b5`; the four B5 single-connection TCP tests still pass after the
  table refactor (**40/40** QEMU). Recorded in `frame_assessment.md`.

### R3 — multi-port USB / orthogonal regions (Frame-relevant)

B6 was single-port/single-device. The roadmap flagged **orthogonal regions
(multiple ports concurrently)** as a framec gate that B6 didn't exercise.
**Validates:** many concurrent lifecycle FSMs of the same type; the "orthogonal
regions" question.

- **R3a (multi-port concurrent enumeration) — done.** Refactored `xhci.rs` from
  single-flight globals to a per-device table (`Device` slots, `CUR_DEV` ambient
  index — the `tcp.rs` connection-table pattern), so the *unchanged* `HubPort` /
  `UsbEnumeration` / `UsbTransfer` FSM actions operate on "the current device."
  Two HID devices (`usb-kbd` on port 5, `usb-mouse` on port 6) are now brought up
  **concurrently**: one `HubPort` + one `UsbEnumeration` instance per device
  coexist, and a single driver loop demuxes each xHCI completion to the right
  instance **by slot** (`dev_by_slot`), pointing `CUR_DEV` at it for the dispatch.
  Slot assignment is the one serialized step (Enable Slot carries no port, so an
  *unbound* returned slot is bound to the requesting device); everything after
  carries the slot and interleaves. Serial: `slot 1 enabled → addressed → slot 2
  enabled → addressed → configured (slot 1) → configured (slot 2) → enumerated 2
  of 2 devices`. This is R2b's connection-table answer applied to USB but driven
  by **real asynchronous hardware completions** rather than synthetic events.
  Validated by `usb_multiport_r3a`; the four B6 single-device tests still pass
  (keyboard stays device 0 / slot 1, so the keypress transfer is unaffected).
- **R3b (mass-storage bulk/SCSI) — done.** Added a `usb-storage` device (backed by
  a raw image with a magic in block 0). It enumerates alongside the HID devices;
  because it is USB3 it sorts onto a lower port, so device identity moved from table
  index to **interface class**: `classify_devices` reads each device's configuration
  descriptor (the first interface's class/protocol + bulk endpoint addresses) and
  routes by class — the keypress transfer to the HID keyboard, SCSI to the
  mass-storage device. `run_msd` then configures the device's **bulk** IN/OUT
  endpoints (a new endpoint type vs the HID interrupt-IN) and drives three SCSI
  commands — `INQUIRY`, `READ CAPACITY(10)`, `READ(10)` of block 0 — each through one
  `UsbMsd` Frame instance's Bulk-Only Transport phase lifecycle
  (`$CommandPhase`→`$DataPhase`→`$StatusPhase`, CBW → data → CSW). Native owns the
  CBW/CSW byte layout, the SCSI CDB, the bulk rings + TRBs, and the CSW validation;
  Frame owns the BOT phase lifecycle. Serial: `bulk endpoints configured (IN + OUT)`
  → `INQUIRY vendor 'QEMU' product 'QEMU HARDDISK'` → `capacity: 128 blocks of 512
  bytes` → `block 0 first 8 bytes: FRAMEOS!` (proof of a real media read). Validated
  by `usb_msd_r3b` + `usb_msd_behavior` (4 host tests) + a state-graph snapshot.
  A genuinely new device class + transfer type. **R3 complete.**

### R7 — message-passing internals: scheduler-as-actor + a per-core reactor (Frame-relevant, near-term)

The multithreading-safety analysis (`frame_assessment.md`, 2026-05-22) established the
governing rule for this OS: **post across contexts, call within a context.** We already
honor it at the hard boundaries (ISR→mainline post/drain, core→core MPSC posts,
hardware→software via the event/used rings). This milestone pushes message-passing
*deeper* where it's a genuine win — ranked by signal:

- **R7a — scheduler-as-actor — done.** `pcsched.rs` no longer drives the `Scheduler`
  through a shared lock. A per-core **mailbox** (`SchedMailbox`, IRQ-safe `SpinLock`)
  carries `SchedMsg::{Ready,Unready}`; workers and the spawn path only *post* into it,
  and the idle loop is the **sole** drainer that dispatches those messages to the FSM
  and reads `is_idle()`. Exactly one context touches the instance, so the
  `with_sched` / `without_interrupts`-around-dispatch dance is gone — the one remaining
  critical section (in `exit_current`) guards the Dead-mark + mailbox push, not an FSM
  dispatch. Same `.frs`, unchanged; the coupling became a queue. This unifies R1a and
  R1b on one model (both now post/drain) and is the native hand-rolling of RFC-0038's
  deferred-send / `@@[cast]` primitive. Behavior-preserving: `smp_pcsched_r1b` still
  reports 3 threads sliced / ~40 switches / ended idle, deterministically; full suite
  green. Recorded in `frame_assessment.md`.
- **R7b — one mailbox primitive — done.** The "converge the ad-hoc drains" goal
  resolved, on investigation, to a specific duplication: the kernel had **three
  byte-for-byte-identical hand-rolled FIFO rings** (`crosscore::PostQueue`,
  `ksched::SchedQueue`, `pcsched::SchedMailbox`) — the *only* software event queues in
  the OS. R7b extracts one generic primitive, `reactor::Mailbox<T, CAP>` (fixed-capacity,
  no-alloc, `const`-constructible, `Option`-buffer so any `T` works), and the three sites
  now share it (each still wraps it in a `SpinLock` for cross-context use). Behavior
  preserved: `smp_cross_core_post_b7`, `smp_percpu_sched_b7`, `smp_pcsched_r1b` all green.
  This is the native realization of RFC-0038's standard mailbox. (The grander "single
  per-core reactor loop owning all instances" is the microkernel rewrite flagged below as
  out-of-scope; the *primitive* is the valuable, low-risk part and is what landed.)
- **R7c — align block + USB completion: resolved by investigation, no change.** The
  premise ("route block/USB completions through the same per-core mailbox as net") does
  not hold: net's `RxPipeline`, B4 block, and B6/R3 USB do **not** use software event
  rings — they drain the **hardware** rings directly (virtio used-ring + an `IRQ_PENDING`
  wakeup flag for block; the virtio RX ring + a staging buffer for net; the xHCI event
  ring via synchronous poll loops for USB). The hardware ring *is* the queue; layering a
  software `Mailbox` on top would duplicate it (incorrect) or add indirection with no
  benefit (a single-pending `AtomicBool` wakeup is the right shape for those paths). So
  there is nothing correct to converge here — R7b already unified every genuine software
  mailbox. The `reactor::Mailbox` primitive is available should a future software-event
  path appear. (Recorded as a finding rather than a forced refactor.)

**Explicitly *not* in scope** (ceremony, not value): query paths (`is_idle()`/`state()`
are reads, not events), the synchronous syscall fast path (microkernel-only payoff),
register/stack mechanics, and intra-context RTC chains where the caller needs the result
now. A full microkernel-ization (each subsystem an isolated message-only server) is a
*research* fork — philosophically pure Frame, but it taxes every cross-subsystem call
with a queue hop; not a committed milestone.

**The framec lesson underneath R7** (feeds R4): the post/drain pattern is currently
entirely *hand-rolled native* (a `SpinLock` ring + a `match` at each drain site). Frame
has `$>`/`<$`, transitions, and typed event payloads (RFC-0025), but the *queue* is
native boilerplate re-implemented each time. The missing actor-model primitive is a
**deferred send** — "enqueue event E to instance I, dispatched by the runtime loop, not
now." With it, R7a–R7c become a Frame idiom instead of native plumbing; it wants pooled,
no-alloc event storage (→ R4 / RFC-0036) and rides on typed events (RFC-0025). The
standing observation: *the runtime spec describes synchronous run-to-completion dispatch,
but every real systems use we've hit needs a queued, deferred dispatch mode, and there is
no Frame-level construct for it.* Worth a framec RFC.

### R4 — the no-alloc / preallocated event path (framec-relevant)

`frame_assessment.md` flags this as *the single highest-value framec change for
systems use*: a dispatch path that doesn't allocate per event. This is primarily a
framec investigation (can the generated `FrameEvent`/context use a preallocated
pool or stack storage?), filed to the transpiler team. If it lands, it removes the
root cause behind post/drain and the hot-path verdict — re-run R1/R2 against it.

### R5 — deeper SMP correctness (native)

Completes B7-3 beyond the leaf-lock stage. Mostly native; the payoff is robustness.

- **R5a — lock ordering + nested-lock deadlock stress — done.** `SpinLock` gained an
  optional **rank** (`with_rank`; `new` stays a rank-0 leaf, unchecked) and a per-CPU
  held-rank checker that **panics at the acquire** if a core tries to take a lock whose
  rank ≤ the highest it already holds — catching an ordering reversal *before* it can
  deadlock against another core. `lockorder.rs` exercises it: two ranked locks (A rank 1,
  B rank 2), every core (BSP + APs) runs `A→B→bump both→release` 20000× concurrently. The
  counters end at exactly cores × 20000 (= 80000) on both iff every nested increment
  serialized with no lost update and no deadlock. Serial: `[smp] nested-lock stress:
  A=80000 B=80000` → `nested-lock ordering: ok`. Validated by `smp_nested_lock_r5`.
- **R5b — per-CPU TSS + IST — done.** APs no longer share the BSP's single TSS. `gdt.rs`
  now holds a TSS per core (GDT grew a TSS descriptor per CPU; `tss_selector(cpu)`), each
  with its own **double-fault IST stack** (`TSS.ist[0]`); the BSP builds them all in
  `gdt::init`, and each AP `ltr`s its own in `load_on_ap(cpu)`. IDT vector 8 (#DF) routes
  through **IST1**, so a fault on any core lands on a known-good per-core stack instead of
  triple-faulting. `set_rsp0` is now per-CPU (`this_cpu_index`); the B3 ring-3/syscall
  path (BSP = core 0) is unchanged (all 6 B3 tests still pass). Each core verifies its own
  loaded TR via `str`. Serial: `[smp] per-CPU TSS+IST: 4 of 4 cores armed (#DF -> IST1)` →
  `ok`. Validated by `smp_percpu_tss_r5`.
- **R5c — sleep-locks — deferred.** "Block rather than spin for long holds" needs a
  blocking scheduler to yield/resume a waiter, which the current run-to-exit AP model
  (`pcsched`) doesn't provide — it's an R1-track scheduler extension, not lock work.
  Deferred with rationale rather than half-built; the leaf/ranked spinlocks cover the
  current short-critical-section needs.

### R6 — the crates.io CI gate (tooling)

B5-7/B6/B7's crates.io CI is red because the kernel uses typed-context framec
(struct enter params) that isn't published yet. Unblock by publishing the
typed-context framec (the transpiler team's call), then green the GitHub CI matrix
to match the locally-green dev container.

## V1.0 self-hosting track (B8–B13)

**The V1.0 north star:** *run framec on Frame OS to compile a hello-world program in
both C and Rust, and run them from an interactive shell.* This is the
self-hosting milestone — the OS hosts its own toolchain.

**The architectural key — funnel every language to one on-device C compiler.** A
full `rustc`+LLVM port is a multi-year, Redox-scale effort (LLVM assumes threads,
mmap, a host linker, a filesystem). Instead, make every path transpile down to one
native C compiler:

```
Frame ──framec──▶ C    ──tcc──▶ ELF                 (C path)
Frame ──framec──▶ Rust ──mrustc──▶ C ──tcc──▶ ELF   (Rust path, no LLVM)
```

`mrustc` (Rust→C) dodges LLVM entirely: once `tcc` is on-device, "Rust on Frame OS"
becomes "port mrustc," not "port LLVM."

**Foundation already in place:** a writable filesystem (`fs::create`/`write_file`),
a ring-3 Frame-driven shell skeleton that tokenizes with the `Parser` FSM
(`user/frameshell.rs`, currently baked-script), a userspace heap (fixed 64 KiB
today), and `fork`/`exec_path`/`wait` + the ELF loader. The shell and "run a program
from it" are largely built; the gaps are input, a growable heap, and the toolchains.

- **B8 — interactive console — done.** Serial RX
  (`serial::rx_byte` + IRQ4 → `console.rs` line discipline: echo, backspace, a byte
  FIFO), a blocking `read_line` syscall (#9; yields via `block_current` until a
  newline), and a real ring-3 REPL `ish` (`user/ish.rs`): prompt → `read_line` →
  tokenize with the same `Parser` FSM the hosted shell uses → builtins (`help`,
  `exit`, `cat`) or fork+exec a program from disk (`/bin/<cmd>`). Gated behind the
  `interactive` cargo feature so the default kernel + the 45-test suite are untouched;
  validated by `cargo xtask console-test` (boots the interactive kernel over
  `-serial stdio`, types `/bin/hello`, asserts it runs from disk, types `exit`).
  **You can type `/bin/hello` at a `frameos$` prompt and a disk-loaded program runs,
  then the shell returns to the prompt.** Three substrate-level (not Frame) bugs were
  found and fixed to get here:
  - A **blocking-I/O bug** — virtio-blk reads busy-`hlt`'d for the completion IRQ
    instead of yielding; now a scheduled process blocks (`block_current`) and is woken
    on the IRQ (`sched::wake_pid`). This is also what lets a *forked child's* disk read
    complete: it yields the CPU so the IRQ can be delivered and serviced.
  - An **interactive build booted too slowly** — it re-ran the entire B0–B7 self-test
    suite (billion-iteration SMP spin loops) before reaching the prompt. An interactive
    build now boots *straight to a shell*; the self-tests are gated to the default build.
  - A **`#PF` at address 0** on the first switch into a ring-3 process — `gdt::set_rsp0`
    reads this core's index via `gs:[0]`, but the BSP's per-CPU GS base was still zero
    because its init (`percpu::init_this_cpu`) lived *inside the SMP demo block* the
    interactive build skips. The interactive path now initializes BSP per-CPU state
    explicitly before running any user process.
- **B9 — process/OS services the toolchains need.** A **growable heap** (`brk`/`mmap`
  syscall + the kernel grows the process address space — toolchains need MBs, not the
  current 64 KiB static), `argv`/`envp` through `exec`, and the missing syscalls
  (`lseek`, `stat`, write-to-fd, `getcwd`, `dup`). *Bounded but substantial.*
  - **B9-1 — growable heap via `brk` — done.** Syscall #10 `brk(new_end)`:
    query (`0`), grow (the kernel demand-maps fresh zeroed `USER|WRITABLE` pages over
    `[break, new_end)` into the process's own address space), or shrink (unmap + free).
    The break is per-process (`Tcb::heap_brk`), starting at a dedicated VA region
    (`sched::USER_HEAP_BASE = 0x3000_0000`, clear of the image at `0x1000_0000` and the
    stack at `0x2000_0000`); a `fork`ed child inherits it (the heap pages are copied with
    the rest of the user half), and `exec` resets it (fresh image ⇒ empty heap). Heap
    pages are reclaimed by the existing `free_address_space` teardown on reap. Validated
    by `brktest` (grows its heap 1 MiB, writes + verifies a pattern across every page) +
    the `brk_growable_heap_b9` smoke test.
  - **B9-2 — `argv` through `exec` — done.** Syscall #11 `exec_argv(buf, len, argc)`:
    `buf` is `argc` NUL-terminated strings, `argv[0]` is the program path (the Unix
    convention). The kernel loads the ELF from disk, then writes a System V x86-64
    initial stack — `argc`, `argv[]`, NULL, `envp` NULL, `auxv` AT_NULL, with the string
    bytes copied to the top of the page — onto the new program's stack, entering it with
    `rsp` at `argc`. The shell (`ish`) packs the parsed tokens into that buffer, so typed
    arguments reach the program; `argtest` reads them via a tiny asm `_start` shim that
    hands the entry `rsp` to Rust. Validated by `console-test` (`/bin/argtest alpha beta`
    → the program echoes `argv[1]=alpha`, `argv[2]=beta`).
  - **B9-3 — the file write path — done.** The fd API gained the syscalls a
    libc/toolchain needs to write output and stat inputs: `open` (#5) takes a flags arg
    (bit0: read/write, back-compatible), and new calls `write` (#12, to a file at the
    fd's offset), `lseek` (#13, SET/CUR/END), `fstat` (#14, size), `stat` (#15, size by
    path), and `dup` (#16, shared-offset descriptor). Backed by a new `fs::write_at`
    (random-access write that allocates blocks + grows size) over the existing
    `OpenFile` access-mode FSM (a read-only fd's writes are dropped, vice versa).
    Validated by `fwtest` (creates `/tmp.txt`, writes, overwrites the middle via a seek,
    fstat's, reopens and verifies — incl. a dup'd fd sharing the offset) + the
    `file_write_roundtrip_b9` smoke test. *Deferred: `envp` and `getcwd` (need a real
    cwd notion) — both land naturally with `frame-libc` (B10), which will own them.*
- **B9.5 — toolchain-ready filesystem — done.** Surfaced by B10: the first
  libc-linked program (8.5 KB) blew past the old **7 KB** file cap (14 direct blocks ×
  512). The FS got a scalable format: `INODE_SIZE` 64→128 with **28 direct + single +
  double indirect** block pointers (max file **~8 MiB**), and a **multi-block bitmap**
  derived from disk size (`Layout::for_total`, addressing up to ~2 TB). The kernel maps
  any file block through `block_for` (direct → single → double indirect, lazily
  allocating index blocks); the host `mkfs` writes the new layout (direct blocks only —
  staging a binary >14 KB, e.g. tcc, will teach it indirect at B11). The classic Unix
  insight applies: **geometric capacity from linear code** — each indirect tier is one
  more bounded branch. The default test disk is bumped to 4 MiB (exercises the
  multi-block bitmap); the format scales far beyond. Validated by a 128 KiB
  double-indirect round-trip folded into `fs_file_roundtrip_b4`.
- **B10 — userspace runtime: `frame-libc` + a `std` platform port.** A C/POSIX-ish
  library (malloc/free over `brk`, file I/O, string, stdio, `exit`) for tcc, **and** a
  Rust `std` backend (`std::sys::frameos`) on top, so framec (Rust + std) can be built
  for the OS. The Redox model. *Large — the "real program-hosting OS" milestone.*
  - **B10-1 — `frame-libc` crate + crt0 — done.** A new `libc/` crate
    (`frame-os-libc`, `no_std`) exposing an `extern "C"` surface: crt0 (`_start` parses
    the kernel's SysV initial stack and calls `main`, generalizing the B9-2 `argtest`
    shim — every program that links the libc gets a real `_start` and just writes
    `main`), syscall thunks, `write` (stdout/stderr → console, else file), `exit`,
    `strlen`. The `cmain` test program is `#![no_main]` and pulls `_start` from the libc
    — exactly the entry path a tcc-compiled C program will take. Validated by
    `console-test` (`/bin/cmain one two` → crt0 → `main` echoes `argc=3`, `argv[2]=two`).
  - **B10-2 — `malloc`/`free`/`calloc`/`realloc` over `brk` — done.** The libc heap
    grows the program break (syscall #10) on demand and runs first-fit over it
    (`linked_list_allocator`); a 16-byte size header makes C `free(ptr)` work without a
    caller-supplied size, and `realloc` preserves contents. Validated by `cmain`
    allocating 200 KiB (forcing a `brk`-grow past the initial 64 KiB chunk), writing +
    verifying a pattern, `realloc`-growing it, and freeing — asserted by `console-test`.
  - **B10-3a — `printf` format-scanner FSM + conversion engine — done.** The **first
    of frame-libc's two Frame systems**: `PrintfScan` (`libc/frame/printf_scan.frs`), a
    per-char scanner over the format string — the same shape as the shell `parser.frs`,
    now compiled a *fourth* time. It emits a directive plan (`Lit` / `Conv{flags,width}`)
    that the native engine (`printf.rs`) renders: number→string for `d i u x X c s p`,
    `%%`, and `0`/`-`/width padding. Frame owns the parsing modes; native owns the bytes.
    Arguments arrive as an explicit `&[Arg]` slice (the Rust-friendly front); the
    C-variadic `printf(fmt, ...)` ABI shim is deferred to B11 (with tcc). frame-libc now
    registers a `#[global_allocator]` over its own `malloc` so the FSM's generated code
    (Vec/Rc/BTreeMap) and the engine can allocate. This also pulled **host indirect-block
    staging** forward (the printf-laden cmain is 33 blocks > the 14 KiB direct limit):
    `mkfs` now writes single/double indirect, mirroring the kernel — so the host stages
    files up to ~8 MiB (tcc-scale). Validated by `console-test` (`/bin/cmain` prints
    `d=-42 u=42 x=ff X=FF c=Q s=world p=0xdead pad=[    7][7    ][00007] pct=%`).
  - **B10-3b — buffered `FILE*` streams — done.** The second Frame system, by
    **reuse**: frame-libc compiles the kernel's own `frame/open_file.frs` to gate a
    `FILE*`'s read/write mode (the *same* FSM the VFS uses — one source, two targets, a
    lifecycle FSM this time, not just the parser). The eof/error indicators are native
    sticky flags (a fixed property + two booleans, not a lifecycle), so `feof`/`ferror`/
    `clearerr` are native while the mode gate is Frame. Real `extern "C"` stdio:
    `fopen`/`fwrite`/`fread`/`fputs`/`fputc`/`fflush`/`fclose`/`feof`/`ferror`/`clearerr`
    (non-variadic, so C-ABI now), plus `stdout`/`stderr` (console-backed) and a
    `fprintf_args` driving the printf engine into a stream (the variadic `fprintf(f,
    fmt, ...)` waits for B11). Validated by `console-test`: `/bin/cmain` fprintf's to the
    console, writes `/gen.txt` via fprintf+fputs, reopens it, reads it back, and confirms
    `feof`. 
  - **B10-4 — line input: `fgetc`/`fgets` + input buffering — done.** The stream's
    read side gained an input buffer (`ibuf`/`ipos`): `fread`/`fgetc`/`fgets` all drain
    it, refilling via the file `read` syscall or — for `stdin` (fd 0, the console, which
    has no plain readable fd) — the blocking `read_line` syscall (#9). `fgetc`, `fgets`
    (newline-keeping, NUL-terminating, NULL at EOF), `getchar`, and `stdin` are added;
    `fread` reworked through the buffer. Validated by `console-test` (`/bin/cmain`
    reopens `/gen.txt` and reads it back a line at a time with `fgets`, then NULL at EOF).
    The C-side libc core (crt0, malloc, printf, full `FILE*` r/w) is now functionally
    complete for a C hello-world. *Next: B10-5 the Rust `std` platform port — the heavy
    half, so framec itself can be built for the OS; the C-variadic `printf`/`fprintf`
    shim lands at B11 with tcc.*
- **B11 — on-device C toolchain (tcc).** Port **tcc** to build against `frame-libc`
  and emit Frame-OS ELF: `cc hello.c -o hello` from the shell. tcc is the right choice
  — small, single-pass, self-contained, designed for exactly this. ✅ **"compile
  hello-world in C and run it from the shell"** (framec emits C → tcc → ELF → run).
  - **B11-1 — C-variadic `printf`/`fprintf` — done.** The deferred shim, on stable
    Rust via a hand-rolled SysV va_list (no nightly). A naked trampoline spills the
    vararg integer registers (printf: rsi–r9; fprintf: rdx–r9) to a stack save area and
    calls a Rust impl that walks them + the stack overflow, feeding the existing scanner
    + conversion engine. Integer/pointer only — we don't support `%f`, so the SSE
    registers are never touched. (*Calling* a variadic extern is stable Rust, so `cmain`
    tests it through the real C ABI: `printf("d=%d x=%x s=%s c=%c", -7, 0xbeef, "hi",
    'Z')` → `d=-7 x=beef s=hi c=Z`.) frame-libc's printf is now genuinely C-callable —
    what a tcc-compiled program emits. *Next: B11-2 a host-cross-compiled C program runs
    on Frame OS (frame-libc as a `.a`); B11-3 tcc itself on-device.*
- **B12 — framec on-device.** Cross-compile framec (Rust + std) for the Frame OS
  target — possible because B10 ported std. `framec hello.frs -l c` runs on the OS.
  *Large, but framec is a big Rust program, not a toolchain — and it dogfoods itself
  (58 Frame systems).*
- **B13 — on-device Rust path (mrustc → tcc).** Port **mrustc** (Rust→C) so
  framec-emitted Rust → mrustc → tcc → ELF, all on-device, no LLVM. ✅ **"compile
  hello-world in Rust and run it from the shell."** *The hardest port (mrustc is large
  C++, pinned to a specific Rust version) — but the only realistic on-device Rust.*

**Feasibility, honestly.** B8–B11 (shell → libc → tcc) is a long but tractable
sequence; C-on-device is real. B12 (framec on-device) is achievable once std is
ported. B13 (Rust on-device via mrustc) is the wall — easily as much work as
B8–B11 combined — but the mrustc+tcc chain makes it *possible* without LLVM. Total
scope exceeds all of B0–B7 combined; it decomposes cleanly, each milestone
independently demonstrable with a serial-console smoke test.

**The Frame angle.** The shell REPL and a `make`/`cc`-like **build driver**
orchestrating `framec → mrustc → tcc → link` (a pipeline FSM with a `$Failed` sink)
are textbook Frame: a compiler toolchain whose driver is written in the language it
compiles.

### Portability / other forward tracks (post-V1.0, not committed)

- **P1 — HAL extraction (`arch/` boundary).** *Prerequisite for any port; valuable on
  its own.* Today the kernel is monolithically x86_64 — ~17 modules carry x86-only
  machine mechanics (`gdt` segmentation, `interrupts` IDT + naked `iretq` stubs,
  `usermode` syscall/sysret, `paging` CR3, `percpu` `IA32_GS_BASE`, `pic`/`pit`/LAPIC,
  port-I/O in `serial`/`io`/`pci`, the `context` switch). Carve these behind an
  `arch::` boundary using **module-swap + an enforcing trait** (the Linux/Redox
  pattern, plus a `trait Arch`/`Mmu`/`InterruptController`/`Timer`/`Console` so the
  compiler checks each arch provides the contract; compile-time `cfg` selection ⇒ zero
  runtime cost). Two phases:
  - **P1.0 — extract `arch/x86_64/` with no second arch.** A pure refactor: move the
    machine modules behind `arch::`, route the rest through it, keep all smoke tests
    green. Extracting the boundary is how the real interface is *discovered*, and it
    makes the project's thesis literal in the tree: **the `arch::` line is exactly the
    Frame/native line** — everything above (the 26 FSMs + high-level Rust) is already
    arch-neutral and ports for free; everything below is per-arch. Valuable as
    documentation/structure even if no port ever ships.
  - **P1.1+ — implement a second arch backend** (`arch/aarch64/` or `arch/riscv64/`)
    against that interface. Note the structural differences a HAL must absorb, not just
    different constants: ARM/RISC-V have **no segmentation** (GDT/TSS vanish), an
    **exception-vector table** instead of an IDT, **MMIO-only** I/O (no port I/O — so
    the legacy 8259/8254/16550-via-ports drivers are x86-only; ARM brings GIC + generic
    timer + PL011 + ECAM behind the same HAL interfaces). Naked trap-entry stubs and the
    context switch stay per-arch asm by nature.
- **AArch64 / Raspberry Pi port.** The concrete instance of P1.1 (the README lists Pi
  Pico / Pi 4/5 as intended runtime targets). Limine already supports x86_64, AArch64,
  and RISC-V, so the boot handoff is portable; serial/UART-first console (B8) is the
  natural bring-up path on those boards. Not committed.
- A device/power state-machine showcase (ACPI sleep states) — a *possible* future
  Frame demonstration, not committed.

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

## B11-3 decisions & short-term follow-ups (tech debt)

Recorded 2026-05-24, while porting `frame-libc` for the on-device C compiler
(tcc, B11-3). Two parts: a design decision (floating point), and an honest list
of the shortcuts/stubs taken to get tcc linking, each scheduled to be paid off.

### Decision: floating point in frame-libc

The B11-3 float scope was chosen as **full float** (the earlier "enable FPU +
save state" call, B11-3a). The implementation splits by what each language owns:

- **Pure Rust (the bulk):** the variadic printf trampolines spill `xmm0–7` so
  `%f`/`%e`/`%g` arguments — which the SysV ABI passes in SSE registers — are
  readable; `strtod`/`strtof` parse; `ldexp` scales; the printf engine renders
  `f64`. This is the real work, in Rust, on top of the B11-3a FPU-save.
- **One-function gcc C shim for `strtold`:** x86 `long double` is the 80-bit x87
  extended type, which **Rust has no type for**. gcc implements it natively, so
  `strtold` lives in `libc/csrc/strtold.c`, compiled by the cross-gcc and linked
  alongside the Rust staticlib by xtask. This is *correct*, not a shortcut — the
  shortcut would be faking `long double` as `f64` (wrong precision + wrong ABI:
  f64 returns in xmm0, `long double` in st0). It is quarantined to build wiring
  so the Rust crate stays pure Rust, and it is the *only* 80-bit surface tcc
  needs (tcc's formatting is all `double` — no `%Lf` — and it uses `ldexp`, not
  `ldexpl`). Downside accepted: frame-libc is no longer 100% Rust; mitigated by
  keeping the C surface to a single, quarantined function.

### Short-term follow-ups (pay these off next)

Shortcuts taken to get tcc to link/compile, in rough priority order. None are
load-bearing for *integer* C compilation, but each is a real gap:

1. **`assert` is a no-op** (`libc/include/assert.h`) — disables tcc's internal
   sanity checks, which could mask a real miscompile. Make it `abort` on failure
   (needs a minimal `__assert_fail`).
2. **`unlink`/`remove` return -1** (`libc/src/posix.rs`) — Frame OS has no
   file-delete syscall. Add one (kernel `fs::unlink` + a syscall) so tcc temp
   files and overwrite paths work.
3. **`getcwd` returns "/"** — Frame OS has no per-process cwd. Add a real cwd
   (per-process state + `chdir`/`getcwd` syscalls) when the shell needs it.
4. **`time`/`localtime`/`gettimeofday` return a fixed epoch** — no RTC/clock
   source yet. Wire a real time source (PIT/HPET/TSC or CMOS RTC) so `__DATE__`/
   `__TIME__` and timing are real.
5. **`execvp`/`mprotect` are stubs** — `execvp` is genuinely unused (tcc is
   self-contained, no external `as`/`ld`); `mprotect` only serves tcc's unused
   `-run` JIT. Acceptable as stubs, but documented so they aren't mistaken for
   working.
6. **tcc's `-run` JIT is compiled but unused** — `CONFIG_TCCBOOT` drops the
   backtrace path; the in-memory `-run` path remains (needs only the `mprotect`
   stub). Frame OS's tcc writes ELF to disk + the shell execs it, so `-run` is
   dead weight. Could be excluded entirely with a small upstream-config change.
7. **`ELF_BUF`/`ARGV_BUF` single-flight** — RESOLVED. The exec scratch buffers
   were shared statics that assumed one `exec` in flight; but `exec`'s disk read
   *blocks* (re-enables interrupts and yields), so two processes exec'ing
   concurrently across that read could clobber the shared buffer (the first
   process's loader would then map the second's program). Fixed by allocating the
   ELF image and the packed argv *per-exec on the kernel heap*
   (`read_exec_elf`/`free_exec_elf` plus a per-call argv `Vec` in
   `kernel/src/usermode.rs`), freed after the synchronous load. Regression guard:
   the `coexec` program + `concurrent_exec_buffers` smoke test (two children exec
   different disk programs at once; argtest's `argv[1]=Z` must survive).
8. **User stack is one page** (`kernel/src/elf.rs`) — RESOLVED in B11-3d: the
   loader now maps a 32-page (128 KiB) user stack (`USER_STACK_PAGES`); tcc's
   recursive-descent parser overflowed 4 KiB. Also bumped the kernel heap to
   8 MiB (`exec` reads each image into a per-exec heap buffer; tcc is ~1.2 MiB)
   and `MAX_TRACKED` to 512 (failed-load rollback covers tcc's ~104 PT_LOAD pages).
9. **`strtod`/`strtof` precision** — the Rust implementations target correctness
   for common cases, not last-bit IEEE rounding; revisit if a float program's
   output proves sensitive.

These are tracked here rather than silently carried; B11-3d/e and beyond should
burn them down (especially #2/#3/#4, which the toolchain genuinely wants).

### B11-3d done (2026-05-24): tcc compiles + runs C on-device

`tcc -B/usr/lib/tcc -static /hello.c -o /out.elf` then `/out.elf` works at the
shell — the C half of the V1.0 north star. Key outcomes (full detail in
`docs/frame_assessment.md`):

- **C-shim libc** (`libc/cshim/cshim.c`, `gcc -fno-pic -fvisibility=hidden`) is
  tcc's link target — no GOT, no PLT, only the simple relocations tcc applies
  correctly. tcc stays **pristine 0.9.27** (the static-link PLT/GOT bugs are real
  and fixed in tcc `mob`, but adopting mob was *measured and rejected* — heavy
  recurring freestanding port for features we don't use; the C-shim is smaller +
  faster on-device). Rust `frame-libc` remains the OS's own runtime.
- **Five kernel/libc bugs fixed** (0-length read/write NULL-deref; scheduler
  TCB-slot leak; non-page-aligned ELF segments; #PF RIP diagnostic; printf `%*`).

New follow-ups from B11-3d (lower priority — none block the V1.0 goal):

10. **C-shim libc is minimal** (`libc/cshim/cshim.c`) — printf handles
    `%d/%u/%x/%c/%s/%p` only; `malloc` is a bump allocator (`free` is a no-op).
    Grow as on-device C programs need more (full printf, real free, more libc).
    Consider sharing the printf scanner FSM with frame-libc instead of two impls.
11. **`unlink` (#2 above) matters more now** — bigger on-device compiles will
    want tcc temp files + output overwrite; add the file-delete syscall.
12. **`qemu-test` artifact thrash** — flipping the kernel's `interactive` feature
    between `console-test` and `qemu-test` in the same `/target` forces full
    rebuilds and can leave a stale/half-written ESP (looked like an all-tests
    empty-capture failure; was a flake, 49/49 green after clean rebuild). A
    separate target dir per feature set, or a `clean` between, would prevent it.

### B11-3e (next): BuildDriver Frame FSM

Frame's turn on this track: a `BuildDriver` Frame system (`.frs`) that
orchestrates the on-device toolchain — `$Idle → $Compiling → $Linking →
$Running → $Done`, with a `$Failed` sink that reports which phase failed and the
exit code. Models the compile→link→run pipeline as an explicit state machine
(the "Frame owns lifecycle, native owns mechanism" split), driven from a user
program that shells out to `/bin/tcc` + execs the output.

### B11-3e done + toolchain tech-debt burn-down (2026-05-24)

`BuildDriver` shipped (`frame/builddriver.frs` → `/bin/buildc`): the compile→
link→run pipeline as a Frame FSM with a `$Failed` sink. console-test drives it
and asserts `[build] pipeline ok; /out.elf exited with code 7`.

Then started paying down the toolchain follow-ups from above:

- **#11 `unlink` — DONE.** New syscall #17 (`kernel/src/fs.rs::unlink`: namei the
  parent dir, refuse directories, free blocks, clear the dirent). Wired through
  frame-libc (`sys_unlink`, real `unlink`/`remove`) and the C-shim
  (`unlink(const char*)`). `fwtest` step 7 unlinks `/tmp.txt` and confirms a
  subsequent open-for-read fails; smoke `file_write_roundtrip_b9` asserts it.
  Bigger on-device compiles can now manage temp files + overwrite outputs.
- **assert → real abort — DONE.** `libc/include/assert.h` was a no-op
  (`((void)0)`); now `assert(expr)` calls `__assert_fail(#expr, __FILE__,
  __LINE__, __func__)` which prints `file:line: func: Assertion `expr' failed.`
  to stderr and `abort()`s (exit 134), in both the C-shim (tcc-linked programs)
  and frame-libc (direct-link). tcc itself is built `-DNDEBUG` so its *own*
  internal asserts stay compiled-out (its current behavior) — the on-device tcc
  never passes `-DNDEBUG`, so user programs' asserts are live. Validated by
  console-test compiling+running `/assert.c` (a false assert) and matching the
  diagnostic (`csrc/tcc_assert.c`).

- **Real clock (CMOS RTC) — DONE.** New `kernel/src/rtc.rs` reads the
  MC146818 CMOS RTC (0x70/0x71, UIP wait + double-read, BCD/12-hour decode) and
  folds it to Unix epoch seconds; exposed as syscall #18 (`time()`). frame-libc's
  `time`/`gettimeofday`/`localtime` (previously fixed stubs) are now real —
  `localtime` does the `civil_from_days` epoch→calendar conversion. tcc reads
  these while preprocessing `__DATE__`/`__TIME__`, so a compiled program carries
  the real build date. QEMU pins `-rtc base=2026-05-24T12:00:00,clock=vm` for
  determinism; `cmain` prints `clock 2026-05-24 12:…` and console-test asserts
  it.

- **Per-process cwd + chdir/getcwd — DONE.** Each TCB now carries a canonical
  absolute cwd (`fork` inherits it, `exec` keeps it, fresh processes start at
  "/"). New syscalls #19 `chdir` and #20 `getcwd`; `fs::resolve` canonicalizes a
  possibly-relative path against the cwd (collapsing `.`/`..`/`//`), and the
  file syscalls (open/stat/unlink) run it so relative paths honor the cwd.
  frame-libc gets real `chdir`/`getcwd`. `cmain` exercises it end to end:
  getcwd at "/", chdir absolute/relative/"..", a failing chdir, and a relative
  `fopen("readme")` that resolves to `/readme`. (Relative `exec` is left for a
  follow-up — the deferred exec/argv path uses absolute `/bin/...` today; an ish
  `cd` builtin is also a future nicety.)

- **`buildc <src>` from argv — DONE.** `buildc` no longer hardcodes `/hello.c`:
  its `_start` is now an argv shim (like argtest) that reads `argv[1]` as the
  source (default `/hello.c`), derives the object (`.c`→`.o`) and output
  (`.c`→`.elf`) paths, and the BuildDriver FSM's `actions::*` build/run those.
  console-test runs both the default (`/hello.c`→`/hello.elf`, exit 7) and an
  explicit `buildc /hi.c` (→`/hi.elf`, exit 3, `csrc/tcc_hi.c`) — a real
  `cc <file>`.

With this the B11-3 toolchain tech-debt list is cleared. The only remaining
follow-up is "grow the C-shim libc as on-device programs need more" — an
open-ended, demand-driven item, not a discrete task. Relative `exec` (deferred
exec/argv still uses absolute `/bin/...`) and an ish `cd` builtin are the small
nice-to-haves noted under cwd.

### C1–C5 (next): the V1.0 capstone — one Frame system → both C and Rust

Honest status check: today framec is only ever invoked with `-l rust` (28
`.frs` → Rust); the C/Rust hello-worlds that compile + run on-device are
*hand-written*, not framec output. So we have NOT yet literally satisfied the
north star — "run framec to compile a hello world in C and Rust and run them
from the shell." This track closes that gap by authoring **one** Frame
hello-world system and running it through framec to **both** backends:

```
                    frame/hello.frs   (one @@system, authored once)
                    /              \
        framec -l rust          framec -l c
              |                      |
      Rust user bin            generated hello.c   (staged at /fhello.c)
        /bin/fhello                  |
              |              on-device tcc + C-shim   (buildc /fhello.c)
          run it                     |
                                  run it
```

Steps (tasks C1–C5):
1. **C1 — spike (de-risk first).** Author `frame/hello.frs`; run host
   `framec compile -l c` and inspect the output: what libc surface/headers does
   it need (alloc, string, stdio, the FrameEvent/Compartment runtime), and does
   tcc 0.9.27 + the C-shim accept it? Produce a go/no-go + concrete gap list.
   The real risk lives here: the C backend emits a richer runtime than the
   minimal C-shim currently provides, and tcc 0.9.27 is strict.
2. **C2 — Rust half.** `hello.frs` → `framec -l rust` → `/bin/fhello`, run from
   the shell (the Rust user-bin path, like `cmain`).
3. **C3 — C half.** `hello.frs` → `framec -l c` → `/fhello.c`; `buildc /fhello.c`
   compiles that *framec-generated* C with the on-device tcc + C-shim and runs
   it. Grow the C-shim to cover the gaps C1 finds — fixing root causes, never
   stubbing behavior away.
4. **C4 — shell demo.** Make sure the shell drives the whole thing end to end
   (`fhello` for Rust, `buildc /fhello.c` for C), proven in console-test: one
   Frame source, both languages, both run from the shell.
5. **C5 — docs/diagram/journal.** `hello.frs` state diagram, README index,
   and the milestone write-up.

If C1 surfaces a gap too large to close cheaply (e.g. the C runtime wants libc
breadth tcc can't easily link), that is reported honestly rather than worked
around — the C-shim grows to meet a *real, working* generated program, not a
hollowed-out one.

### C1–C5 DONE (2026-05-24): the V1.0 north star is literally met

One Frame system (`frame/hello.frs`, `$Ready → $Greeted`) now runs in **both**
languages from the shell:
- **Frame → Rust:** `framec -l rust` (in user/build.rs) → `/bin/fhello`, which
  drives the generated FSM and prints `transpiled to Rust!`.
- **Frame → C:** `framec -l c` on the *same* source (xtask `build_fhello_c`) →
  `/fhello.c`; `buildc /fhello.c` compiles + links + runs it with the **on-device
  tcc**, printing `transpiled to C!` and exiting 0.

console-test asserts both. What it took:
- **C-shim grew** `realloc` (malloc now carries a 16-byte size header), `strdup`,
  and a new `<stdint.h>` (`intptr_t` etc.) — exactly what framec's C runtime
  (`FrameDict`/`FrameVec`) needs. Real implementations, no stubs.
- **`fs::create` is now multi-block.** It only ever used the root dir's first
  data block (a 16-entry cap), though `dir_lookup`/`unlink` already iterated all
  of `dir.direct`; the capstone's extra build artifacts pushed root past 16 and
  exposed it. Fixed to grow into new dir blocks on demand.
- **One-line tcc patch** (`tccelf.c:1082`, `|| s1->output_type ==
  TCC_OUTPUT_EXE`): the framec-generated C is the first on-device program to call
  its **own** non-`static` functions, and tcc 0.9.27's broken static-exe PLT
  crashed it (`#PF` at a garbage `jmp *GOT(%rip)`). The C-shim only covered libc
  (hidden) calls; this is the upstream/mob fix for the caller side. Surgical;
  tcc otherwise pristine. See `third_party/tcc/README.frame-os.md`. (Decision
  made with the user — the dependency was previously kept pristine; the capstone
  showed the on-device C toolchain was printf-only-capable without it.)

This is the genuine close of the V1.0 north star: **framec compiles a hello-world
to C and Rust and both run from the shell.** Remaining is the open-ended "grow
the C-shim as programs need more" + the small cwd/exec niceties.

Validation: `console-test` PASS (both `fhello` + `buildc /fhello.c`), `qemu-test`
48/49. The lone failure is `concurrent_exec_buffers`, a **pre-existing flake**
unrelated to this changeset: it asserts the *contiguous* substring `argv[1]=Z`,
but under concurrent exec the byte-at-a-time `write_char` of argtest's
`argv[1]=`/`Z` interleaves on the shared serial console with the parent's
`[wait] pid … reaped …` line — the argv *value* is correct, the bytes just
aren't adjacent. It reproduces ~1/4 **in isolation** (rate rises with host load),
and `fs::create` (the only kernel delta here) is never on its path. Follow-up:
make that test robust to console interleaving (or give the console a per-line
lock) — tracked separately, not a capstone regression.

**Flake fixed (2026-05-24).** Root cause: `argtest` printed one byte per
`write_char` syscall (#0), each preemptible, so a concurrent process's output
(the kernel's `[wait] … reaped …`) landed mid-line. Fix: the `write` syscall
(#12) now handles fd 1/2 — it emits the whole buffer to the console in one
syscall, which (ring-3 is single-core, syscalls run `IF=0`) prints with no
preemption point, i.e. an **atomic line**. `argtest` now builds each line in a
buffer and writes it with one `write(1, …)`. A real capability (a program can
write an atomic line), not a test-only patch; the test keeps its strong
`argv[1]=Z` assertion. Validated 8/8 isolated (was ~1/4) + full suite 49/49.

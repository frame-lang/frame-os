# `Shell`

> The hosted-mode Frame OS shell: prompts the user, reads input, tokenizes it via the `Parser` system, classifies into a `Builtin`, executes, and loops. At H1 (Step 2) the state graph is `$Prompting → $Parsing → $RunningBuiltin → $Prompting`, with `$Exiting` as a terminal sink reachable from `$Prompting` via the `exit`/`quit` keywords or `interrupt()`.

| Property | Value |
|---|---|
| Track | Hosted (will be reused in Bare-metal at B2) |
| Milestone introduced | H0 |
| Source file | [`../../frame/shell.frs`](../../frame/shell.frs) |
| State diagram | [`shell.svg`](shell.svg) |
| Instances at runtime | Exactly one per process |
| Status | In progress (H1 Step 2 — structure landed; Step 3 fills in builtin behavior) |

## State diagram

![Shell state graph](shell.svg)

Regenerate via `cargo xtask regen-diagrams` after any `.frs` change. The SVG is committed to the repo and `cargo xtask check-diagrams` enforces drift.

## States

### `$Prompting`

The shell is waiting for user input. The first prompt is printed by the state's `$>` enter handler at construction time. Subsequent prompts are printed on each re-entry from `$RunningBuiltin` (the cycle's natural completion point).

**Transitions out:**
- `line(input)` → `$Exiting` — when `input.trim()` is `"exit"` or `"quit"` (fast path; doesn't go through the parser)
- `line(input)` → `$Parsing` — for any other non-empty input; the line is stashed in `domain.current_line` for `$Parsing.$>` to read
- `interrupt()` → `$Exiting` — Ctrl-C or Ctrl-D from the host loop

**Events handled (no transition):**
- `line(input)` — stays in `$Prompting` for empty/whitespace-only input; re-prints the prompt
- `is_done()` → returns `false`

### `$Parsing`

Transient. Drives `Parser` synchronously to tokenize `domain.current_line`, classifies the resulting tokens into a `Builtin`, and transitions to either `$RunningBuiltin` (parse succeeded) or back to `$Prompting` (parse error — prints a message first).

**Transitions out:**
- `$>()` → `$RunningBuiltin` — when `Parser` reaches `$Done`; the classified `Builtin` is in `domain.current_builtin`
- `$>()` → `$Prompting` — when `Parser` reaches `$Failed` (e.g. unterminated quote); the parse-error message is printed

**Events handled (no transition):**
- `line(input)` — defensively declared, unreachable in practice (the `$>` handler runs synchronously and transitions out within the same `shell.line()` call)
- `interrupt()` → `$Prompting` — defensive; same reasoning
- `is_done()` → returns `false`

### `$RunningBuiltin`

Transient. Executes the classified `Builtin` (mutating `domain.cwd` if `cd`, reading `domain.history` if `history`, etc.), appends the input to history, and returns to `$Prompting`.

**Transitions out:**
- `$>()` → `$Prompting` — always, after `execute()` returns

**Events handled (no transition):**
- `line(input)` — defensively declared, unreachable
- `interrupt()` → `$Prompting` — defensive (H2 will add `$RunningExternal` where `interrupt()` actually matters)
- `is_done()` → returns `false`

### `$Exiting`

Terminal state. The shell's `$>` enter handler prints "goodbye". The host loop sees `is_done()` is `true` and stops.

**Transitions out:** none.

**Events handled (no transition):**
- `line(input)` — ignored
- `interrupt()` — ignored
- `is_done()` → returns `true`

## Interface

| Method | Parameters | Returns | Purpose |
|---|---|---|---|
| `line` | `input: &str` | `()` | Process one line of input from the user |
| `interrupt` | `()` | `()` | Process a Ctrl-C / Ctrl-D / SIGINT-equivalent signal from the host loop |
| `is_done` | `()` | `bool` | Query whether the shell is in `$Exiting` and the host loop should stop |

The interface is unchanged from H0. H1 added states and transitions but kept the same three public methods, so the host loop in `shell/src/main.rs` is unaffected by the H1 extension.

`line(input)` in `$Prompting` decides based on `input.trim()`: empty → stay (re-prompt), `"exit"`/`"quit"` → `$Exiting`, otherwise → `$Parsing` (with the line stashed in `domain.current_line`). In all other states `line` is defensively declared (transient states never receive line() in practice; $Exiting ignores it).

`interrupt()` semantics: H1 maps it to `$Exiting` from `$Prompting` (same as H0). H2 will revise — `$Prompting.interrupt()` will clear the line and stay, `$RunningExternal.interrupt()` will SIGKILL the child. The state-dependent dispatch is the Frame argument.

`is_done()` is the host loop's only state observation. Transient states (`$Parsing`, `$RunningBuiltin`) return `false`; `$Exiting` returns `true`.

## Domain

| Field | Type | Initial value | Purpose | Lifetime |
|---|---|---|---|---|
| `current_line` | `String` | `String::new()` | The line being processed, set on `$Prompting → $Parsing` so `$Parsing.$>` can read it | One-shot per cycle |
| `current_builtin` | `Builtin` | `Builtin::Empty` | The classified result from `$Parsing.$>`, consumed by `$RunningBuiltin.$>` | One-shot per cycle |
| `cwd` | `std::path::PathBuf` | `std::env::current_dir().unwrap_or_default()` | Shell's tracked working directory | System lifetime — updated by the `cd` builtin |
| `history` | `Vec<String>` | `Vec::new()` | Lines that resulted in a builtin execution | System lifetime — appended in `$RunningBuiltin.$>` |

## Why a state machine

Honest answer for H0: the Frame argument is weakest here.

The minimal H0 shell has two states and one input event. As plain Rust this would be a `Done` boolean flag and a function. Frame buys very little at this size.

So why use Frame? **Because the shell grows.** Looking at the H1, H2, H3 roadmap entries:

- H1 adds `$Parsing` and `$RunningBuiltin` states, plus 8 builtin commands, plus a separate `Parser` system that the shell composes with.
- H2 adds `$RunningExternal` with state-specific signal handling — Ctrl-C means different things in different states, which is the textbook case for state-driven dispatch.
- H3 adds `$Suspended` for Ctrl-Z, plus a `JobControl` manager system and per-job `Job` instances.

Each of those additions is a *localized change* in Frame: new state, new transitions, framepiler regenerates dispatch. In plain Rust each is a hunt-and-peck through every place the `Done` flag would have grown into a `ShellState` enum.

The H0 doc records the *start* of that progression. The Frame argument compounds with each subsequent milestone; H0 alone doesn't make the case.

What's lost by not using Frame at H0? Almost nothing in absolute terms, but conceptually: the established pattern. If we wrote H0 in plain Rust and "introduced Frame at H1", we'd have to refactor the H0 code, and the project's argument would be weaker because Frame wasn't there from the start.

## Composition

**Calls into:**
- `self.print_prompt()` — native action; uses `std::io::stdout` to print `"frame-os> "` and flush
- `self.print_goodbye()` — native action; prints `"goodbye"`
- `self.print_unknown(cmd)` — native action; prints `"unknown command: {cmd} (try 'exit')"`

**Called from:** the host loop in [`shell/src/main.rs`](../../shell/src/main.rs), which constructs the `Shell` once and calls `shell.line(input)` for each line read from stdin.

**Native modules used by actions:** `std::io::Write` (for flushing stdout). No other dependencies at H0.

## Testing

See [`../testing.md`](../testing.md) for the project-wide testing approach.

**State graph snapshot (Level 2):**
- Test file: [`../../shell/tests/state_graphs.rs`](../../shell/tests/state_graphs.rs)
- Snapshot file: `shell/tests/snapshots/state_graphs__shell_state_graph.snap` (auto-generated on first test run)
- Test name: `shell_state_graph_snapshot`
- Status: present; snapshot accepted after first run via `cargo insta review`

**Behavioral tests (Level 3):**
Test file: [`../../shell/tests/shell_behavior.rs`](../../shell/tests/shell_behavior.rs).

- `shell_starts_not_done` — fresh `Shell` is in `$Prompting`, `is_done()` is `false`
- `exit_command_transitions_to_exiting` — `line("exit")` → `is_done()` is `true`
- `quit_command_transitions_to_exiting` — `line("quit")` → `is_done()` is `true`
- `exit_with_trailing_newline_works` — `line("exit\n")` works (host loop sends trailing newline)
- `exit_with_surrounding_whitespace_works` — `line("  exit  ")` works
- `empty_line_does_not_exit` — `line("")` stays in `$Prompting`
- `whitespace_only_line_does_not_exit` — `line("   \t  ")` stays in `$Prompting`
- `unknown_command_does_not_exit` — `line("xyzzy")` stays in `$Prompting`
- `exiting_state_ignores_further_lines` — once in `$Exiting`, further `line()` calls don't change `is_done()`
- `interrupt_in_prompting_transitions_to_exiting` — `interrupt()` from `$Prompting` → `is_done()` is `true`
- `interrupt_in_exiting_is_idempotent` — `interrupt()` from `$Exiting` is a no-op (no panic, state unchanged)
- `interrupt_after_unknown_commands_still_exits` — after `line("foo"); line("bar"); interrupt()`, `is_done()` is `true`
- `many_unknown_commands_before_exit` — stress check that we can stay in `$Prompting` indefinitely

**Integration tests (Level 4):** Implicit at H1 Step 2 — every Shell behavioral test that calls `line("non-empty-non-exit-input")` exercises the Shell+Parser composition (the line goes through `$Parsing` which calls `Parser::__create`, `consume`, `finalize`, `tokens`). A dedicated integration test file is not necessary while the composition is straightforward.

**E2E tests (Level 6):**
Test file: [`../../shell/tests/e2e.rs`](../../shell/tests/e2e.rs).

- `prints_banner_on_startup` — binary prints the banner
- `prints_prompt` — binary prints the prompt
- `exit_command_exits_cleanly` — typing `exit` produces "goodbye" and exit code 0
- `quit_command_exits_cleanly` — typing `quit` produces "goodbye" and exit code 0
- `eof_exits_cleanly` — closing stdin produces "goodbye" and exit code 0
- `unknown_command_prints_message` — typing `xyzzy` produces "unknown command: xyzzy"
- `empty_lines_dont_crash` — repeated empty input doesn't produce unknown-command messages
- `multiple_commands_before_exit` — typing several unknown commands followed by `exit` works

**QEMU smoke tests (Level 7):** N/A — `Shell` at H0 runs only in the hosted track.

**Hardware tests (Level 8):** N/A — same reason.

## Native action implementations

The action bodies are inside the `actions:` block in [`../../frame/shell.frs`](../../frame/shell.frs). Each is a few lines of Rust:

- `print_prompt()` — `print!("frame-os> "); io::stdout().flush()`. The flush matters: without it, the prompt doesn't appear until after the user has typed a line on some terminals.
- `print_goodbye()` — `println!("goodbye")`. Newline is intentional so the next shell command on the user's terminal isn't glued to "goodbye".

At H1, `print_unknown` moved out of `actions:` and into `execute()` in [`../../shell/src/builtin.rs`](../../shell/src/builtin.rs) — unknown commands are now a `Builtin::Unknown` variant that flows through the normal `$Parsing → $RunningBuiltin → execute()` path. Output format is unchanged (`unknown command: {cmd} (try 'exit')`), so the H0 E2E tests still pass.

The actions are unsafe-free and `std`-only. They will need to be re-implemented for the bare-metal `Shell` at B2 (writing to `SerialDriver` instead of `stdout`); the Frame source itself is unchanged.

## Native dispatch layer (H1+)

H1 adds a `Builtin` enum and `classify` / `execute` functions in [`../../shell/src/builtin.rs`](../../shell/src/builtin.rs). The Shell state machine doesn't know what each builtin does — `$RunningBuiltin.$>` just calls `execute(&self.current_builtin, &mut self.cwd, &self.history)` and lets the native code do the data work. This split keeps the Frame system focused on lifecycle dispatch and leaves the per-builtin behavior to ordinary Rust, per the architecture doc's "30/70 Frame-to-Rust ratio" guideline.

The `Builtin` enum:
- `Cd(Option<String>)`, `Pwd`, `Ls(Option<String>)`, `Cat(Option<String>)`, `Echo(Vec<String>)`, `History`, `Help`
- `Empty` — no-op for empty token vectors
- `Unknown(String, Vec<String>)` — command not matched; carries the name for the error message and the args for H2's external-execution path

`classify(tokens: Vec<String>) -> Builtin` maps the parser's token output to a variant. `execute(builtin, &mut cwd, &history)` dispatches; at H1 Step 2 the per-variant bodies are `println!("(todo: <name>)")` placeholders. Step 3 fills them in one at a time, each with its own E2E test.

## Open questions

- **Should `line()` return something to the host?** Today the host calls `line()` and then queries `is_done()` separately. A future cleanup might fold these — `line()` returns a `bool` indicating whether to continue. Not necessary for H0; revisit if H1 surfaces a need.
- **Are `exit` and `quit` both genuinely useful, or is one redundant?** Two commands for the same action might be unnecessary surface. Pro: matches user expectation from both bash (`exit`) and Python REPL (`quit`). Con: more state-event pairs to test. Defer the decision; if it becomes annoying, drop `quit`.
- **Should there be a `help` builtin at H0?** Currently the banner mentions `exit`/`quit`. A `help` builtin would be 5 lines and would not require any state machine changes. Defer to H1, which is where builtins legitimately live; the H0 banner is the only help facility at this milestone.

## Related documents

- [Architecture](../architecture.md) — overall project structure, where Shell fits in the hosted track
- [Roadmap](../roadmap.md#h0--minimum-viable-shell) — H0 scope and success criteria
- [Testing](../testing.md) — project-wide testing approach this doc's Testing section follows
- [Systems index](README.md) — where to find docs for other Frame systems (none yet, at H0)

## Change log

- **2026-05-19** — initial doc; H0 implementation with `line` and `is_done`.
- **2026-05-19** — added `interrupt()` event for Ctrl-C / Ctrl-D handling, completing H0 scope per `docs/roadmap.md`. State graph adds `Prompting -> Exiting [label=interrupt]` edge. Three new behavioral tests cover the new state-event pairs.
- **2026-05-19** — H1 Step 2: added `$Parsing` and `$RunningBuiltin` transient states, four domain fields (`current_line`, `current_builtin`, `cwd`, `history`), and the native `Builtin` enum + `classify` / `execute` dispatch in `shell/src/builtin.rs`. State graph now has 9 edges. `print_unknown` moved from `actions:` into `execute()` so unknown commands flow through the standard `$Parsing → $RunningBuiltin` cycle. All H0 behavioral and E2E tests still pass unchanged. The 8 builtin variants currently have placeholder `(todo: <name>)` bodies; Step 3 fills in real behavior.

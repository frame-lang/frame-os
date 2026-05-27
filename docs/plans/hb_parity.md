# H↔B shell parity — full architectural parity (one Frame source, both targets)

**Status: PLANNED (2026-05-27).** Goal: the **same** `Shell` + `JobControl` +
`Parser` Frame FSMs drive a full shell on **both** the hosted target (Linux/
macOS/Windows app — H-track) **and** bare-metal ring 3 (replacing the
hand-written `ish`), at feature parity with the S1–S10 bare-metal shell. This is
the strongest form of the Frame OS thesis: *one FSM source, radically different
targets* — currently proven only for `Parser`.

## Current state (gap map, 2026-05-27)

| Concern | Hosted (H-track) | Bare-metal (`ish`) |
|---|---|---|
| Tokenizing | `Parser` FSM | **same `Parser` FSM** ✅ (proven, B4-6 first half) |
| Shell control flow | `Shell` FSM (`$Prompting/$Parsing/$RunningBuiltin/$RunningForeground/$Exiting`) | **hand-written flat dispatch** (NOT the Shell FSM) |
| Job control | `JobControl` + `Job` FSMs, spawn via `std::process` | `IshJobs` FSM (flat job table), fork/exec/wait via **syscalls** |
| Builtins | `Builtin` enum, `std::fs`/`std::env` | hand-written, syscalls |
| Pipes `\|` / redirection `<` `>` `>>` | **`Pipeline` FSM** (M1 ✅) — Parser tags operators, Pipeline parses the grammar; native `exec.rs` runs it | implemented natively in `ish` (`parse_redirs`, `run_pipeline` + `dup2`) — migrates onto the shared `Pipeline` FSM at M3/M4 |

**The crux.** The execution *mechanism* differs (sibling `std::process` spawn vs
hierarchical syscall fork/exec/wait), but the *coordination* (parse → classify →
run builtin / run foreground / background / fg / bg / stop / reap) is identical.
That is exactly the project's established **FSM-owns-logic / native-owns-mechanism**
split — same shape as `virtio_blk`'s backend seam and the RAM-disk backend. So
parity = put the spawn/wait/signal mechanism behind a **process backend** seam and
let one set of FSMs coordinate both.

Two genuine modeling gaps beyond the seam:
1. **Pipes + redirection** aren't in any FSM (only `ish`'s native code). They must
   be modeled once (Parser tags the operators; the Shell/JobControl side carries a
   parsed pipeline/redirection structure) so both targets share them.
2. **JobControl vs IshJobs** are two FSMs for the same job. Parity unifies them
   (one `JobControl`/`Job` with a pluggable spawn backend), or keeps `JobControl`
   and makes `ish` drive it via the syscall backend.

## Milestones (each shippable + validated)

Sequenced lowest-risk-first; every milestone leaves both builds green.

- **M1 — Hosted feature parity (H1→H3, FSM-driven). ✅ DONE 2026-05-27.** The
  hosted shell was already at H0–H3 (builtins, external foreground, job control:
  `&`/`fg`/`bg`/`jobs`/`kill`/Ctrl-Z) — confirmed green (159 tests) at M1 start.
  The one gap was **pipes + redirection**, now added FSM-first:
  - **`Parser` tags operators** (`parser.frs`): unquoted `|` `<` `>` `>>` `&`
    become typed `Token`s; quoted `"|"` stays a `Word` (operator-vs-word is a
    scanner-mode decision, so it belongs in the Parser). New `typed_tokens()`
    query; legacy `tokens(): Vec<String>` reconstructs literals so `ish` is
    byte-identical (migrates at M3/M4). +7 Parser tests.
  - **New `Pipeline` FSM** (`pipeline.frs`): folds the token stream into a
    `Vec<Command>` (stages + `< > >>` redirs) + background flag, owning the
    grammar with `$ReadingCommand`/`$ExpectingTarget`/`$TrailingAmp`/`$Done`/
    `$Error` states. 16 behavioral tests + state-graph snapshot + diagram + doc.
    (Decision recorded under Risks: option (a)+(b) — Parser emits typed tokens
    **and** a dedicated Pipeline FSM parses them, per the user's call.)
  - **Execution mechanism** stays native (`shell/src/exec.rs`, std::process):
    `Shell` gains `$RunningPipeline` (foreground external pipelines via OS
    pipes); single-command `< > >>` runs inline; builtin `> f` uses a Unix
    fd-redirect guard so `echo hi > f` writes the file (user-visible parity with
    `ish`, where echo is `/bin/echo`). 13 new E2E tests.
  - Validated: full host suite green, clippy (host + kernel both configs + user
    crate) clean, fmt clean, check-diagrams clean. `ish` unchanged + still
    builds bare-metal.

  **The cross-cutting modeling decision is now settled** (see Risks): Parser
  emits typed tokens + a dedicated `Pipeline` FSM consumes them.

- **M2 — Process-backend seam. ✅ DONE 2026-05-27.** `Job`'s spawn/poll/signal
  mechanism moved behind the `ProcessBackend` trait (`shell/src/process_backend.rs`);
  `Job` holds a `Box<dyn ProcessBackend>` from a per-crate `default_backend()` and
  no longer mentions `std::process`/`libc`. Hosted impl = `StdProcessBackend`
  (`std::process` + `libc::kill`). The `Shell`/`JobControl`/`Job` FSMs are
  unchanged — **the `Job` state-graph snapshot is byte-stable** (pure mechanism
  refactor). This prepares (does not wire) the ring-3 syscall backend. Validated:
  all H3 behavioral + E2E green, clippy/fmt/diagrams clean, kernel + user crates
  unaffected.

- **M3 — Ring-3 FSM reuse.** Staged (chosen 2026-05-27, lowest-risk-first):
  - **M3a — `Pipeline` FSM into `ish`. ✅ DONE 2026-05-27.** `ish` now drives its
    parsing through the shared `Parser` → `Pipeline` FSMs (the *same*
    `frame/pipeline.frs` the hosted shell compiles), retiring its hand-written
    `parse_redirs` + manual `|`-split. The Pipeline FSM is proven on **bare
    metal**: `console-test` green end-to-end, exercising FSM-parsed redirection
    (`echo > / >> / wc <`) and pipes (`echo … | wc`) plus the full S1–S10 suite.
    `ish` still owns execution (fork/exec/dup2/pipe via syscalls). Headline so
    far: `Parser` **and** `Pipeline` are now one source running on Linux *and*
    bare metal. (`ish` net −37 lines.)
  - **M3b — Ring-3 `Shell` control-flow FSM reuse.** Chosen shape (2026-05-27):
    *fat FSM, thin env* — keep all 6 Shell states + the full $Parsing routing in
    the FSM; abstract every target-specific operation behind a `ShellEnv` trait.
    - **M3b.1 — `ShellEnv` seam + hosted refactor. ✅ DONE 2026-05-27.** Added
      `CommandKind` + the `ShellEnv` trait to the shell.frs prolog; the `Shell`
      FSM now routes prompt/goodbye/tick/classify/run_builtin/run_foreground/
      spawn_background/spawn_foreground/fg/run_pipeline/wait_foreground/println
      through `self.env: Box<dyn ShellEnv>` and no longer mentions std, the
      `Builtin` enum, `JobControl`, or a `PathBuf` cwd. Hosted `StdShellEnv`
      (shell/src/shell_env.rs) wraps the existing classify/execute/exec +
      JobControl + cwd, preserving behavior exactly — **state graph byte-stable,
      all 200+ hosted tests green**, clippy/fmt/diagrams clean. The FSM is now
      environment-agnostic and ready to compile for ring 3.
    - **M3b.2 — ring-3 `IshShellEnv` + compile shell.frs for `x86_64-unknown-none`
      (next).** Implement `ShellEnv` with ish's native syscalls (print/classify/
      run_builtin/run_external/run_pipeline/fork/exec/wait + IshJobs).
    - **M3b.3 — drive ish's loop through the `Shell` FSM; `console-test` green,
      now Shell-FSM-driven.** Keep ish's hand-written dispatch until the FSM path
      passes.

- **M4 — Consolidate + retire `ish`'s hand-written dispatch.** Switch `ish` fully to
  the shared `Shell` + `JobControl` FSMs over the syscall backend; retire the
  bespoke flat dispatch and (if unified) `IshJobs`. **Closes B4-6.** Validate: both
  targets at S1–S10 parity, driven by the *same* FSMs. Update diagrams + docs +
  `frame_assessment.md` — the headline "this exact FSM runs a shell on Linux and
  bare metal" result.

## Risks / open design decisions
- **Pipes/redirection modeling (M1). ✅ DECIDED 2026-05-27.** `Parser` emits typed
  operator tokens **and** a dedicated `Pipeline` FSM parses them into a
  `Vec<Command>` (option (a) + a second FSM, the user's call — "maximal Frame").
  The grouping is a genuine token-driven FSM (`$ReadingCommand`/`$ExpectingTarget`/
  `$TrailingAmp`), not a native fold. Execution stays native (`exec.rs`). Note: the
  `Pipeline` event is `consume(kind: TokenKind, text: String)` rather than
  `consume(t: Token)` because framec moves a non-Copy enum event-param out of a
  shared ref (won't compile) — a Copy tag + a cloned String is the supported
  shape (documented in `pipeline.frs`).
- **Execution model asymmetry.** Hosted = sibling processes (`std::process`);
  ring-3 = hierarchical fork→exec→wait. The backend seam must express both behind
  one interface the FSM drives — the hard part of M2/M3.
- **Don't regress `ish`.** It's a working S1–S10 shell; M3 keeps it until the
  FSM path is proven, then M4 swaps. No flag-day rewrite.
- **Scope.** This is a multi-milestone program; each milestone is independently
  shippable and validated. Not all-or-nothing.

## Why it's worth it
The disk path now shows two Frame systems coordinating a concurrent critical path
(`IoScheduler` + `BlockRequest`). This program does the same for the *shell*: one
`Shell` + `JobControl` source coordinating a full interactive shell on a hosted OS
*and* bare metal — the clearest statement of "write the state machine once, run it
anywhere" the project can make.

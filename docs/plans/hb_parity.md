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
| Pipes `\|` / redirection `<` `>` `>>` | **not modeled** anywhere (Parser treats as words) | implemented natively in `ish` (`parse_redirs`, `run_pipeline` + `dup2`) |

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

- **M1 — Hosted feature parity (H1→H3, FSM-driven).** Bring the hosted shell up to
  the bare-metal feature set *on the hosted side first* (lower risk: `std` + the
  existing e2e tests + CI matrix). Confirm/complete builtins (H1), external
  foreground (H2), job control (H3: `&`/`fg`/`bg`/`jobs`/`kill`/Ctrl-Z), then add
  **pipes + redirection** to the hosted `Shell` (the first cross-cutting modeling
  decision — see Risks). Validate: `shell/tests/e2e.rs` + behavioral + CI matrix.

- **M2 — Process-backend seam.** Abstract `Job`'s spawn/poll/signal (`std::process`)
  behind a native backend interface, the `Shell`/`JobControl`/`Job` FSMs unchanged.
  Hosted backend = `std::process`; this prepares (not yet wires) the ring-3
  backend. Validate: hosted behavior unchanged (e2e + behavioral green).

- **M3 — Ring-3 `Shell` FSM reuse.** Compile `shell.frs` for `x86_64-unknown-none`
  (Parser already does), drive `ish`'s loop through the `Shell` FSM, and implement
  its actions + the M2 backend with **syscalls** (fork/exec/waitpid/dup2/pipe/kill).
  Keep `ish`'s hand-written path until the FSM path passes. Validate: `console-test`
  (full S1–S10) green, now FSM-driven.

- **M4 — Consolidate + retire `ish`'s hand-written dispatch.** Switch `ish` fully to
  the shared `Shell` + `JobControl` FSMs over the syscall backend; retire the
  bespoke flat dispatch and (if unified) `IshJobs`. **Closes B4-6.** Validate: both
  targets at S1–S10 parity, driven by the *same* FSMs. Update diagrams + docs +
  `frame_assessment.md` — the headline "this exact FSM runs a shell on Linux and
  bare metal" result.

## Risks / open design decisions
- **Pipes/redirection modeling (M1).** Options: (a) `Parser` emits typed operator
  tokens + a `Shell`-side pipeline/redirection struct (cleanest, most Frame); (b)
  keep parsing native, driven by the FSM (less FSM, faster). Decide at M1 start;
  prefer (a) for the thesis, but bound the scope.
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

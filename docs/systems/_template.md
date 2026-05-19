# `<SystemName>`

<!--
  PER-SYSTEM DOCUMENTATION TEMPLATE — Frame OS

  Copy this file when writing a per-system doc. Rename to <system_name>.md
  (lowercase, underscores). Replace each section with content; delete the
  HTML comments before committing.

  Sections marked "REQUIRED" must be present in every per-system doc.
  Sections marked "OPTIONAL" are encouraged but skippable for trivial systems.

  Required tone: matter-of-fact, technical, no marketing language. The doc
  is a reference; it should make the system's behavior unambiguous, not
  argue for it. The "Why a state machine" section is the one place to make
  the Frame argument — keep it brief and grounded.

  Required level of detail: a reader who has read the architecture doc and
  knows Frame should be able to use this doc as the authoritative source
  for the system's interface, states, and transitions. No "TBD" or "see
  source" — the source is also linked, but the doc should stand alone.
-->

> **One-line summary** *(REQUIRED)* — One sentence describing what this system does and where it lives. Example: "Top-level kernel lifecycle for boot, run, and shutdown, running once per kernel image in the bare-metal track."

| Property | Value |
|---|---|
| Track | Hosted / Bare-metal / Shared |
| Milestone introduced | (e.g. B1) |
| Source file | [`../../frame/<file>.frs`](../../frame/<file>.frs) |
| State diagram | [`<file>.svg`](<file>.svg) |
| Instances at runtime | (e.g. "exactly one" / "one per task, up to MAX_TASKS" / "one per UART") |
| Status | Planned / In progress / Documented |

## State diagram

*REQUIRED.* Embed the generated SVG. GitHub renders SVGs inline in markdown:

```markdown
![<SystemName> state graph](<file>.svg)
```

If the diagram hasn't been generated yet (system is in "Planned" or early "In progress"), include a placeholder ASCII diagram showing the state structure. Replace with the real SVG once `framec -l graphviz` produces one.

## States

*REQUIRED.* One subsection per state, in the order they appear in the source. For each state:

### `$StateName`

Brief description of what this state represents. What is the system doing while it's here? What's the user-observable consequence?

**Transitions out:**
- `event_name()` → `$TargetState` — under what condition
- `$>` (enter) — what happens on entry (if anything beyond default)
- `<$` (exit) — what happens on exit (if anything beyond default)

**Events handled (no transition):**
- `event_name()` — what the handler does, what state-local variables it updates

**Events ignored:** *(REQUIRED if the system uses state-as-gate; skip otherwise)*
- `event_name()` — explicitly noted as ignored, with a one-sentence justification. (E.g., "Ignoring `kill()` in `$Zombie` is intentional — the process is already terminated.")

**Forwarding:** *(REQUIRED if HSM)*
- `=> $^` placement and rationale. E.g., "Trailing `=> $^` so `panic()` events forward to `$Booting`'s handler. No other events forward — `$InitMemory`'s handlers fully own their dispatch."

**State variables (`$.`):** *(OPTIONAL — include if any)*
- `$.var_name: type = init` — what this stores, what it survives (state activity only), when it's read and written.

## Interface

*REQUIRED.* Public methods callers invoke. One table:

| Method | Parameters | Returns | Purpose |
|---|---|---|---|
| `method_name` | `arg: type, ...` | `: type` or `(none)` | What the call does at the system level |

For each method, follow with a paragraph that describes:
- Which states accept it (and what each does with it)
- Which states ignore it
- Any preconditions the caller is responsible for (e.g., "must not be called from inside a handler", "blob must be non-empty")

## Domain

*REQUIRED if the `domain:` block is non-empty; skip otherwise.* Persistent data the system owns. One table:

| Field | Type | Initial value | Purpose | Lifetime |
|---|---|---|---|---|
| `field_name` | `type` | `init_expr` | What it stores | "System lifetime" / "Reset on every transition into <state>" |

## Why a state machine

*REQUIRED.* This is the section that makes Frame's case for this system specifically. Keep it brief — two to four paragraphs.

Answer three questions:

1. **What would this look like as plain Rust?** Sketch the equivalent code structure without Frame. Usually an enum + match + scattered `if state == X` checks across multiple functions.
2. **What does Frame buy?** Be specific. Examples that count: exhaustiveness checking forces every dispatch site to handle new states; HSM parent state catches errors from many children with one handler; state-dependent dispatch makes "this event means different things in different states" structural rather than conditional; state graph is renderable directly from the implementation.
3. **What would be lost by not using Frame here?** Honest answer. Sometimes "very little" — note when this is the case (it's evidence for using state machines selectively, which is its own Frame argument). Most of the time it's some specific bug class that would re-enter the code, or some specific documentation property that would degrade.

If the answer to (2) is weak — "the state machine is small, the events are few, the dispatch is trivial" — that's a signal the system is the wrong granularity and the section should say so. A doc that admits "this system is borderline" is more useful than one that overclaims.

## Composition

*REQUIRED.* How this system fits into the larger Frame OS.

**Calls into:** what other Frame systems or native modules this system invokes.
- `OtherSystem.method(...)` — when and why
- `native::module::function()` — when and why

**Called from:** what invokes this system's interface methods.
- The kernel's boot stub calls `kernel.boot()` at startup
- The timer interrupt handler calls `scheduler.tick()` on each tick
- Other systems hold a reference and call methods directly

**Native modules used by actions:**
- `crate::module::function` — what this action does

## Testing

*REQUIRED.* What's covered, what isn't, where the tests live. See [`../testing.md`](../testing.md) for the project-wide testing approach.

**State graph snapshot (Level 2):**
- Test file: `tests/state_graphs.rs`
- Snapshot file: `tests/snapshots/<crate>__state_graphs__<system_name>.snap`
- Status: present / not yet written

**Behavioral tests (Level 3):**
List which state-event pairs have explicit tests. For each, briefly note what the test asserts.

- `boot()` in `$Booting` → `$InitMemory` (asserts: state name after call)
- `kernel_panic()` in any boot child → `$Halted` (asserts: state name, halt flag set)
- *(list each meaningful state-event pair)*

State-event pairs that are deliberately *not* tested in isolation should be noted with a one-sentence reason. E.g., "$Halted's event ignores aren't tested individually — Frame's dispatch guarantees them by construction."

**Integration tests (Level 4):**
Which other systems is this one exercised against, in which test files.

- `kernel/tests/scheduler_runs_tasks.rs` — exercises `Kernel.tick()` calling `Scheduler.tick()` with multiple tasks
- *(list each relevant integration test)*

**QEMU smoke tests (Level 7):**
If this system has bare-metal-specific behavior, which QEMU tests cover it.

- `kernel/tests/qemu_smoke.rs::boot_prints_banner` — exercises `Kernel`'s init phases through to `$Running`
- *(list each relevant QEMU test, or "not applicable" for hosted-only systems)*

**Hardware tests (Level 8):**
If this system has hardware-specific behavior, which `#[ignore]`-gated tests cover it.

- `kernel/tests/hardware.rs::pico_serial_loopback` — exercises `SerialDriver` on real Pico hardware
- *(list each relevant hardware test, or "not applicable")*

## Native action implementations

*REQUIRED if the actions are non-trivial; can be brief otherwise.* The Frame source declares actions but the actions themselves are native Rust. Document anything important about the action layer:

- Where the action bodies live (typically in the same `.frs` file's `actions:` block, but may delegate to a separate Rust module for complex work)
- Track-specific differences for shared systems (hosted vs. bare-metal)
- Any `unsafe` blocks and their justification
- Any locks, atomics, or other concurrency primitives

## Persistence

*OPTIONAL — include only if the system is `@@[persist(...)]`.* Frame OS kernel systems generally are not persisted; the hosted shell's `history` might be persisted to disk in a future milestone. If this system is persisted:

- The blob type (`String`, `Vec<u8>`, etc.)
- The save and load method names (`@@[save(<name>)]` and `@@[load(<name>)]`)
- Any `@@[no_persist]` domain fields and why they're excluded
- The serialization library used (serde, rmp_serde, custom, etc.)
- Where the blob is stored (file path, embedded resource, network)

## Open questions

*OPTIONAL — include if there are unresolved design points.* Anything the implementation hasn't pinned down. Examples:

- Whether `$Stopped` should be a separate state or a sub-state of `$Blocked`
- Whether the system should be `@@[persist(...)]` for crash recovery
- Whether timer calibration is worth its own state or should collapse to inline code

Open questions are not commitments; they're notes for the next person (or future you) to resolve. Each should be specific enough to be answerable.

## Related documents

*REQUIRED.* Links to other docs that touch this system.

- [Architecture](../architecture.md) — overall project structure
- [Roadmap](../roadmap.md) — which milestone introduced this system
- [Related system 1](other_system.md) — how this system composes with it
- [Related system 2](another_system.md) — comparison or contrast

## Change log

*OPTIONAL.* Brief log of major design changes, when the doc accumulates them. Format:

- **YYYY-MM-DD** — added `$Stopped` state to support SIGSTOP/SIGCONT
- **YYYY-MM-DD** — refactored to merge `$Validating` and `$Executing` (was: two states with no distinct behavior)
- **YYYY-MM-DD** — initial doc, system implemented at milestone X

Skip until the doc accumulates real changes worth recording.

---

<!--
  GUIDANCE NOTES (delete before committing real per-system docs)

  Length target: 200-500 lines of markdown per system. Shorter for trivial
  systems (3-state drivers); longer for complex ones (Scheduler, Process).
  A doc much shorter than 200 lines probably under-documents; much longer
  than 500 probably over-explains and should defer detail to the source.

  Diagrams: the generated .svg is the source of truth for "which states
  exist and how they connect." The doc's text reinforces and explains;
  it does not duplicate the graph in ASCII art (which would drift). One
  exception: include an ASCII placeholder before the .svg exists, then
  delete the placeholder once the SVG is committed.

  Naming: states use `$Pascal` per Frame convention. Events use
  `snake_case()` with parens. Domain fields use `snake_case`. Method
  signatures match the Frame source verbatim including the colon-type
  notation: `method(arg: type): return_type`.

  Linking to source: use relative paths (`../../frame/scheduler.frs`)
  not absolute. The repo's structure is portable; absolute paths break
  when someone fetches a subtree.

  Tone: technical reference. Avoid marketing words ("powerful", "elegant",
  "robust"). Avoid hedge words ("usually", "typically", "should") unless
  you mean them — if a state always responds to an event, say "always",
  not "typically".

  Audience: someone who has read frame_language.md and Frame OS's
  architecture.md, but has never seen this specific system before. They
  should be able to read the doc once and have a working mental model.

  When two systems are very similar (e.g. Task at B1 and Process at B4),
  prefer one doc that covers the evolution rather than two duplicating
  most of their content. Use clear section markers ("At B1:" / "At B4:")
  to distinguish behavior at different milestones.
-->

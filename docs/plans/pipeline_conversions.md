# Plan — convert the data pipelines to enter-param FSMs

> **Status:** In progress. `SyscallDispatcher` (`aca1b78`) and `ElfLoader` (`7f7e1e8`) done. `PageFaultHandler` reviewed and **deliberately left as-is** (not a data pipeline — see below). Remaining: the RX packet pipeline (B5). Validating against the new local framec only (the published 4.2.0 is set aside).

## Goal
Where a Frame system is a *data pipeline* — a datum flows down a sequence/fan-out of states — carry that data **on the transitions, as enter parameters** (`-> (data) $Next` → `$>(data)`), instead of stashing it in domain fields or native globals. This maximizes Frame's expressiveness (the data flow is visible in the state graph) and exercises framec's typed enter-param codegen. It also produces the cookbook "data pipeline" recipes.

## The framec mechanism + the CI-safety rule (read first)

framec has **two** enter-arg codegens, and which one CI uses matters:

- **Published (crates.io 4.2.0):** enter args are `Vec<String>` — each arg is `ToString`'d on the transition and `parse::<T>()`'d in the handler. Works **only** for `T: Display + FromStr` (integers, `bool`, `char`, `String`). A struct/array has no `Display`/`FromStr` → **won't compile**.
- **New local build (RFC-0025.1, not yet published):** enter args become a **typed per-state context struct** (`struct $StateContext { x: T }`, `derive(Clone)` + a `Default` impl). Works for any `T: Clone + Default` — including `[u8; N]`, `Vec<u8>`, and owned descriptor structs. No stringification.

**CI installs framec from crates.io.** So:

| Threaded type | Compiles on crates.io? | Commit-safe now? |
|---|---|---|
| Scalars (`u64`/`u16`/`bool`/`char`/`String`) | Yes (round-trips through String) | **Yes** |
| Byte arrays / `Vec` / descriptor structs | No (needs the new build) | **No — hold until the new framec is published** |

Two more gotchas, both already verified harmless:
- The graphviz snapshot differs between framec builds by a **trailing blank line** only; `insta` normalizes it, so snapshot tests stay green on either build. `check-diagrams` (DOT→SVG via `dot`) is unaffected (dot ignores trailing whitespace).
- Enter-param types under the new build need `Default` (the context struct derives it) — a descriptor struct must `#[derive(Clone, Default)]`.

## The pipelines (and the non-pipelines)

### 1. `SyscallDispatcher` — DONE (`aca1b78`)
`$Validating → $Executing` threading `(num, a0, a1)`. Scalars → CI-safe on the published framec. The minimal recipe; zero behavior change.

### 2. `PageFaultHandler` — **reviewed; deliberately NOT converted** (decided 2026-05-21)
A classifier whose fault data is genuinely *shared state*, not forward-flowing pipeline data:
- `fault_addr` is **logged in `$Fatal`/`$Killing`** (reached via the `unrecoverable()` funnel) **and queried post-settle** by `fault_addr()`, which two behavioral tests assert (even though the kernel doesn't call it).
- `unrecoverable()` is an argless "give up" signal that **fans in** from both `$Classifying` and `$LazyFault`.

A full enter-param conversion would either drop the `fault_addr()` query (losing a public method + test coverage) or duplicate the address into a domain latch — both *worse* than capturing the fault once in the domain and letting the disposition states read it. This is the architecture doc's "the invariant is shared state → domain field" case. **Decision: leave as-is.** PFH is a classifier, not a data pipeline; domain is the right tool.

### 3. `ElfLoader` — phase pipeline — **DONE** (`7f7e1e8`)
Threads an `ElfHeader` descriptor (`phoff/phentsize/phnum`) `$ReadingHeader → $ValidatingHeader → $MappingSegments` via enter params; the ELF bytes / pml4 / mapped-pages rollback list stay native, and `entry` stays in the global for the `entry_va()` query. The "thread the parsed descriptor, keep the payload native" recipe. Requires the new typed-context framec (struct enter param). Validated: 6/6 behavioral, snapshot unchanged, 24/24 QEMU.

*Original plan (for reference):* introduce `ElfHeader`, return it from `read_header() -> Option<ElfHeader>`, thread it; keep the payload native.
*Current:* every phase calls `crate::elf::*` which reads/writes a single native `ELF` global; the Frame system threads nothing.
*Plan:* introduce `crate::elf::ElfHeader { entry, phoff, phentsize, phnum }` (`#[derive(Clone, Copy, Default)]`). `read_header() -> Option<ElfHeader>`; thread it `$ReadingHeader → $ValidatingHeader → $MappingSegments` as an enter param (`$>(hdr: crate::elf::ElfHeader)`); `validate_header(hdr)` / `map_segments(hdr)` take it by value. `$BuildingStack` needs no header.
*Keep native:* the ELF **bytes**, target **pml4**, and the **mapped-pages rollback list** stay in the `elf` module (genuinely shared, accumulating native state — the "payload"); `entry_va()`/`stack_top()` interface queries still read it post-load. So the recipe is exactly *"thread the parsed descriptor, keep the payload native."*
*CI:* a struct enter param does **not** compile on crates.io framec → **hold until the new framec is published.** (Alternative to land sooner: thread the four fields as separate scalars — CI-safe but clunky; not recommended.)
*Touches:* `frame/elf_loader.frs`, `kernel/src/elf.rs`, the `elf` host double in `kernel-tests/src/lib.rs` (must mirror `ElfHeader` + the new signatures).
*Validation:* `elf_loader_behavior` (6) + snapshot + QEMU `ring3_syscall_b3`, `exec_b3`, both userspace-shell tests.

### 4. RX packet pipeline — NEW (B5 Steps 3–4); **needs the new framec** (bytes/descriptor)
The genuine showcase: `$Classifying → $Arp | $Ipv4 → $Icmp | $Udp | $Tcp → $Respond`, threading a parsed packet descriptor (`#[derive(Clone, Default)]` of offsets/proto/lengths/ports) — and optionally the `[u8; N]` frame — via enter params, the bytes otherwise in a native buffer. Built as part of B5 Step 3 (UDP) / Step 4 (TCP). Hold until the new framec is published.

### Not pipelines (leave as-is)
- **`Parser`** — a *streaming* scanner: data arrives per `consume(c)` **event** and accumulates in domain (`current`/`tokens`). That's the push/accumulate recipe, **not** enter-param threading; converting it would be miscategorizing it. Document as a separate cookbook recipe; don't change it.
- **`Kernel`** boot chain — pure sequencing, no datum flows. A `@@system Kernel($(boot_config))` state-param could carry config *if* one existed; there's none, so no change.
- Lifecycle/manager/mode machines (`Process`, `ProcessTable`, `BlockRequest`, `Mount`, `OpenFile`, `ArpResolver`, `Scheduler`, `SerialDriver`) — state encodes "where in a lifecycle," not "what stage of processing this datum is at." Not pipelines.

## Sequencing
- **Done:** `SyscallDispatcher` (`aca1b78`), `ElfLoader` (`7f7e1e8`).
- **Decided not to convert:** `PageFaultHandler` (classifier with shared/queried state).
- **Remaining:** the RX packet pipeline — built as part of B5 Steps 3 (UDP) / 4 (TCP), the genuine showcase of threading a parsed packet descriptor (and/or `[u8; N]`) via enter params.

Validation target is the new local framec build; the published 4.2.0 is set aside (it will catch up when the new framec is released — `ElfLoader`'s struct enter param and the RX pipeline require it; `SyscallDispatcher`'s `u64` args happen to work on both).

## Per-conversion checklist
- [ ] Update the `.frs`; thread the data via enter params; trim the domain/native field it replaces (only where not also needed post-pipeline or by a fan-in funnel).
- [ ] Update the native side + any `kernel-tests` host double to the new signatures.
- [ ] `cargo build` kernel + `clippy -D warnings`; host behavioral suite; state-graph snapshot (expect unchanged — insta normalizes).
- [ ] `cargo xtask qemu-test` (the system's smoke tests) + `check-diagrams`.
- [ ] Confirm CI-safety per the table above before committing; otherwise hold.

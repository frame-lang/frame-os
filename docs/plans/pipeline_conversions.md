# Plan — convert the data pipelines to enter-param FSMs

> **Status:** In progress. `SyscallDispatcher` done (`aca1b78`). The rest planned here. No rush — each conversion is a careful refactor of a load-bearing system, validated and committed one at a time.

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

### 2. `PageFaultHandler` — classifier; **CI-safe now** (scalars)
*Current:* `$Classifying.fault(addr, error_code)` stashes `fault_addr` + `is_user` in the **domain**; `$LazyFault`/`$Fatal`/`$Killing` read them for logging, and `fault_addr()` is an interface query *after* disposition.
*Plan:* thread the fault descriptor forward as **scalar enter params** — `addr: u64` (and the derived `is_user: bool`) — from `$Classifying` into `$LazyFault` and (via the `unrecoverable()` funnel) the disposition sinks, so each state receives the fault it's acting on rather than reading a shared field.
*Keep in domain:* `fault_addr` must stay (the `fault_addr()` interface query is read by the native #PF handler *after* the machine settles, and the funnel `unrecoverable()` is a self-sent event that carries no args). So this is a **partial** thread: `$Classifying → $LazyFault` carries `addr`; the disposition decision still reads `self.fault_addr`/`self.is_user` because `unrecoverable()` fans in from multiple children.
*Honesty note:* because the disposition is reached via a no-arg funnel event (`unrecoverable()`), and the address is queried post-settle, the domain field is partly load-bearing here. The clean win is `$Classifying → $LazyFault` carrying `addr`; forcing the rest through enter params would fight the funnel design. **Recommend: thread `addr` into `$LazyFault` only; leave the funnel + query on the domain.** Small, honest, CI-safe.
*Validation:* PFH behavioral suite (9 tests) + `page_fault_handler_state_graph_snapshot` + QEMU `page_fault_demand_b2`, `page_fault_fatal_b2`, `user_fault_does_not_crash_kernel_b3`.

### 3. `ElfLoader` — phase pipeline; **needs the new framec** (descriptor struct)
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
1. **Now (published-framec-safe):** `PageFaultHandler` (thread `addr` into `$LazyFault`; scalars). Commit independently, full revalidation.
2. **After the new framec ships to crates.io:** `ElfLoader` (header descriptor) and the RX packet pipeline (B5 3–4). Build locally + hold until then, or land once CI's framec has typed contexts.

## Per-conversion checklist
- [ ] Update the `.frs`; thread the data via enter params; trim the domain/native field it replaces (only where not also needed post-pipeline or by a fan-in funnel).
- [ ] Update the native side + any `kernel-tests` host double to the new signatures.
- [ ] `cargo build` kernel + `clippy -D warnings`; host behavioral suite; state-graph snapshot (expect unchanged — insta normalizes).
- [ ] `cargo xtask qemu-test` (the system's smoke tests) + `check-diagrams`.
- [ ] Confirm CI-safety per the table above before committing; otherwise hold.

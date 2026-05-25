# S6 (pipes) + IoScheduler I/O-sequencing — handoff / problem statement

**Status: RESOLVED, all work UNCOMMITTED (pending one clean validation run + your
commit approval).** Written 2026-05-25. Originally a handoff for an open bug; the
bug is now **root-caused and fixed** — see "RESOLVED" below. Kept as the record of
the whole thread.

## UPDATE (resolved): the S5 flakiness + tcc regression were ONE bug

Both were the **status-byte early-read race**: virtio only guarantees a request's
buffers (data + status) are all written once `used.idx` advances; polling the
status byte could read it before the data DMA landed. Lethal for tcc's heavy
multi-sector reads (corrupt ELF, deterministic); intermittent for `fs::create`
(occasional stale `root.size`/dir-block read → misplaced dirent → "cannot open
/r.txt"). Diagnostics proved create itself was always correct (`relookup=57`); the
failure was a *later* stale read. **Fix:** `wait_and_drain` polls `used.idx`
advancing (spec-correct completion), then reads data+status. **14 consecutive
end-to-end passes** followed (the only 2 fails were the first runs, on a host still
hot from ~20 prior runs). The completion poll is correctly *native* (it's a
hardware-contract fact, not a state machine — see the `frame_assessment.md`
2026-05-25 follow-up). The original "open bug" analysis below is left for history.

## TL;DR

- **Committed, good:** S1–S5 (`ef47f3b` is S5; `c1ab829` S4). Note the S5 commit
  contains the per-process fd table but the **original** disk path.
- **Uncommitted, believed-correct:** S6 pipes + `Pipe` FSM + `IoScheduler`
  supervisor FSM + virtio used-ring completion fix + `MAX_OPEN` 16→32 + disk
  serialization. Kernel builds clean; `cargo fmt`/CI-clippy were clean earlier.
- **Validated:** S5 redirection (`2 2 20`) and S6 pipe (`1 3 13`) pass *with the
  supervisor* (seen green in run "io2"/"io4"). The on-device tcc regression I
  introduced was root-caused + fixed (used-ring, below).
- **OPEN BUG (blocker):** an **intermittent** S5 create failure — `echo foo >
  /r.txt` sometimes leaves `/r.txt` un-linked (`cat: cannot open /r.txt`). It has
  **survived all three disk-completion implementations**, so the disk-completion
  layer is NOT the cause. Root cause unknown; suspected in `fs::create`'s
  multi-step dirent/`root.size` update. See below.
- **Environment caveat:** the dev host (arm64 emulating x86_64 via TCG) was badly
  throttled from ~20 back-to-back `console-test` runs; runs started timing out at
  random early needles. Validate on a cool/idle host, one run at a time.

## What was built (uncommitted)

New Frame systems (the dogfooding additions):
- `frame/pipe.frs` → `Pipe` FSM. Pipe end-lifecycle: `$Writable`→`$Drained`,
  writer-presence as the state that decides "read on empty blocks vs returns
  EOF". Reader/writer counts in domain. Driven by `kernel/src/pipe.rs` (which
  owns the 64 KiB ring buffer — the FSM owns only the lifecycle, like `OpenFile`
  owns mode while the VFS owns bytes).
- `frame/io_scheduler.frs` → `IoScheduler` **supervisor** FSM. `$Idle`/`$Busy` +
  a `disk_q` waiter queue in domain. Owns the single-flight virtio-blk engine's
  *access*: `acquire_disk(pid)` grants-or-enqueues, `release_disk()` hands off to
  the next waiter or goes idle. This is a new Frame *shape* for this codebase — a
  coordinator/arbiter, not a per-instance lifecycle. Driven by `sched.rs`.

Native wiring:
- `kernel/src/sched.rs`: `IO_SCHED` instance + `with_io_sched`, `acquire_disk()`
  (drives FSM then `block_current_until(disk_owner(pid))`), `release_disk()`
  (`hand_off` + `wake_pid`), and a **boot bypass** (`!is_preemption_active() ||
  pid==0` returns early — the supervisor may not exist yet and boot is
  single-threaded). Also `block_current_until(ready_fn)` — a lost-wakeup-proof
  block primitive (checks `ready` atomically with marking Blocked).
- `kernel/src/virtio_blk.rs`: `read_sector`/`write_sector` bracket the txn with
  `sched::acquire_disk()`/`release_disk()`. `wait_and_drain` now polls the
  **used-ring index** (`used.idx`) as the completion signal (see the fix below);
  the old `DISK_BUSY`/waiter-array native lock was removed.
- `kernel/src/vfs.rs`: per-process fd table (S5) + `Slot::Pipe` variant +
  `make_pipe`/`is_pipe_read`/`pipe_writers_open`. `MAX_OPEN` raised 16→32 (console
  fds 0/1/2 now occupy table slots; tcc opens many files and needs the headroom).
- `kernel/src/usermode.rs`: `pipe` syscall (#23), deferred pipe-read blocking
  (`do_pipe_read_loop`), `is_known_syscall` ≤ 23.
- `user/src/ish.rs`: `|` pipeline parsing + `run_pipeline` (fork writer→pipe-w /
  reader→pipe-r, parent closes both ends + waits twice). `build_argv` extracted.
- `xtask/src/main.rs`: S6 console-test step (`echo pipe one two | wc` → `1 3 13`).
- `kernel/build.rs` + `kernel/src/frame_systems.rs`: register `pipe` + `io_scheduler`.

## The tcc regression — root-caused and FIXED (keep this)

Symptom: on-device `tcc /hello.c -o /out.elf` ran but `/out.elf` was missing/invalid
(`command not found: /out.elf`). **Deterministic.** Bisected (serialization off +
pipe step skipped → still failed) to my completion-detection change.

Root cause: I had `wait_and_drain` poll the device-written **status byte** as
"done". Virtio only guarantees a request's buffers (data *and* status) are all
written once the device advances **`used.idx`** — per-buffer write order is
unspecified. Polling status could observe it before the data DMA landed. Invisible
for a 1-sector S5 read; tcc's heavy multi-sector reads got stale bytes.

Fix (in `wait_and_drain`): poll `used.idx` advancing (spec-correct, race-free),
then read status. Builds clean. This is correct and should stay regardless of how
the S5 bug below is resolved.

## OPEN BUG: intermittent S5 create failure (the blocker)

Symptom: `echo redir-out > /r.txt` (the redirect child: `open_out` → create →
`dup2`→`exec echo`) intermittently leaves `/r.txt` un-findable. Evidence it's the
*link*, not the data: when it fails, a following `echo x >> /r.txt` makes the file
fresh (so the `>` create's inode was orphaned — written but not linked into the
root dir / `root.size` not durably advanced — so the next create reuses the same
dirent slot). No `#PF`/panic; the child exits cleanly.

**Crucially: this has survived all three disk-completion mechanisms** — the
original IRQ `block_current`, the status-byte poll, and the used-ring poll. So the
completion layer is NOT the cause. The earlier "status-byte fixed S5" was luck on
a 2-sample run.

What's ruled out:
- Disk-completion wait (3 implementations, same flakiness).
- IoScheduler serialization (bisect: disabling it didn't change tcc; S5 is
  single-process anyway — shell `wait()`s, so the redirect child is the only disk
  user during its create → no cross-process clobber).
- fd exhaustion (`MAX_OPEN` 32 didn't matter for this).

Prime suspect: **`fs::create` (`kernel/src/fs.rs:375`) is not atomic across its
disk ops** — `alloc_inode` → `write_inode` → `read_inode(ROOT)` → `block_for` →
`read_block`/`write_dirent`/`write_block` → `root.size += DIRENT_SIZE` →
`write_inode(ROOT)`. Each sector op now yields (`block_current_until`). Hypothesis
to test: a stale `root.size` read, a lost `root.size` write-back, or an
interaction with the multi-block root directory (S4 bumped INODE_BLOCKS and made
the root dir multi-block) such that the dirent is written at offset `root.size`
but `root.size` isn't durably advanced → `namei` (which iterates `[0, root.size)`)
can't see it and the next create overwrites it. (Single-process, so it's not a
classic data race — more likely a read-stale / write-lost across a yield, or an
off-by-one in the multi-block dirent-append path under `block_for` allocating a
new dir block.)

### Recommended next step (on a COOL host)

Add a **failure-only** diagnostic in `fs::create`: right before `Some(ino)`,
re-`lookup(name)` (or re-read the root dir) and if the just-created name is NOT
found, `serial::writeln` the name + `ino` + `root.size` + the dirent block/offset.
That catches the exact lost-create deterministically the next time a run reaches
S5, with the values needed to see whether it's `root.size`, the dirent block, or
`block_for`. Keep prints failure-only so they don't perturb timing (the bug is
timing-sensitive — heavy serial output hid it before).

Then run the console-test ONCE on an idle host (the build is cached; the QEMU run
is what's slow). A clean run also finally validates the used-ring tcc fix +
S5 + S6 end-to-end.

## Commit plan (once S5 is genuinely reliable)

**RESOLVED — diagrams + the gate that blocked them.** The graphviz-version
blocker is fixed at the source: `check-diagrams` now gates the **DOT** (`framec -l
graphviz`, version-independent), not the rendered SVG, so committing the new
diagrams no longer requires CI's exact graphviz. Both systems are registered in
`DIAGRAMS`, all systems have a committed `<name>.dot` (the gated artifact),
`docs/systems/{pipe,io_scheduler}.{md,svg}` are written, and `check-diagrams`
passes. (See `frame_assessment.md` 2026-05-25 tooling entry.)

Original commit-plan note (for history) — likely one commit (changes interleave):
the disk used-ring fix + IoScheduler/Pipe FSMs + S6 pipes + per-process fd-table
pipe support. The `docs/frame_assessment.md` entry for this
episode is already written (2026-05-25). Do NOT commit until a clean console-test
pass is observed (per the "never claim validation without executing" rule).

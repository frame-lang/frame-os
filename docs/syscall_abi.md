# Frame OS syscall ABI (B3)

The user/kernel calling convention for the bare-metal kernel. Minimal by
design — enough to run freestanding user programs that print, exit, and
`fork`/`exec`/`wait`. It grows (file I/O, etc.) at B4 when a filesystem lands.

## Calling convention

User code invokes a syscall with the `syscall` instruction:

| Register | Role |
|---|---|
| `rax` | syscall number (in); return value (out) |
| `rdi` | first argument |
| `rsi` | second argument |
| `rcx`, `r11` | **clobbered** by the `syscall` instruction (return RIP / RFLAGS) |
| all others | preserved across the syscall |

The kernel entry (`usermode.rs::syscall_entry`) switches to the calling
process's per-process kernel stack, builds a full trap frame, and routes the
call through the `SyscallDispatcher` Frame system (which validates the number,
then executes or rejects it). Return is via `iretq`. Syscalls run with
interrupts disabled (FMASK clears IF), so they are not preempted mid-flight.

Unknown syscall numbers are rejected: `SyscallDispatcher` forwards them to its
`$Active.reject` handler (`=> $^`), and the call returns `ENOSYS` (38).

## Syscalls

| # | Name | Args | Returns | Description |
|---|---|---|---|---|
| 0 | `write_char` | `rdi` = byte | `1` | Write one byte to the serial console. |
| 1 | `exit` | `rdi` = code | (never returns) | Terminate the process (`Process → $Zombie`); the scheduler runs another. |
| 2 | `fork` | — | child pid (parent) / `0` (child) | Duplicate the process: eager address-space copy + trap-frame copy. Both run concurrently. |
| 3 | `exec` | `rdi` = program id | (never returns on success) | Replace the process's image with a baked program (no filesystem yet — selected by id). |
| 4 | `wait` | — | child exit code, or `u64::MAX` (ECHILD) | Block until a child exits; reap it (free its `Process` slot + address space) and return its status. |

## Process model

- Each process is a scheduled entity with its own address space (PML4) and
  ring-0 kernel stack; the scheduler preempts user processes in ring 3 and
  switches CR3 + `TSS.RSP0` per process (`sched.rs`).
- Lifecycle is the `Process` Frame system: `$Created → $Ready ⇄ $Blocked →
  $Zombie → $Reaped`. `fork` admits a child; `exit`/a fatal fault moves a
  process to `$Zombie`; a parent's `wait` reaps it to `$Reaped`.

## Signals (basic, native bookkeeping)

The B3 signal subset is implemented as native bookkeeping rather than a Frame
state machine (a full `sigaction`/mask machine would be ceremony for three
signals):

- **SIGKILL** — `Process.kill()` (state-dependent: live → `$Zombie`, no-op once
  exited). Used internally; a user-facing `kill` syscall arrives with the
  richer ABI at B4.
- **SIGSEGV** — a fatal ring-3 page fault routes through `PageFaultHandler`'s
  `$Killing` state, terminating the offending process (the kernel survives).
- **SIGCHLD** — a child's exit wakes its parent if the parent is blocked in
  `wait` (`sched::exit_current` → mark the parent runnable).

## Notes / limits (B3)

- Single-core; the syscall stack switch uses statics rather than `swapgs` +
  per-CPU GS (that arrives at B7/SMP).
- No filesystem yet, so `exec` selects programs by id (baked into the kernel
  image); B4 replaces this with loading from disk.
- Arguments are register-only (no pointer/buffer args yet); strings and buffers
  arrive with the file I/O syscalls at B4.

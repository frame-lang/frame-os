# Frame OS syscall ABI (B3–B4)

The user/kernel calling convention for the bare-metal kernel. Minimal by
design — enough to run freestanding user programs that print, exit,
`fork`/`exec`/`wait` (B3), and do file I/O + `exec` from disk (B4 Step 4a). It
grows further (a richer `kill`, etc.) at later B4 steps.

## Calling convention

User code invokes a syscall with the `syscall` instruction:

| Register | Role |
|---|---|
| `rax` | syscall number (in); return value (out) |
| `rdi` | first argument |
| `rsi` | second argument |
| `rdx` | third argument (used by `read`; B4 Step 4a) |
| `rcx`, `r11` | **clobbered** by the `syscall` instruction (return RIP / RFLAGS) |
| all others | preserved across the syscall |

The `SyscallDispatcher` Frame system carries only the number + first two
arguments (`num`/`a0`/`a1`); a 3-argument syscall (`read`) reads `rdx` from the
trap frame directly, the same way `fork`/`exec` read it. Pointer arguments
(`open`'s path, `read`'s buffer) are user virtual addresses — valid because a
syscall runs in the caller's address space (CR3 is unchanged across entry).

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
| 3 | `exec` | `rdi` = program id | (never returns on success) | Replace the process's image with a baked program (selected by id; pre-filesystem path, kept for the B3 demos). |
| 4 | `wait` | — | child exit code, or `u64::MAX` (ECHILD) | Block until a child exits; reap it (free its `Process` slot + address space) and return its status. |
| 5 | `open` | `rdi` = path ptr, `rsi` = path len | fd, or `u64::MAX` | Open a file by absolute path for reading (B4 Step 4a). Resolves via `fs::namei` through the VFS fd table (one `OpenFile` per fd). |
| 6 | `read` | `rdi` = fd, `rsi` = buf ptr, `rdx` = len | bytes read (`0` = EOF) | Read from an open fd into a user buffer, advancing its offset (B4 Step 4a). |
| 7 | `close` | `rdi` = fd | `0` | Close an fd (`OpenFile → $Closed`) and free its slot (B4 Step 4a). |
| 8 | `exec` (path) | `rdi` = path ptr, `rsi` = path len | (never returns on success) | Replace the process's image with a program loaded **from disk** by path (B4 Step 4a). `u64::MAX` if the path doesn't resolve to a regular file. |

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

## Notes / limits

- Single-core; the syscall stack switch uses statics rather than `swapgs` +
  per-CPU GS (that arrives at B7/SMP).
- Two `exec` forms coexist: `exec` (3) selects a baked program by id (the
  pre-filesystem path, kept for the B3 `fork`/`exec` demos), and `exec` (8)
  loads a program from disk by path (B4 Step 4a). Disk `exec` reads the whole
  ELF into a fixed scratch buffer (64 KiB), enough for the freestanding user
  programs.
- Pointer/buffer arguments exist as of B4 Step 4a (`open`'s path, `read`'s
  buffer). They are read directly as user VAs (CR3 unchanged during a syscall);
  there is no per-byte copy validation yet — a fault in a bad user pointer would
  route through `PageFaultHandler` like any other ring-3 fault.

# Architecture

This document describes how Frame OS is structured: which components exist, which are written in Frame and which in native Rust, and how the pieces fit together for both the hosted-mode shell and the bare-metal kernel.

## Frame syntax conventions used here

A brief note before the architecture proper, since this document references Frame syntax throughout. Frame OS uses the current Frame attribute syntax:

- **`@@[target("rust")]`** at the top of every `.frs` file (the older `@@target rust` form is not used).
- **`@@[main]`** to mark a file's primary system when more than one system is defined in the same file.
- **`@@[persist(<blob_type>)]` with `@@[save(<name>)]` and `@@[load(<name>)]`** for any system that needs serialization. Most Frame OS systems do not.
- **HSM forwarding is explicit** — under Frame's current semantics (RFC-0019), unhandled events do *not* automatically forward to parent states. A child state must declare a trailing `=> $^` (or forward inside a specific handler with `=> $^`) to make parent-state handlers fire. The architectural patterns described below all assume this explicit-forwarding model.
- **Return values use `@@:(expr)`, `@@:return = expr`, or `@@:return(expr)`.** A bare `return` is native; it exits the handler but does not set the return value (the framepiler emits W415 for this).

These are noted up front so the code patterns described below — particularly the HSM error-handling patterns in `Kernel`, `SyscallDispatcher`, and `PageFaultHandler` — are grounded in current Frame syntax rather than an older form.

## Two tracks, one source tree

Frame OS produces two distinct artifacts from a shared source tree:

**Hosted-mode shell** is a single Rust executable that runs as a normal process on Linux, macOS, or Windows. It presents a command prompt, parses input, runs built-in commands and external programs, and handles signals. It is, in shape, a small Unix shell. The interesting property is that every piece of its behavior is implemented as a Frame state machine.

**Bare-metal kernel** is an OS image that boots in QEMU and on real hardware. It manages tasks, drives a serial console, dispatches syscalls (in later milestones), and loads programs. The Frame systems describe its control flow; native Rust handles the unsafe primitives.

Some Frame systems appear in both tracks. `Shell` and `Parser` are reused, with track-specific action implementations. The kernel-specific systems (`Kernel`, `Scheduler`, `Task` / `Process`, `ProcessTable`, `SyscallDispatcher`, `ElfLoader`, `SerialDriver`, `KernelTimer`) only exist in the bare-metal track. The hosted-specific systems (`JobControl`, `Job`) only exist in the hosted track. This selective sharing is the point of organizing the code this way — the Frame layer is portable across deployment shapes for the pieces where portability makes sense, while track-specific systems live where they belong.

## The Frame layer vs. the native layer

This is the project's central architectural commitment, and it deserves to be stated precisely.

**Frame owns control flow.** What state am I in? What event arrived? Given this state and this event, what's the legal response? Which transitions are allowed from here? This is the territory where the Frame argument is strongest: an explicit state graph is more auditable, more localizable, and more compiler-checked than the same logic written as ad-hoc conditionals.

**Native Rust owns data, primitives, and unsafe operations.** Manipulating page tables. Performing context switches. Writing to MSRs. Talking to hardware registers. Implementing a heap allocator. Parsing bytes into structured records. This is the territory where state machines would add ceremony without buying clarity — these are operations, not lifecycles.

The split is roughly 30% Frame, 70% native Rust by line count in the bare-metal kernel. This ratio is the right balance. Substantially more Frame would indicate using state machines where they don't fit; substantially less would indicate Frame isn't doing meaningful work.

In the hosted shell, the ratio inverts somewhat — closer to 50/50 — because more of the shell's logic is genuinely state-shaped (parsing modes, job control, command execution lifecycle) and less of it is data manipulation. This is itself evidence of where Frame fits naturally: user-facing coordination layers more than low-level data plumbing.

## Hosted-mode shell architecture

The hosted shell is the simpler of the two artifacts and a good entry point for understanding the project.

**Frame systems present in the hosted shell:**

- `Shell` — the top-level lifecycle. States: `$Booting → $Prompting → $Parsing → $RunningBuiltin | $RunningExternal → $Prompting`, with `$Exiting` as a terminal sink. Signal handling differs by state, which is the textbook case for state-driven dispatch — Ctrl-C in `$Prompting` clears the line, in `$RunningExternal` kills the child, in `$RunningBuiltin` is ignored.

- `Parser` — turns a typed line into a structured command. States represent parsing modes: `$ReadingWord → $InWord → $InQuotedString → $ReadingWord → $Done`. The state machine handles quoting, escaping, and whitespace coherently.

- `JobControl` and `Job` (later milestone) — `JobControl` is the manager that tracks background jobs and `fg`/`bg` semantics. `Job` is the per-instance state machine — one instance per running job, with states for foreground/background/stopped/done. The pattern (one manager system + N instance systems) appears again in the bare-metal kernel with `ProcessTable` and `Process`.

**Native Rust around the Frame systems:**

- The main loop reads lines via `rustyline` (line editing, history, key bindings).
- The action handlers for builtins use `std::fs`, `std::env`, and so on.
- External commands spawn via `std::process::Command`. The shell blocks in `$RunningExternal` for the duration.
- Signal handling uses `signal-hook` on Unix and a Windows-specific path for Ctrl-C on Windows.

The shell deliberately uses the host OS for everything it can — filesystem operations, process spawning, environment variables. It is not a sandbox or a virtual environment. It's a shell that happens to be a state machine.

### What Frame contributes specifically

Worth being explicit, since this is the smallest example where the Frame argument can be made concretely:

Without Frame, the shell would be a main loop with a `current_state: ShellState` enum and a `match` on that enum in every place a state-dependent decision happens (signal handler, command dispatcher, line reader). That works fine. It's how shells are typically written.

With Frame, the state is implicit in *where the code is executing* — if the active handler is on `$RunningExternal`, you're in `$RunningExternal` by construction. The compiler enforces that every state declares which events it handles. Adding a `$Suspended` state for Ctrl-Z support is a localized change: a new state declaration, a new transition from `$RunningExternal`, and the framepiler regenerates dispatch code that handles it everywhere. Without Frame, the same change would require updating the `match` in every signal handler that branches on `current_state`.

The marginal value on a small shell is modest. It scales up as the shell does. The same argument made larger is the argument for using Frame in the kernel, where the state machines have more states and the dispatch sites are more numerous.

## Bare-metal kernel architecture

The bare-metal kernel is where the Frame argument carries the most weight. The remainder of this section walks through the kernel's structure.

### Layer cake

From boot to user programs, top to bottom. Items are tagged with the milestone (B0-B4) at which they first appear:

```
┌─────────────────────────────────────────────────────────┐
│  User programs (bytecode at B3; ELF at B4 stretch)      │
├─────────────────────────────────────────────────────────┤
│  Shell, builtins (Frame systems, B2)                    │
├─────────────────────────────────────────────────────────┤
│  Bytecode interpreter (Frame system, B3)                │
├─────────────────────────────────────────────────────────┤
│  Scheduler (B1), Task (B1) → Process / ProcessTable (B4)│
├─────────────────────────────────────────────────────────┤
│  SyscallDispatcher (B4), ElfLoader (B4)                 │
├─────────────────────────────────────────────────────────┤
│  PageFaultHandler (B4)                                  │
├─────────────────────────────────────────────────────────┤
│  Drivers — SerialDriver (B0), KernelTimer (B1)          │
├─────────────────────────────────────────────────────────┤
│  Native: paging, GDT/IDT, context switch, heap, MMU     │
├─────────────────────────────────────────────────────────┤
│  Boot stub (assembly + Rust, Limine handoff, B0)        │
└─────────────────────────────────────────────────────────┘
```

The layers above the line are Frame-organized. The layers below are native Rust and assembly. The split is principled, not arbitrary.

### Frame systems in the kernel

Each of these is a `.frs` file. The list grows across milestones — B1 needs `Kernel`, `Scheduler`, `Task`, `SerialDriver`. B3 adds `Interpreter`. B4 adds `Process`, `ProcessTable`, `SyscallDispatcher`, `ElfLoader`, `PageFaultHandler`.

What follows is a *brief description* of each system: what it is, what its states are, why it earns being a state machine. The authoritative reference for each system (once implemented) lives in [`systems/`](systems/) — one file per system, with the state diagram, the full interface, the domain, and the detailed transition rules. The summaries below are the catalog; the per-system docs are the reference.

**`Kernel`** — top-level boot, run, and shutdown lifecycle. Implemented as a hierarchical state machine where `$Booting` is a parent state with phase children (`$InitMemory`, `$InitIDT`, `$InitTimer`, `$InitConsole`, `$LaunchInit`). After boot, sits in `$Running` until shutdown. Panic handling lives on the `$Booting` parent and on `$Running` independently. **The HSM wins here are real**: each init phase declares a trailing `=> $^` so unhandled `panic()` events forward to the parent's handler — panic handling is written once, then explicitly inherited by each child rather than duplicated five times. (Frame's HSM model, post-RFC-0019, requires this forwarding to be explicit; the trade is a small amount of boilerplate per child in exchange for unambiguous dispatch semantics.)

**`Scheduler`** — picks the next task to run on each tick. States: `$Idle` (no runnable tasks), `$PickingNext` (selecting from the ready queue), `$Running` (a task is executing), `$ContextSwitching` (saving/restoring registers). The state machine ensures that operations only happen when they're legal — you can't ask `pick_next_task()` while in `$ContextSwitching`, the dispatcher won't route that event.

**`Task`** (B1) / **`Process`** (B3) — per-task or per-process lifecycle. The textbook state graph is `$Created → $Ready → $Running → $Blocked → $Zombie → $Reaped`. Each instance is one task or process; the kernel holds a fixed-size array of these. State-dependent dispatch makes `kill()` mean different things in different states: from a live state it transitions to `$Zombie`; from `$Zombie`/`$Reaped` it's a no-op. *As implemented (B3 Step 3), `Process` omits `$Running`* — "currently on the CPU" flips every timer tick from the preemptive scheduler's ISR, which cannot fire Frame events (non-reentrant), so it is native scheduler state, not a Frame transition. `$Ready` means "runnable." This is the same honest-modeling call already made for `Task`, `Scheduler`, and `SerialDriver`. `Process` adds `$Zombie`/`$Reaped` over `Task`, and funnels `kill()` to a `$Alive` parent via `=> $^` (written once, inherited by `$Created`/`$Ready`/`$Blocked`). See [`systems/process.md`](systems/process.md).

**`ProcessTable`** (B3) — manager for the process array. The textbook framing is per-slot allocation states (`$Free → $Reserved → $Active → $ZombieAwaitingReap`), where `$Reserved` covers in-progress `fork()` that might fail partway. *As implemented (B3 Step 3), `ProcessTable` is a `JobControl`-style manager* holding `Vec<Process>` with `$HasCapacity ⇄ $Full` under a `$Managing` parent — the per-slot allocation lifecycle would largely duplicate the `Process` lifecycle, and `$Reserved`'s partial-fork case arrives with `fork`/`exec` at Step 5. The one invariant worth a state now is capacity (`spawn()` rejects when `$Full`). See [`systems/process_table.md`](systems/process_table.md).

**`SyscallDispatcher`** (B4) — routes incoming syscalls to handlers, with HSM error handling. `$Active` is a parent state with `$Validating`, `$Executing`, `$Returning` as children. Error events (`bad_arg`, `permission_denied`, `out_of_memory`) are declared as handlers on `$Active`; each child declares a trailing `=> $^` so these error events forward to the parent's handlers when fired from deep inside a child state. This means error paths route correctly without explicit `Result<>` plumbing through every syscall implementation.

**`ElfLoader`** (B3) — loads a static ELF into a process address space. States are loading phases: `$ReadingHeader → $ValidatingHeader → $MappingSegments → $BuildingStack → $Done`, with `$Failed` as a sink that cleans up partial work. Every phase routes failure to the single `$Failed` state, whose enter handler does the rollback once — avoiding a `Result<>` ladder that would obscure the load sequence. *As implemented (B3 Step 4a), this is a flat phase FSM, not an HSM*: the funnel is "many phases → one sink" with shared cleanup in `$Failed`, not `=> $^` forwarding (there is no shared *handler* to centralize — failure detection differs per phase). The phases cascade from construction, like the `Kernel` boot chain. `crate::elf` owns the ELF64 parsing + PT_LOAD mapping; the loaded program is a freestanding-Rust static ELF baked into the kernel image. See [`systems/elf_loader.md`](systems/elf_loader.md).

**`PageFaultHandler`** (B4) — classifies page faults and dispatches to the appropriate response. States: `$Classifying → $StackGrow | $CopyOnWrite | $LazyFault | $Killing`. The parent state `$FaultActive` declares a handler for "unrecoverable, kill the process" events; child states forward via `=> $^` so any deep error in a fault handler routes to process termination without explicit error plumbing.

**`Interpreter`** (B3) — the bytecode VM. The machine block is literally the fetch-decode-execute cycle: `$Fetching → $Decoding → $Exec{Push, Add, Print, ...} → $Fetching → $Halted | $Faulted`. Each opcode is a state. This is arguably the project's most expressive Frame system — the interpreter's structure isn't approximated by a state machine, it *is* a state machine, top to bottom.

**`SerialDriver`** — manages the UART. States: `$Idle → $Transmitting → $Draining → $Idle`. The state machine handles "can I accept another byte right now?" without a `busy` flag scattered through the codebase. Different states respond differently to `write(byte)` — `$Idle` starts transmission, `$Transmitting` queues, `$Draining` blocks or returns busy depending on configuration.

**`KernelTimer`** — coordinates the periodic interrupt source. States: `$Stopped → $Calibrating → $Running`. This system is borderline — the calibration phase has genuinely different behavior than the running phase (calibration measures TSC frequency or similar, running just lets ticks arrive), but if calibration ends up being a single configuration call rather than a multi-step process, `KernelTimer` may collapse to plain Rust. Decision deferred until B1 implementation; included here so the architecture acknowledges the question.

**`Shell`** (in bare-metal, over serial) — same Frame source as the hosted shell, but the *actions* are completely different: writes go to `SerialDriver` instead of `stdout`, and "external commands" map differently. In B2 there are no external commands — only builtins. In B3, the shell can `run <program>` to execute loaded bytecode through the `Interpreter` system, which is the bare-metal analogue of the hosted shell's "shell out to host". The state machines are portable across these very different action surfaces because Frame separates *which states exist and what events they handle* (portable) from *what each handler actually does* (not portable, native code).

### Native Rust modules in the kernel

These deliberately are *not* Frame systems. Each is a piece of work where state machines would add ceremony without clarity benefit. The line-count estimates below are rough; they're included to convey relative size, not as commitments.

- **`boot`** — the entry from Limine. Sets up the initial environment, calls into the Frame `Kernel` system's `boot()` interface method. Order of magnitude: ~150 lines including the assembly stub.

- **`memory`** — physical frame allocator (bitmap or buddy), virtual memory manager (page table manipulation), kernel heap allocator (linked-list or `linked_list_allocator` crate). These are data structures with operations, not lifecycles. Order of magnitude: ~1000 lines, dependent on which allocator design.

- **`arch`** — architecture-specific code. GDT/IDT setup (x86_64), exception level setup (AArch64), context switch assembly, MSR manipulation, port I/O. Order of magnitude: ~600 lines per architecture, mostly distinct between x86_64 and AArch64.

- **`interrupt`** — the low-level interrupt handler entry points. Saves registers, calls into the appropriate Frame system's interface method (timer interrupt → `Scheduler.tick()`, syscall → `SyscallDispatcher.request(...)`, etc.). Order of magnitude: ~200 lines.

- **`elf`** — the actual byte-level ELF parsing. The `ElfLoader` Frame system orchestrates the phases; this module contains the parsing primitives. Order of magnitude: ~300 lines.

The pattern across all of these: native Rust does the *operation*, the Frame system decides *when and in what order* operations happen.

## How the layers connect

The interface between Frame systems and native Rust is straightforward and worth being explicit about.

**Frame systems call native Rust** through actions and through native code embedded in handlers. An action is a private method on the system; it's written as native Rust inside the `actions:` block. Inside a handler, native Rust expressions and statements pass through verbatim. So the `SerialDriver`'s `$Transmitting` enter handler might look like:

```frame
$Transmitting {
    $>(byte: u8) {
        unsafe { write_uart_data(byte); }
        if uart_tx_complete() {
            -> $Idle
        }
    }
}
```

The `unsafe { ... }` block is plain Rust, dropped into the handler body. The framepiler doesn't parse or validate it; it passes it through.

**Native Rust calls Frame systems** through the systems' interface methods. From the boot stub's perspective, the Frame `Kernel` system is a Rust struct (`Kernel`) with public methods (`boot()`, `tick()`, etc.). Calling them looks like calling any other Rust method. Internally, those calls go through Frame's dispatch machinery and route to the appropriate state's handler.

**Interrupts** are the most subtle case. A timer interrupt fires; the CPU jumps to the interrupt handler the IDT was configured with. That handler (native Rust, marked `extern "x86-interrupt"`) saves any registers the handler will clobber, then calls `kernel.tick()`. From there, the Frame state machine handles everything. The pattern is: native code receives the hardware event, frames it as an event to a Frame system, and lets the state machine determine the response.

## Build pipeline

The build pipeline is deliberately conventional. We use Cargo + a small `xtask` crate for orchestration. There is no Makefile, no shell scripts in the canonical path, and no platform-specific build logic that doesn't fit inside Rust.

**`framec` invocation** happens in `build.rs` scripts inside each crate that contains Frame source. The `build.rs` invokes `framec` to convert `.frs` files to `.rs` files in `$OUT_DIR`, then `cargo` compiles the generated Rust as part of the normal build. This is the standard Rust build-script pattern, the same one used by `bindgen`, `tonic-build`, and many others.

**Frame source files** use the current Frame attribute syntax: `@@[target("rust")]` at the top of each `.frs` file, `@@[main]` to mark the primary system if there's more than one in a file, `@@[persist(<type>)]` for any system that needs save/restore (most kernel systems do not). The `@@target` and `@@persist` shorthand forms from earlier Frame versions are not used; Frame OS standardizes on the explicit attribute form for clarity and forward compatibility.

**Cross-compilation targets** are managed through Rust's standard target system. The hosted shell builds for `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, etc., as appropriate. The bare-metal kernel builds for `x86_64-unknown-none`, `aarch64-unknown-none`, and `thumbv6m-none-eabi` (Pi Pico).

**Bootloader integration** uses Limine (chosen over GRUB because Limine builds on all three host OSes including macOS, has a simpler boot protocol, and is actively maintained). The bare-metal build produces a kernel ELF, which is packaged with the Limine binaries and a config file into a bootable image.

**Running the kernel** is via `cargo xtask qemu` (for QEMU x86_64), `cargo xtask qemu-arm` (for QEMU AArch64), or `cargo xtask pico-flash` (for the Pico over USB). These are subcommands of an internal `xtask` crate. The `xtask` pattern keeps all build orchestration in cross-platform Rust rather than scattered across shell scripts.

## Persistence of decisions

A few architectural decisions are load-bearing for the project's coherence. They're written here so they can be referenced and, if needed, deliberately changed rather than drifted from:

**Limine, not GRUB.** Forced by macOS support; kept because Limine is genuinely better for hobby OS work.

**Cargo + xtask, no Make or shell scripts.** Forced by Windows-without-WSL support; kept because it's the modern Rust convention.

**Rust first, C port maintained as a design constraint, not a parallel implementation.** Both the Frame source and the native Rust are written with the C port in mind (no Rust-specific patterns that don't translate, clean ownership boundaries so manual memory management is plausible, no dependency on the type system for correctness beyond what Frame itself provides). The C port is a future artifact, not a currently-maintained one.

**30/70 Frame-to-Rust ratio as a guideline, not a rule.** When deciding whether a new component should be a Frame system, the question is "does this have meaningful state-dependent dispatch?" not "are we using enough Frame?" Tracking the ratio is a sanity check, not a target.

**State diagrams are checked into the repo.** Every Frame system has its `.svg` output committed, regenerated as part of the build, and referenced from documentation. This makes the Frame argument visible to anyone browsing the repo on GitHub without checking out and building.

# Vision

## What Frame OS is for

Frame OS exists to demonstrate, in working code, that an operating system written with explicit state machines is meaningfully clearer, more auditable, and more maintainable than the same OS written conventionally.

The project's primary deliverable is not the OS itself. The primary deliverable is a *credible argument* for the Frame language as a systems-programming tool, with the OS as the proof. A working Frame OS that boots, schedules tasks, drives hardware, and loads programs is the kind of artifact that lets the Frame argument be made on technical merit rather than on speculation.

If Frame OS achieves nothing other than "another hobby Unix-like exists," it has failed its purpose. If it achieves "you can look at this code and immediately see how every part of the kernel works because the state graphs are explicit," it has succeeded.

## Why this argument needs to be made

Every operating system already contains state machines. They're in process lifecycles, TCP connections, USB device enumeration, file descriptor states, scheduler policies, syscall dispatch, interrupt handlers, driver protocols. The state machines aren't optional; they're how the work is organized at the conceptual level.

What's optional is whether they're *visible* in the code.

In a typical OS, state lives in integer fields (`task->state = TASK_RUNNING`), transitions are scattered across functions (`set_task_state(t, TASK_INTERRUPTIBLE)`), and the graph of which transitions are legal exists only in maintainers' heads and in comments. Adding a new state means hunting through the codebase for every place the integer is compared, every function that mutates it, every error path that needs to know about it. The implicit nature of the state machine is the source of an entire class of bugs.

Frame makes the state machines explicit. A `Task` system declares its states, declares which events each state responds to, declares its transitions. The framepiler generates dispatch code (a `match` in Rust, an `if/else` chain in C) that exhaustively handles the state space — and on Rust specifically, the compiler enforces that exhaustiveness. Adding a new state forces every dispatch site to acknowledge it.

This is not a revolutionary idea. UML state charts, Stateflow, Harel statecharts, and a long lineage of state-machine modeling tools have made similar arguments for decades. What Frame adds is that the model and the code are the same artifact. There's no translation step where the engineer might mis-encode the diagram. The diagram *is* the code, with `framec -l graphviz` rendering it back to visual form on demand.

Frame OS is the test: does this argument hold up when you build something real?

## Audience

Three distinct audiences, in priority order:

**1. Systems engineers evaluating Frame.** Someone considering Frame for their own embedded or systems work wants to see whether Frame survives contact with hard problems — interrupt handlers, scheduler design, error paths, hardware drivers. Frame OS demonstrates this by being a hard problem. The argument is: if Frame works here, it'll work for your simpler systems problem.

**2. The Frame language project itself.** Frame OS serves as a stress test for the language and the framepiler. Real systems work surfaces real ergonomic issues — missing patterns, awkward error handling, generated-code performance problems — that wouldn't show up in toy examples. Feedback from this project should flow back into Frame's design and the framepiler's implementation.

**3. OS hobbyists, students, and curious developers.** Frame OS sits in the same intellectual neighborhood as xv6, MINIX, and the various blog-post hobby OSes. For this audience, the value is pedagogical: "this is what an OS looks like when its state machines are written explicitly." If a student reads the Frame OS scheduler and walks away with a clearer mental model of how schedulers work than they'd have gotten from reading Linux's, the project has been useful.

The project is explicitly *not* for production deployment, end users, or anyone looking for "a Linux alternative." See non-goals below.

## What success looks like

Concrete, falsifiable criteria. If we achieve these, the project has succeeded; if we don't, we should be honest about why.

**Technical milestones:**

- Hosted-mode Frame OS shell runs on Linux, macOS, and Windows, with builtins, external command execution, and signal handling. (H3)
- Bare-metal Frame OS boots in QEMU x86_64, prints a banner over serial, runs at least three concurrent Frame-defined tasks under a Frame scheduler. (B1)
- Bare-metal Frame OS runs real user programs in ring 3 — static ELF binaries with `fork`/`exec`/`wait`, isolated per-process address spaces, and preemptive multitasking. (B3)
- Bare-metal Frame OS has an on-disk filesystem and loads + runs programs from disk; a userspace shell `cat`s a file and `exec`s a program from disk. (B4)

**Quality milestones:**

- Every Frame system has a generated state diagram checked into the repo, automatically regenerated as part of the build, and referenced from its per-system doc in [`docs/systems/`](systems/README.md). The template at [`docs/systems/_template.md`](systems/_template.md) defines what "documented" means for a Frame system.
- A reader can navigate from any kernel subsystem's per-system doc to its state diagram to its Frame source without intermediate translation.
- Code organization reflects the principle that state machines belong where lifecycle dispatch matters and plain code belongs everywhere else (see [`architecture.md`](architecture.md) for the specific split).
- Every Frame system has at least a state-graph snapshot test and behavioral tests for its committed transitions. The testing approach is documented in [`testing.md`](testing.md); the per-system template requires each system's doc to specify its test coverage.

**Validation milestones (aspirational, not gated):**

These depend partly on outside factors (audience reception, upstream maintainer decisions). The project succeeds technically without them; achieving them strengthens the case.

- At least one written piece (blog post, paper, conference talk) explains a specific kernel subsystem with the Frame source, the generated diagram, and the equivalent conventional-code version side by side. The reader should be able to judge for themselves whether Frame helped.
- Feedback from building Frame OS produces a documented list of Frame language or framepiler issues — improvements that *would* benefit Frame, regardless of whether they're upstreamed. The list itself is the deliverable.

**Stretch milestone:**

- Bare-metal Frame OS with full Tier-3 capabilities: page tables, user-mode execution, ELF loading, syscalls, hardware-enforced process isolation. (B4)

The stretch milestone is the most ambitious technical achievement but is explicitly optional. Achieving B3 with strong documentation is a more valuable outcome than reaching B4 in a state that's too rough to document or evaluate.

## Non-goals

Equally important to what we're doing is what we're explicitly *not* doing. Listing these upfront prevents scope creep and sets correct expectations.

**Frame OS is not a Linux replacement.** It does not aim for binary compatibility, syscall compatibility, or any user-visible parity with Linux. Linux binaries do not run on Frame OS and never will.

**Frame OS is not a serious deployment target.** Nobody should run Frame OS as their primary OS. It has no security model worth trusting, no production-grade hardware support, no maintenance commitment, no backwards-compatibility guarantees. It's a demonstration artifact.

**Frame OS is not a complete OS.** Many things real OSes have — networking, USB, GPUs, audio, filesystems on disk, multi-core, SMP, virtualization — are explicitly out of scope. Adding any of these would multiply the project's size without strengthening its central argument. Some may appear as future stretch goals; none are committed.

**Frame OS is not certified, certifiable, or on a certification path.** The certification regimes for safety-critical software (DO-178C, ISO 26262, IEC 62304, IEC 61508) require substantial process artifacts and tool qualification that this project does not produce. A future project might use Frame OS as a starting point for a certified RTOS; this project is not that project.

**Frame OS is not a real-time operating system in the formal sense.** It has no guaranteed worst-case latency, no priority inheritance, no formal timing analysis. The Tier-1 microcontroller variant could *become* an RTOS with substantial additional work; it isn't one today.

**Frame OS is not a research vehicle for novel OS design.** The OS designs being demonstrated — cooperative tasks, classical Unix process model, classical scheduler designs — are well-understood and conventional. The novelty is in *how* they're expressed, not *what* is expressed. We are not proposing new OS concepts.

**Frame OS does not run Frame programs at the user level.** User programs are ordinary freestanding native binaries (static ELF, raw syscalls); the Frame state machines live in the kernel and the shell, not in the user programs. A "Frame at the user level" extension is conceivable but not committed. (An earlier plan had a B3 bytecode VM with its own instruction set; that was removed in the 2026-05-20 re-baseline in favor of real ELF user programs.)

## Project values

How we want to work, written down so future decisions can be checked against them:

**Honesty about scope.** When something is hard, say so. When a tier is realistic, say so. When a milestone slips, document why. The credibility of the Frame argument depends on the project's credibility; over-promising hurts both.

**Frame in service of the problem.** Use Frame where it helps. Use native code where it doesn't. The project's purpose is to demonstrate where Frame *is* useful, which requires being equally clear about where it isn't. A kernel that uses Frame for everything would be a worse demonstration than one that uses Frame appropriately.

**Documentation is a deliverable, not an afterthought.** Every Frame system has a doc explaining what it does, why it's a state machine, and what would be lost by not having it as one. The docs are part of how the argument is made.

**Cross-platform from day one.** Linux, macOS, and Windows developers should be able to clone and contribute. The C port should be considered in every architectural decision, even though Rust is the implementation language. Architectural choices that quietly close off the C port should be flagged and decided deliberately, not made by accident.

**Small, finished pieces over large, unfinished ones.** A working B2 with good docs is more valuable than a half-broken B4. Land each milestone in a documented, demonstrable state before moving to the next.

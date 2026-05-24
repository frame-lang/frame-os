# Vendored TinyCC (tcc) — Frame OS B11-3

This directory vendors a **pinned** copy of [TinyCC](https://repo.or.cz/tinycc.git)
so the on-device C toolchain (B11-3) builds reproducibly and is unaffected by
upstream drift — the same philosophy as the pinned `framec 4.2.1` in the dev
container.

- **Upstream:** TinyCC
- **Version:** `0.9.27` (the last official release)
- **Source:** `https://download.savannah.gnu.org/releases/tinycc/tcc-0.9.27.tar.bz2`
- **License:** LGPL 2.1 (see `COPYING`)

## What is vendored

Only the files the **x86_64-linux build** needs — not the other-architecture
backends (`arm*`, `arm64*`, `c67*`, `il-*`), the Windows/PE bits, the test
suite, or the bundled runtime/headers. The set:

- **Compiled translation units:** `tcc.c` (driver), `libtcc.c`, `tccpp.c`
  (preprocessor), `tccgen.c` (generator), `tccelf.c` (ELF), `tccasm.c`,
  `tccrun.c`, `x86_64-gen.c`, `x86_64-link.c`, `i386-asm.c`.
- **`#include`d by `tcc.c`:** `tcctools.c`.
- **Headers / tables:** `tcc.h`, `libtcc.h`, `config.h`, `elf.h`, `stab.h`,
  `stab.def`, `tcctok.h`, `i386-asm.h`, `i386-tok.h`, `x86_64-asm.h`, `tcclib.h`.
- `config.h` is the `./configure --cpu=x86_64` output (just `CONFIG_TCCDIR` +
  `TCC_VERSION`); regenerate by running upstream `./configure` if bumping.

## How it is built

`tcc` is cross-compiled with `x86_64-linux-gnu-gcc -ffreestanding -nostdinc`
(includes: the compiler's freestanding dir for `stdarg`/`stddef`/`stdint`, then
`libc/include`, then this dir) against `frame-libc` (the same toolchain flow as
the B11-2 `chello`, just multi-file) and linked with the Frame OS user linker
script into `/bin/tcc`. tcc 0.9.27 defaults to `ONE_SOURCE`, so the build is two
units: `libtcc.c` (`-DONE_SOURCE=1`, pulls in `tccpp`/`tccgen`/`tccelf`/`tccasm`/
`tccrun`/`x86_64-gen`/`x86_64-link`/`i386-asm`) + `tcc.c` (`-DONE_SOURCE=0`, the
driver, which `#include`s `tcctools.c`).

Build defines:

- `TCC_TARGET_X86_64` — the only backend we build.
- `CONFIG_TCC_STATIC` — drop the `dlopen`/`-rdynamic` path (no `dlfcn.h`).
- `CONFIG_TCCBOOT` — drop tcc's crash-backtrace machinery (it needs `signal`/
  `ucontext`, which Frame OS has no use for; this is a real tcc config option,
  not a source patch). The `-run` JIT itself stays compiled (it only needs
  `mmap`/`mprotect`, stubbed in frame-libc) but is unused — Frame OS's tcc
  writes its output ELF to disk and the shell execs it.

The vendored files are **unmodified** from upstream — the entire port is the
`frame-libc` header set (`libc/include`) + the functions it implements (B11-3c),
so a version bump is a clean re-vendor.

The system headers tcc includes (`stdio`/`stdlib`/`string`/`errno`/`math`/
`fcntl`/`setjmp`/`time` + `stdarg`/`stddef` from the compiler) are supplied by
`frame-libc`'s `libc/include`; the functions it calls are implemented in
`frame-libc` (B11-3c). Files here are **not modified** from upstream — the port
lives entirely in frame-libc, so a version bump is a clean re-vendor.

The `include/` subdirectory (`stdarg.h`, `stddef.h`, `stdbool.h`, `float.h`,
`varargs.h`) is tcc's own intrinsic headers, vendored verbatim from the same
0.9.27 release; they are staged on-device at `{tcc_lib_path}/include` so a
compiled program's `#include <stdarg.h>` resolves.

## Known limitation: static linking of Rust/LLVM objects (B11-3d)

tcc 0.9.27's **fully-static x86-64 linker** mishandles objects with pervasive
GOT/PLT relocations, which is exactly what a Rust/LLVM-compiled `libc.a`
(`core` + `compiler_builtins` + frame-libc) emits. Two confirmed bugs: (1) it
builds PLT stubs for default-visibility globals even in a static link, and those
stubs have a wrong GOT displacement (jump into the PLT, GOT slots unfilled); and
(2) the `.got` for `R_X86_64_GOTPCREL` data relocations is left unfilled. A
program tcc links against the Rust `libc.a` therefore *links* but crashes at
runtime. (Details + disassembly evidence in `docs/frame_assessment.md`,
2026-05-24.)

**We do not patch tcc** (these files stay pristine). Instead, tcc links against a
**C-shim libc** built with `gcc -fno-pic` (no GOT relocations) and
`-fvisibility=hidden` (so tcc's existing linker resolves `PLT32` calls directly,
no PLT) — sidestepping both bugs. The Rust `frame-libc` remains the runtime for
the OS's own programs.

Both bugs *are* fixed in tcc's active `mob` branch (0.9.28rc — the
`build_got_entries` condition gained `|| output_type & TCC_OUTPUT_EXE`), so no
upstream report is warranted (0.9.27 is frozen). We **evaluated adopting mob**
(to drop the C-shim and link the Rust `frame-libc` directly) and **rejected it**
(2026-05-24, see docs/frame_assessment.md): mob's newer feature set needs a
heavy, recurring freestanding port (generate `tccdefs_.h`, vendor `dwarf.h`,
`-DCONFIG_TCC_SEMLOCK=0`, and stub `<signal.h>` + the glibc `ucontext_t` +
`environ` for the unused native `-run` path), and the payoff — a 7–11 MiB Rust
`libc.a` tcc must scan every compile — is worse than the few-KB C-shim. Staying
on pinned 0.9.27 + the C-shim is simpler, smaller, and faster on-device.

## The one-line static-exe PLT patch (V1.0 capstone)

The C-shim solves the bug **for libc**: gcc compiles it `-fvisibility=hidden`,
so its symbols are `STV_HIDDEN` and tcc's `build_got_entries`
(`tccelf.c:1082`) already converts their `PLT32` call relocations to direct
`PC32`. But that conversion fires *only* for hidden/local symbols — **not** for
a tcc-compiled program's **own** default-visibility globals. So a program that
calls its own non-`static` function still routes that call through the broken
static-exe PLT and crashes (`#PF` at a garbage `jmp *GOT(%rip)` address). The
existing tests (`tcchello`, `tcc_hi`, `tcc_assert`) never tripped this — they
only call libc — but the V1.0 capstone's **framec-generated C** (`/fhello.c`,
whose `main` calls `Hello_new`/`Hello_greet`/`Hello_greeted`) is the first real
program to call its own functions, and it exposed the latent defect: the
on-device C toolchain could only ever run printf-only toys.

So we now apply **one surgical patch** to `tccelf.c:1082` — the exact upstream
(`mob`) fix — adding `|| s1->output_type == TCC_OUTPUT_EXE` to that condition:
for a **fully-static executable** (our only output mode; we always link
`-static`, no shared libraries), every intra-image call is safe to resolve
directly, so all `PLT32`/`PC32` function relocations become `PC32`. This is a
minimal, well-understood change that makes the C toolchain usable for real
programs; tcc is otherwise still pristine 0.9.27. (We did *not* adopt mob
wholesale — see the rejection above; this is just the single relocation-fix
line.) The C-shim stays (it's still the link-time libc, and `-fno-pic` keeps the
libc free of GOT relocations); the patch fixes the *caller* side for the
program's own globals.

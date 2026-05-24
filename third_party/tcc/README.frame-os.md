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

`tcc` is cross-compiled with `x86_64-linux-gnu-gcc -ffreestanding` against
`frame-libc` (the same toolchain flow as the B11-2 `chello`, just multi-file)
and linked with the Frame OS user linker script into `/bin/tcc`. The build
defines `TCC_TARGET_X86_64` and compiles the units separately (no `ONE_SOURCE`).

The system headers tcc includes (`stdio`/`stdlib`/`string`/`errno`/`math`/
`fcntl`/`setjmp`/`time` + `stdarg`/`stddef` from the compiler) are supplied by
`frame-libc`'s `libc/include`; the functions it calls are implemented in
`frame-libc` (B11-3c). Files here are **not modified** from upstream — the port
lives entirely in frame-libc, so a version bump is a clean re-vendor.

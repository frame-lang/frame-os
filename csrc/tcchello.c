/* csrc/tcchello.c (B11-3d) — the C program the *on-device* tcc compiles.
 *
 * Staged on the FS at /hello.c. At the shell:
 *     tcc -B/usr/lib/tcc -static /hello.c -o /out.elf
 *     /out.elf
 * tcc reads this through frame-libc's <stdio.h> (in the staged /usr/include),
 * links it with crt1.o + libc.a from /usr/lib, and writes a runnable Frame OS
 * ELF. Deliberately minimal: a #include and a printf prove the include search,
 * the compile, the link, and the exec all work. The return value (7) shows up
 * in the kernel's "[user] exited with code 7" line, a second proof it ran. */
#include <stdio.h>

int main(void) {
    printf("hello from a tcc-compiled program on Frame OS!\n");
    return 7;
}

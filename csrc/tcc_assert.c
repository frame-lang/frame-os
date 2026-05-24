/* csrc/tcc_assert.c (B11-3 follow-up) — exercises a *failing* assert through
 * the on-device tcc + C-shim libc. Staged on the FS at /assert.c; the
 * console-test compiles it and runs it:
 *     tcc -B/usr/lib/tcc -static /assert.c -o /assert.elf
 *     /assert.elf
 * The assertion is false, so __assert_fail prints
 *     /assert.c:<line>: main: Assertion `answer == 42' failed.
 * to stderr and abort()s (exit 134) — the program never reaches the printf.
 * This proves assert() is a real abort, not a no-op. */
#include <assert.h>
#include <stdio.h>

int main(void) {
    int answer = 41; /* deliberately wrong */
    assert(answer == 42);
    printf("unreachable: assert did not fire\n");
    return 0;
}

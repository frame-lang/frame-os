/* csrc/tcc_hi.c (B11-3 follow-up) — a *second* C source, so the console-test
 * can prove `buildc <src>` takes its source path from argv (not a hardcoded
 * /hello.c). Staged at /hi.c; `buildc /hi.c` compiles it to /hi.elf and runs
 * it. The distinct message + exit code (3, vs /hello.c's 7) confirm it was this
 * file that got built and run. */
#include <stdio.h>

int main(void) {
    printf("hi from a second source built by buildc!\n");
    return 3;
}

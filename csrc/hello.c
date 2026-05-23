/* csrc/hello.c (B11-2)
 *
 * A plain C program — no Frame-OS-specific code, just standard C against
 * frame-libc's headers. Cross-compiled on the host with gcc (-ffreestanding
 * -nostdlib), linked against libframe_os_libc.a + its crt0 with the Frame OS
 * linker script, baked onto the disk at /bin/chello, and run from the shell.
 *
 * It exercises the libc end to end: printf (variadic), malloc/free, and a
 * fopen/fprintf/fclose -> fopen/fgets/fclose file round-trip. This proves a
 * normal C program, compiled the normal way, runs on Frame OS — the step
 * before tcc compiles C *on the device* (B11-3). */

#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    printf("chello: hello from C on Frame OS! argc=%d\n", argc);

    /* heap: malloc, fill, print, free */
    char *buf = (char *)malloc(64);
    if (!buf) {
        printf("chello: malloc failed\n");
        return 1;
    }
    for (int i = 0; i < 10; i++) {
        buf[i] = 'A' + i;
    }
    buf[10] = '\0';
    printf("chello: malloc buf = %s\n", buf);
    free(buf);

    /* files: write with fprintf, read back with fgets */
    FILE *f = fopen("/chello.out", "w");
    if (!f) {
        printf("chello: fopen(w) failed\n");
        return 1;
    }
    fprintf(f, "C wrote this: %d\n", 1234);
    fclose(f);

    FILE *g = fopen("/chello.out", "r");
    char line[64];
    if (g && fgets(line, sizeof(line), g)) {
        printf("chello: read back: %s", line); /* line keeps its newline */
    } else {
        printf("chello: read back failed\n");
    }
    if (g) {
        fclose(g);
    }

    printf("chello: done\n");
    return 0;
}

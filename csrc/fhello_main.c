/* csrc/fhello_main.c (V1.0 capstone, C half) — the C `main` appended after the
 * framec-generated Hello FSM (frame/hello.frs -> `framec -l c`). It is
 * concatenated into /fhello.c at build time, so it has NO includes of its own
 * and relies on the generated prefix above it: the `Hello` type, Hello_new /
 * Hello_greet / Hello_greeted, and <stdio.h> (which the generated file pulls
 * in). A real state transition gates the output, mirroring the Rust `fhello`. */

int main(void) {
    Hello* h = Hello_new();
    if (Hello_greeted(h)) {
        printf("fhello: FAIL greeted before greet\n");
        return 1;
    }
    Hello_greet(h);
    if (Hello_greeted(h)) {
        printf("fhello: hello from a Frame system, transpiled to C!\n");
        return 0;
    }
    printf("fhello: FAIL not greeted after greet\n");
    return 1;
}

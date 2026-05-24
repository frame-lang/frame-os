/* libc/cshim/cshim.c (B11-3d) — a tiny C standard library for the programs the
 * on-device tcc compiles.
 *
 * Why a separate C libc when frame-libc already exists? frame-libc is Rust, and
 * a Rust/LLVM staticlib is full of GOT/PLT relocations (R_X86_64_GOTPCREL +
 * PLT32) that tcc 0.9.27's fully-static linker mishandles (broken PLT, unfilled
 * GOT — see third_party/tcc/README.frame-os.md). This shim is compiled with
 * `gcc -fno-pic` (direct addressing → ZERO GOT relocations) and
 * `-fvisibility=hidden` (every symbol hidden → tcc resolves PLT32 calls to them
 * DIRECTLY, no PLT). The result links to only the simple PC32/64 relocations tcc
 * applies reliably. The Rust frame-libc stays the runtime for the OS's own
 * programs; this is purely the link target for tcc-compiled C.
 *
 * Self-contained: no #includes (built `-nostdinc`), only `__builtin_va_*`.
 * Talks to the kernel directly via the Frame OS syscall ABI
 * (rax=number, rdi/rsi/rdx=args): 1=exit, 6=read, 12=write(fd,buf,len).
 *
 * Coverage is deliberately minimal — enough for a printf/puts/malloc hello world
 * and the memcpy/memset tcc emits for struct ops. It grows as on-device programs
 * need more. */

typedef unsigned long size_t;
typedef long ssize_t;
typedef __builtin_va_list va_list;
#define va_start(ap, last) __builtin_va_start(ap, last)
#define va_arg(ap, ty) __builtin_va_arg(ap, ty)
#define va_end(ap) __builtin_va_end(ap)

/* --- raw syscalls --------------------------------------------------------- */

static long syscall3(long n, long a, long b, long c) {
    long ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return ret;
}

__attribute__((noreturn)) void exit(int code) {
    syscall3(1, code, 0, 0);
    for (;;) {
    }
}
__attribute__((noreturn)) void _exit(int code) { exit(code); }
__attribute__((noreturn)) void abort(void) { exit(134); }

ssize_t write(int fd, const void *buf, size_t len) {
    /* stdout/stderr go to the console via write_char (syscall 0), one byte at a
     * time — the kernel routes fd 1/2 that way, not through the file write
     * (syscall 12), which is for real open files. Matches frame-libc's `write`. */
    if (fd == 1 || fd == 2) {
        const unsigned char *p = buf;
        for (size_t i = 0; i < len; i++) syscall3(0, p[i], 0, 0);
        return (ssize_t)len;
    }
    return syscall3(12, fd, (long)buf, (long)len);
}
ssize_t read(int fd, void *buf, size_t len) {
    return syscall3(6, fd, (long)buf, (long)len);
}
int unlink(const char *path) {
    /* syscall 17: 0 ok, -1 (u64::MAX) if it doesn't resolve to a file. */
    size_t n = 0;
    while (path[n]) n++;
    return syscall3(17, (long)path, (long)n, 0) == -1L ? -1 : 0;
}

/* --- mem/string ----------------------------------------------------------- */

void *memset(void *d, int c, size_t n) {
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}
void *memcpy(void *d, const void *s, size_t n) {
    unsigned char *a = d;
    const unsigned char *b = s;
    while (n--) *a++ = *b++;
    return d;
}
void *memmove(void *d, const void *s, size_t n) {
    unsigned char *a = d;
    const unsigned char *b = s;
    if (a < b) {
        while (n--) *a++ = *b++;
    } else {
        a += n;
        b += n;
        while (n--) *--a = *--b;
    }
    return d;
}
int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = a, *y = b;
    while (n--) {
        if (*x != *y) return (int)*x - (int)*y;
        x++;
        y++;
    }
    return 0;
}
size_t strlen(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    return n;
}
int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return (int)(unsigned char)*a - (int)(unsigned char)*b;
}
char *strcpy(char *d, const char *s) {
    char *r = d;
    while ((*d++ = *s++)) {
    }
    return r;
}

/* --- a simple bump allocator over brk (syscall 10) ------------------------ */
/* Each block is prefixed with a 16-byte header storing its usable size, so
 * `realloc` knows how much to copy. The header keeps the returned pointer
 * 16-aligned. `free` is still a no-op (a bump allocator never reclaims). */

static unsigned long heap_cur, heap_end;
void *malloc(size_t n) {
    n = (n + 15) & ~(size_t)15; /* 16-align the usable size */
    if (heap_cur == 0) {
        heap_cur = heap_end = (unsigned long)syscall3(10, 0, 0, 0); /* query brk */
    }
    unsigned long need = n + 16; /* 16-byte size header + usable bytes */
    if (heap_cur + need > heap_end) {
        unsigned long want = heap_cur + need;
        unsigned long grown = (unsigned long)syscall3(10, (long)((want + 0xFFFF) & ~0xFFFFUL), 0, 0);
        if (grown < want) return 0; /* OOM */
        heap_end = grown;
    }
    unsigned long *hdr = (unsigned long *)heap_cur;
    hdr[0] = n; /* usable size, for realloc */
    void *p = (void *)(heap_cur + 16);
    heap_cur += need;
    return p;
}
void free(void *p) { (void)p; /* bump allocator: no per-object free */ }
void *calloc(size_t nmemb, size_t size) {
    size_t n = nmemb * size;
    void *p = malloc(n);
    if (p) memset(p, 0, n);
    return p;
}
void *realloc(void *p, size_t n) {
    if (!p) return malloc(n);
    unsigned long old = ((unsigned long *)p)[-2]; /* size header sits 16 bytes back */
    void *q = malloc(n);
    if (!q) return 0;
    memcpy(q, p, old < n ? old : n);
    return q; /* old block leaks — bump allocator */
}
/* strdup lives here (not with the string fns) so `malloc` is already defined —
 * the shim is built `-nostdinc`, so a forward use would be an implicit decl. */
char *strdup(const char *s) {
    size_t n = strlen(s) + 1;
    char *d = malloc(n);
    if (d) memcpy(d, s, n);
    return d;
}

/* --- minimal printf to stdout (fd 1) -------------------------------------- */

static void outc(char c) { write(1, &c, 1); }
static void outs(const char *s) {
    if (!s) s = "(null)";
    write(1, s, strlen(s));
}
static void outnum(unsigned long v, int base, int sign) {
    char buf[32];
    int i = 0, neg = 0;
    if (sign && (long)v < 0) {
        neg = 1;
        v = (unsigned long)(-(long)v);
    }
    const char *digits = "0123456789abcdef";
    if (v == 0) buf[i++] = '0';
    while (v) {
        buf[i++] = digits[v % (unsigned)base];
        v /= (unsigned)base;
    }
    if (neg) outc('-');
    while (i) outc(buf[--i]);
}
int printf(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    for (const char *p = fmt; *p; p++) {
        if (*p != '%') {
            outc(*p);
            continue;
        }
        p++;
        int lng = 0;
        while (*p == 'l') {
            lng++;
            p++;
        }
        switch (*p) {
            case 'd':
            case 'i':
                outnum(lng ? (unsigned long)va_arg(ap, long) : (unsigned long)(long)va_arg(ap, int), 10, 1);
                break;
            case 'u':
                outnum(lng ? va_arg(ap, unsigned long) : (unsigned long)va_arg(ap, unsigned int), 10, 0);
                break;
            case 'x':
                outnum(lng ? va_arg(ap, unsigned long) : (unsigned long)va_arg(ap, unsigned int), 16, 0);
                break;
            case 'p':
                outs("0x");
                outnum((unsigned long)va_arg(ap, void *), 16, 0);
                break;
            case 'c':
                outc((char)va_arg(ap, int));
                break;
            case 's':
                outs(va_arg(ap, const char *));
                break;
            case '%':
                outc('%');
                break;
            default:
                outc('%');
                outc(*p);
                break;
        }
    }
    va_end(ap);
    return 0;
}
int puts(const char *s) {
    outs(s);
    outc('\n');
    return 0;
}
int putchar(int c) {
    outc((char)c);
    return c;
}

/* --- assert (real abort, not a no-op) ------------------------------------- */
/* `__assert_fail` is what <assert.h>'s assert() calls on a failed check: print
 * "file:line: func: Assertion `expr' failed." to stderr (fd 2), then abort.
 * Hidden visibility (like every shim symbol) → tcc resolves the call directly,
 * no PLT. */
__attribute__((noreturn)) void __assert_fail(const char *expr, const char *file,
                                             unsigned int line, const char *func) {
    if (!file) file = "?";
    if (!func) func = "?";
    if (!expr) expr = "?";
    write(2, file, strlen(file));
    write(2, ":", 1);
    char buf[16];
    int i = 0;
    if (line == 0) buf[i++] = '0';
    while (line) {
        buf[i++] = (char)('0' + line % 10);
        line /= 10;
    }
    while (i) {
        char c = buf[--i];
        write(2, &c, 1);
    }
    write(2, ": ", 2);
    write(2, func, strlen(func));
    write(2, ": Assertion `", 13);
    write(2, expr, strlen(expr));
    write(2, "' failed.\n", 10);
    abort();
}

/* --- crt0 (Rust half) ----------------------------------------------------- */
/* `_start` (crt1.s) hands the SysV initial stack pointer here. `main` is
 * declared hidden so __libc_start's call resolves to a direct PC32 (no PLT),
 * the same reason every symbol in this shim is hidden. */
__attribute__((visibility("hidden"))) extern int main(int argc, char **argv, char **envp);

void __libc_start(unsigned long *sp) {
    int argc = (int)sp[0];
    char **argv = (char **)&sp[1];
    char **envp = argv + argc + 1;
    exit(main(argc, argv, envp));
}

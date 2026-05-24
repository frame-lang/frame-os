/* libc/csrc/strtold.c (B11-3c)
 *
 * The one piece of frame-libc that must be C, not Rust: parsing an 80-bit
 * `long double`. x86 `long double` is the x87 80-bit extended type, which Rust
 * has no equivalent for — so this lives in C, where `long double` is native and
 * the intermediate arithmetic keeps 80-bit precision. Faking it as `f64` would
 * be wrong (precision *and* the calling convention: f64 returns in xmm0, a
 * `long double` in st0).
 *
 * Self-contained: no libc calls, so it has no link dependencies of its own.
 * Compiled by the cross-gcc and linked alongside the Rust staticlib (xtask).
 * tcc uses `long double` only for `1.5L`-style source literals (its formatting
 * is all `double`), so `strtold` is the sole 80-bit symbol it needs. */

static int is_space(int c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f';
}

long double strtold(const char *s, char **end) {
    const char *p = s;
    while (is_space((unsigned char)*p)) {
        p++;
    }
    int neg = 0;
    if (*p == '+') {
        p++;
    } else if (*p == '-') {
        neg = 1;
        p++;
    }
    long double val = 0.0L;
    int any = 0;
    while (*p >= '0' && *p <= '9') {
        val = val * 10.0L + (long double)(*p - '0');
        any = 1;
        p++;
    }
    int frac = 0;
    if (*p == '.') {
        p++;
        while (*p >= '0' && *p <= '9') {
            val = val * 10.0L + (long double)(*p - '0');
            frac++;
            any = 1;
            p++;
        }
    }
    int exp10 = -frac;
    if (any && (*p == 'e' || *p == 'E')) {
        const char *q = p + 1;
        int eneg = 0;
        if (*q == '+') {
            q++;
        } else if (*q == '-') {
            eneg = 1;
            q++;
        }
        if (*q >= '0' && *q <= '9') {
            int e = 0;
            while (*q >= '0' && *q <= '9') {
                e = e * 10 + (*q - '0');
                q++;
            }
            exp10 += eneg ? -e : e;
            p = q;
        }
    }
    /* Scale by 10^exp10 at full long-double precision. */
    long double scale = 1.0L;
    int e = exp10 < 0 ? -exp10 : exp10;
    while (e-- > 0) {
        scale *= 10.0L;
    }
    if (exp10 < 0) {
        val /= scale;
    } else {
        val *= scale;
    }
    if (end) {
        *end = (char *)(any ? p : s);
    }
    return neg ? -val : val;
}

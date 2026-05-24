/* frame-libc <assert.h> (B11-3 follow-up): a real assert that aborts with a
 * diagnostic — no longer a no-op. `assert(expr)` evaluates expr; if it is false
 * it calls __assert_fail(#expr, __FILE__, __LINE__, __func__), which prints
 *     file:line: func: Assertion `expr' failed.
 * to stderr and then abort()s (exit 134). Define NDEBUG before including (or
 * build with -DNDEBUG, as the vendored tcc does) to compile assertions out.
 *
 * Note: unlike a one-shot include guard, the `assert` macro is (re)defined on
 * every inclusion based on the *current* NDEBUG — matching the C standard, so a
 * translation unit may flip NDEBUG between two #includes. Only the
 * __assert_fail prototype is guarded (declaring it once is enough). */
#ifndef _FRAMEOS_ASSERT_DECL
#define _FRAMEOS_ASSERT_DECL
extern void __assert_fail(const char *__assertion, const char *__file,
                          unsigned int __line, const char *__function)
    __attribute__((noreturn));
#endif

#undef assert
#ifdef NDEBUG
#define assert(expr) ((void)0)
#else
#define assert(expr) \
    ((expr) ? (void)0 : __assert_fail(#expr, __FILE__, __LINE__, __func__))
#endif

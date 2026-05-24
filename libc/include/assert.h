/* frame-libc <assert.h> (B11-3). NDEBUG-style: assertions compile to nothing,
 * so frame-libc needs no __assert_fail and tcc's internal asserts cost nothing.
 * (tcc's asserts guard compiler-internal invariants; on a pinned, working tcc
 * they don't fire, and disabling them keeps the libc surface minimal.) */
#ifndef _FRAMEOS_ASSERT_H
#define _FRAMEOS_ASSERT_H

#define assert(expr) ((void)0)

#endif /* _FRAMEOS_ASSERT_H */

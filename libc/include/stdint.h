/* frame-libc <stdint.h> (B11-3 capstone). The vendored tcc 0.9.27 ships no
 * <stdint.h> of its own (only stdarg/stddef/stdbool/float), and on-device there
 * is no host toolchain to borrow one from — so frame-libc provides it. x86-64
 * is LP64: int is 32-bit, long/pointer are 64-bit. framec's C backend uses
 * intptr_t (and the fixed-width types), so a tcc-compiled Frame program needs
 * this. */
#ifndef _FRAMEOS_STDINT_H
#define _FRAMEOS_STDINT_H

typedef signed char int8_t;
typedef short int16_t;
typedef int int32_t;
typedef long int64_t;

typedef unsigned char uint8_t;
typedef unsigned short uint16_t;
typedef unsigned int uint32_t;
typedef unsigned long uint64_t;

typedef long intptr_t;
typedef unsigned long uintptr_t;
typedef long intmax_t;
typedef unsigned long uintmax_t;

#define INT8_MAX 0x7f
#define INT16_MAX 0x7fff
#define INT32_MAX 0x7fffffff
#define INT64_MAX 0x7fffffffffffffffL
#define INT8_MIN (-INT8_MAX - 1)
#define INT16_MIN (-INT16_MAX - 1)
#define INT32_MIN (-INT32_MAX - 1)
#define INT64_MIN (-INT64_MAX - 1)

#define UINT8_MAX 0xff
#define UINT16_MAX 0xffff
#define UINT32_MAX 0xffffffffU
#define UINT64_MAX 0xffffffffffffffffUL

#define INTPTR_MAX INT64_MAX
#define INTPTR_MIN INT64_MIN
#define UINTPTR_MAX UINT64_MAX
#define SIZE_MAX UINT64_MAX

#endif /* _FRAMEOS_STDINT_H */

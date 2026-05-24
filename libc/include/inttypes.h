/* frame-libc <inttypes.h> (B11-3). The fixed-width integer types come from the
 * compiler's freestanding <stdint.h>; this adds the printf/scanf length macros
 * for the 64-bit types that tcc uses in format strings. */
#ifndef _FRAMEOS_INTTYPES_H
#define _FRAMEOS_INTTYPES_H

#include <stdint.h>

#define PRId64 "ld"
#define PRIu64 "lu"
#define PRIx64 "lx"
#define PRIX64 "lX"
#define PRIo64 "lo"

#define PRId32 "d"
#define PRIu32 "u"
#define PRIx32 "x"

#endif /* _FRAMEOS_INTTYPES_H */

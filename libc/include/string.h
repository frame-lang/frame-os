/* frame-libc <string.h> (B11-2). strlen comes from frame-libc; memcpy/memset/
 * memcmp are provided by Rust's compiler-builtins (bundled in the staticlib). */
#ifndef _FRAMEOS_STRING_H
#define _FRAMEOS_STRING_H

typedef unsigned long size_t;

size_t strlen(const char *s);
void *memcpy(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);

#endif /* _FRAMEOS_STRING_H */

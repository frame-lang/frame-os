/* frame-libc <string.h> (B11-2). strlen comes from frame-libc; memcpy/memset/
 * memcmp are provided by Rust's compiler-builtins (bundled in the staticlib). */
#ifndef _FRAMEOS_STRING_H
#define _FRAMEOS_STRING_H

typedef unsigned long size_t;

#ifndef NULL
#define NULL ((void *)0)
#endif

size_t strlen(const char *s);
void *memcpy(void *dst, const void *src, size_t n);
void *memmove(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);

int strcmp(const char *a, const char *b);
int strncmp(const char *a, const char *b, size_t n);
char *strcpy(char *dst, const char *src);
char *strncpy(char *dst, const char *src, size_t n);
char *strcat(char *dst, const char *src);
char *strchr(const char *s, int c);
char *strrchr(const char *s, int c);
char *strstr(const char *haystack, const char *needle);

#endif /* _FRAMEOS_STRING_H */

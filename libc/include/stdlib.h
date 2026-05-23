/* frame-libc <stdlib.h> (B11-2) — allocation + process exit. */
#ifndef _FRAMEOS_STDLIB_H
#define _FRAMEOS_STDLIB_H

typedef unsigned long size_t;

void *malloc(size_t size);
void free(void *ptr);
void *calloc(size_t nmemb, size_t size);
void *realloc(void *ptr, size_t size);
void exit(int code);

#endif /* _FRAMEOS_STDLIB_H */

/* frame-libc <stdlib.h> (B11-2) — allocation + process exit. */
#ifndef _FRAMEOS_STDLIB_H
#define _FRAMEOS_STDLIB_H

typedef unsigned long size_t;

#ifndef NULL
#define NULL ((void *)0)
#endif

void *malloc(size_t size);
void free(void *ptr);
void *calloc(size_t nmemb, size_t size);
void *realloc(void *ptr, size_t size);
void exit(int code);
void abort(void);

int atoi(const char *s);
long strtol(const char *s, char **end, int base);
unsigned long strtoul(const char *s, char **end, int base);
unsigned long long strtoull(const char *s, char **end, int base);
double strtod(const char *s, char **end);
/* strtof/strtold are declared by tcc.h itself (its "non-ISOC99" workaround). */

char *getenv(const char *name);

void qsort(void *base, size_t nmemb, size_t size,
           int (*cmp)(const void *, const void *));

#endif /* _FRAMEOS_STDLIB_H */

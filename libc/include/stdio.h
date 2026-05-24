/* frame-libc <stdio.h> (B11-2) — the subset frame-libc implements.
 *
 * Prototypes for the extern "C" symbols the Rust libc exports. FILE is opaque
 * (a frame-libc FileStream); programs only hold FILE*. printf/fprintf are
 * variadic. size_t is `unsigned long` (8 bytes) on the x86-64 target, matching
 * the Rust usize the implementation uses. */
#ifndef _FRAMEOS_STDIO_H
#define _FRAMEOS_STDIO_H

#include <stdarg.h> /* va_list, for vsnprintf */

typedef struct FILE FILE;
typedef unsigned long size_t;

#ifndef NULL
#define NULL ((void *)0)
#endif

#define EOF (-1)

/* The standard streams as FILE* lvalues (glibc-style), so C code can write
 * `fprintf(stderr, ...)`. frame-libc exports them as globals (B11-3c). */
extern FILE *stdin;
extern FILE *stdout;
extern FILE *stderr;

int printf(const char *fmt, ...);
int fprintf(FILE *f, const char *fmt, ...);
int sprintf(char *buf, const char *fmt, ...);
int snprintf(char *buf, size_t n, const char *fmt, ...);
int vsnprintf(char *buf, size_t n, const char *fmt, va_list ap);
int sscanf(const char *str, const char *fmt, ...);
int puts(const char *s);
int putchar(int c);

FILE *fopen(const char *path, const char *mode);
FILE *fdopen(int fd, const char *mode);
int fclose(FILE *f);
size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *f);
size_t fread(void *ptr, size_t size, size_t nmemb, FILE *f);
int fputs(const char *s, FILE *f);
int fputc(int c, FILE *f);
int fgetc(FILE *f);
char *fgets(char *s, int n, FILE *f);
int fseek(FILE *f, long offset, int whence);
long ftell(FILE *f);
int fflush(FILE *f);
int feof(FILE *f);
int ferror(FILE *f);
void clearerr(FILE *f);
int remove(const char *path);

#endif /* _FRAMEOS_STDIO_H */

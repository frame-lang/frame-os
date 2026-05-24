/* frame-libc <errno.h> (B11-3) — a single global errno. frame-libc is
 * single-threaded per process, so a plain global suffices (no __errno_location
 * TLS indirection). The syscall wrappers set it; tcc only ever reads it. */
#ifndef _FRAMEOS_ERRNO_H
#define _FRAMEOS_ERRNO_H

extern int errno;

/* A few codes tcc / programs may compare against. Values are arbitrary but
 * distinct (frame-libc has no real errno taxonomy yet). */
#define EPERM 1
#define ENOENT 2
#define EIO 5
#define EBADF 9
#define ENOMEM 12
#define EACCES 13
#define EEXIST 17
#define EINVAL 22
#define ERANGE 34

#endif /* _FRAMEOS_ERRNO_H */

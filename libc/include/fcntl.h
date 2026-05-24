/* frame-libc <fcntl.h> (B11-3) — open(2) + its flag set. The flag *values* match
 * the Frame OS open syscall's expectations (bit0 = write, used by libc's open
 * wrapper); the rest are the conventional Linux bits so tcc's `O_RDONLY |
 * O_BINARY` etc. compile and behave. */
#ifndef _FRAMEOS_FCNTL_H
#define _FRAMEOS_FCNTL_H

#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#define O_CREAT 0100
#define O_EXCL 0200
#define O_TRUNC 01000
#define O_APPEND 02000
#define O_BINARY 0 /* no text/binary distinction on Frame OS */

int open(const char *path, int flags, ...);

#endif /* _FRAMEOS_FCNTL_H */

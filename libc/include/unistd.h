/* frame-libc <unistd.h> (B11-3) — POSIX fd I/O + a few process bits tcc uses.
 * Thin wrappers over the Frame OS syscalls (read/write/close/lseek/unlink/dup)
 * plus getcwd/chdir and execvp. size_t/ssize_t/off_t are the x86-64 widths. */
#ifndef _FRAMEOS_UNISTD_H
#define _FRAMEOS_UNISTD_H

typedef unsigned long size_t;
typedef long ssize_t;
typedef long off_t;

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

ssize_t read(int fd, void *buf, size_t count);
ssize_t write(int fd, const void *buf, size_t count);
int close(int fd);
off_t lseek(int fd, off_t offset, int whence);
int unlink(const char *path);
int dup(int fd);
char *getcwd(char *buf, size_t size);
int chdir(const char *path);
int execvp(const char *file, char *const argv[]);

#endif /* _FRAMEOS_UNISTD_H */

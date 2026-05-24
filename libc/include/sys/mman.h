/* frame-libc <sys/mman.h> (B11-3). Only present so tcc's tccrun.c (the in-memory
 * `tcc -run` JIT path) compiles. Frame OS's tcc writes its output ELF to disk and
 * the shell execs it (the chosen run model), so `-run` is never used; the mmap
 * family is stubbed in frame-libc and never called on the compile-to-file path. */
#ifndef _FRAMEOS_SYS_MMAN_H
#define _FRAMEOS_SYS_MMAN_H

typedef unsigned long size_t;

#define PROT_READ 0x1
#define PROT_WRITE 0x2
#define PROT_EXEC 0x4

#define MAP_PRIVATE 0x02
#define MAP_ANONYMOUS 0x20
#define MAP_FAILED ((void *)-1)

void *mmap(void *addr, size_t length, int prot, int flags, int fd, long offset);
int munmap(void *addr, size_t length);
int mprotect(void *addr, size_t length, int prot);

#endif /* _FRAMEOS_SYS_MMAN_H */

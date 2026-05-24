/* frame-libc <setjmp.h> (B11-3). tcc uses setjmp/longjmp for its error-recovery
 * funnel (a parse/codegen error longjmps back to the per-file handler). The
 * jmp_buf holds the 8 callee-saved-ish slots frame-libc's asm setjmp saves:
 * rbx, rbp, r12, r13, r14, r15, rsp, and the return address (rip). gcc lowers a
 * bare `setjmp` call to `_setjmp`, so both names resolve to the same routine. */
#ifndef _FRAMEOS_SETJMP_H
#define _FRAMEOS_SETJMP_H

typedef unsigned long jmp_buf[8];

int setjmp(jmp_buf env);
int _setjmp(jmp_buf env);
void longjmp(jmp_buf env, int val);

#endif /* _FRAMEOS_SETJMP_H */

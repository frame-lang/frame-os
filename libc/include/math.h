/* frame-libc <math.h> (B11-3). tcc uses only ldexp (scaling a parsed float
 * mantissa by a power of two) for `double`, and ldexpl for `long double`. */
#ifndef _FRAMEOS_MATH_H
#define _FRAMEOS_MATH_H

double ldexp(double x, int exp);
long double ldexpl(long double x, int exp);

#endif /* _FRAMEOS_MATH_H */

/* frame-libc <time.h> (B11-3). tcc uses time()/localtime() to stamp __DATE__ and
 * __TIME__ into a translation unit. Frame OS has no wall clock yet, so the
 * frame-libc implementation returns a fixed epoch — fine for a self-hosting
 * compiler (the timestamp need only be a valid, stable value). */
#ifndef _FRAMEOS_TIME_H
#define _FRAMEOS_TIME_H

typedef long time_t;

struct tm {
    int tm_sec;
    int tm_min;
    int tm_hour;
    int tm_mday;
    int tm_mon;
    int tm_year;
    int tm_wday;
    int tm_yday;
    int tm_isdst;
};

time_t time(time_t *t);
struct tm *localtime(const time_t *timep);

#endif /* _FRAMEOS_TIME_H */

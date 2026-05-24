/* frame-libc <sys/time.h> (B11-3). tcc calls gettimeofday() once (to seed a
 * temp-file name / timing); frame-libc returns a fixed time (no wall clock). */
#ifndef _FRAMEOS_SYS_TIME_H
#define _FRAMEOS_SYS_TIME_H

typedef long time_t;
typedef long suseconds_t;

struct timeval {
    time_t tv_sec;
    suseconds_t tv_usec;
};

struct timezone {
    int tz_minuteswest;
    int tz_dsttime;
};

int gettimeofday(struct timeval *tv, struct timezone *tz);

#endif /* _FRAMEOS_SYS_TIME_H */

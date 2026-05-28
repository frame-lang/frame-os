// kernel/src/rtc.rs
//
// The arch-agnostic wall-clock accessor — it sits on the `hal::Clock` seam
// (B-HAL.1). The CMOS mechanism + the BCD/epoch decode now live behind the HAL
// in `arch/<isa>/rtc.rs`; this is the thin forwarder the kernel calls.
//
// The on-device libc `time()`/`gettimeofday()`/`localtime()` read this (via the
// `time()` syscall) so a tcc-compiled program's `__DATE__`/`__TIME__` (which tcc
// computes via `time()` + `localtime()` while preprocessing) and any user
// `time()` call reflect real wall-clock time, not a fixed stub.

use crate::hal::{self, Clock as _};

/// Current wall-clock time as Unix epoch seconds (UTC).
pub fn epoch_secs() -> u64 {
    hal::clock().epoch_secs()
}

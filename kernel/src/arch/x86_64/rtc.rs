// kernel/src/arch/x86_64/rtc.rs
//
// The x86_64 implementation of `hal::Clock`: the CMOS real-time clock
// (Motorola MC146818-compatible) → Unix epoch seconds (B-HAL.1).
//
// This is the *mechanism*, relocated behind the HAL seam. The CMOS is read
// through the index/data port pair (0x70/0x71). Register 0x0A's bit 7 is
// "update in progress" (UIP) — the chip is mid-tick and the time registers are
// unstable. Register 0x0B says whether the values are BCD or binary (bit 2) and
// whether hours are 24-hour (bit 1). We wait out UIP, read all fields twice
// until two reads agree (so we never latch a half-updated value), decode
// BCD/12-hour as the status byte dictates, then fold the broken-down civil time
// into epoch seconds. The RTC is treated as UTC — the same convention QEMU's
// `-rtc base=<timestamp>` presents.
//
// The arch-agnostic `rtc.rs` facade forwards `epoch_secs()` here through
// `hal::clock()`; the on-device libc `time()`/`gettimeofday()`/`localtime()`
// (and hence a tcc-compiled program's `__DATE__`/`__TIME__`) read it.

use crate::hal::Clock;
use crate::io::{inb, outb};

const CMOS_ADDR: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_STATUS_A: u8 = 0x0A;
const REG_STATUS_B: u8 = 0x0B;

const STATUS_A_UIP: u8 = 0x80; // update in progress
const STATUS_B_24H: u8 = 0x02; // 1 = 24-hour mode (else 12-hour, hi bit = PM)
const STATUS_B_BIN: u8 = 0x04; // 1 = binary values (else packed BCD)

fn read_reg(reg: u8) -> u8 {
    // The high bit of the index port also gates NMI; we only ever select the
    // low CMOS registers, leaving that bit clear (NMI enabled), matching the
    // firmware's state on entry.
    outb(CMOS_ADDR, reg);
    inb(CMOS_DATA)
}

fn update_in_progress() -> bool {
    read_reg(REG_STATUS_A) & STATUS_A_UIP != 0
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Raw {
    sec: u8,
    min: u8,
    hour: u8,
    day: u8,
    mon: u8,
    year: u8,
}

fn read_raw() -> Raw {
    while update_in_progress() {} // wait for a stable (non-updating) window
    Raw {
        sec: read_reg(REG_SECONDS),
        min: read_reg(REG_MINUTES),
        hour: read_reg(REG_HOURS),
        day: read_reg(REG_DAY),
        mon: read_reg(REG_MONTH),
        year: read_reg(REG_YEAR),
    }
}

fn bcd_to_bin(v: u8) -> u8 {
    (v & 0x0F) + ((v >> 4) * 10)
}

/// Days since 1970-01-01 for a proleptic-Gregorian y/m/d (Howard Hinnant's
/// `days_from_civil`). `y` is the full year, `m` in [1,12], `d` in [1,31]. For
/// all years ≥ 1970 (the RTC is 21st-century here) the result is non-negative.
fn days_from_civil(y: u64, m: u64, d: u64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // Mar=0 … Feb=11
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// The x86_64 CMOS real-time clock. A zero-sized handle over the CMOS ports —
/// the HAL's `Clock` device.
pub struct CmosRtc;

static RTC: CmosRtc = CmosRtc;

/// The x86_64 wall-clock source (the CMOS RTC).
pub fn clock() -> &'static CmosRtc {
    &RTC
}

impl Clock for CmosRtc {
    /// Current wall-clock time as Unix epoch seconds (UTC). Reads the RTC twice
    /// and retries until two consecutive reads agree, so a mid-update tick is
    /// never latched.
    fn epoch_secs(&self) -> u64 {
        let mut a = read_raw();
        loop {
            let b = read_raw();
            if a == b {
                break;
            }
            a = b;
        }

        let status_b = read_reg(REG_STATUS_B);
        let is_bin = status_b & STATUS_B_BIN != 0;
        let is_24h = status_b & STATUS_B_24H != 0;
        let conv = |v: u8| if is_bin { v } else { bcd_to_bin(v) };

        let sec = conv(a.sec) as u64;
        let min = conv(a.min) as u64;
        // In 12-hour mode the PM flag is bit 7 of the *raw* hour byte (set
        // before BCD decoding), so strip it before converting.
        let pm = !is_24h && (a.hour & 0x80 != 0);
        let mut hour = conv(a.hour & 0x7F) as u64;
        if !is_24h {
            if pm {
                if hour != 12 {
                    hour += 12;
                }
            } else if hour == 12 {
                hour = 0;
            }
        }
        let day = conv(a.day) as u64;
        let mon = conv(a.mon) as u64;
        let year = 2000 + conv(a.year) as u64; // RTC year is 0..99; assume 21st century

        days_from_civil(year, mon, day) * 86_400 + hour * 3_600 + min * 60 + sec
    }
}

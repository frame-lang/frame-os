// kernel/src/pit.rs
//
// 8254 Programmable Interval Timer (B1 Step 3).
//
// Channel 0 is wired to IRQ0. We program it in mode 3 (square-wave
// generator) with a divisor chosen for the requested frequency, so it
// raises IRQ0 periodically — the heartbeat that drives preemption.

use crate::io::outb;

const PIT_CH0: u16 = 0x40;
const PIT_CMD: u16 = 0x43;

/// PIT input clock (~1.193182 MHz).
const PIT_BASE_HZ: u32 = 1_193_182;

/// Program channel 0 to fire IRQ0 at approximately `hz` per second.
pub fn init(hz: u32) {
    let divisor = (PIT_BASE_HZ / hz) as u16;
    // Command: channel 0, access lobyte+hibyte, mode 3 (square wave), binary.
    outb(PIT_CMD, 0x36);
    outb(PIT_CH0, (divisor & 0xFF) as u8);
    outb(PIT_CH0, (divisor >> 8) as u8);
}

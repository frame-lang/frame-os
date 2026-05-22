// kernel/src/spin.rs
//
// The kernel's spinlock primitive (B7 Step 2) — the locking foundation for SMP.
// `SpinLock<T>` is a test-and-set lock that is **IRQ-safe**: acquiring it saves
// and clears the interrupt flag on the current core, and releasing it restores
// the prior flag. That matters because a lock can be taken both from mainline
// code and from an interrupt handler on the *same* core: if the timer ISR fired
// while mainline held the lock, it would spin forever waiting for a release that
// can't happen (the mainline is suspended) — a self-deadlock. Disabling
// interrupts for the duration of the critical section makes that impossible,
// while cross-core contention is resolved by the spin.
//
// LOCK ORDERING (documented, per the B7 plan): locks must be acquired in a fixed
// global order to avoid deadlock. Current locks + their rank (acquire low→high,
// never the reverse):
//   1. (none yet hold another lock while locked)
// As of Step 2 every `SpinLock` is a *leaf* — held only for a short critical
// section, never while acquiring a second lock. New locks that nest must be
// added to this list with a rank, and code must respect the order.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// An IRQ-safe test-and-set spinlock.
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// Safe to share across cores: access to `data` is serialized by `locked`.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(value),
        }
    }

    /// Acquire the lock, returning a guard. Interrupts are disabled on this core
    /// until the guard is dropped (and restored to their prior state then).
    pub fn lock(&self) -> SpinGuard<'_, T> {
        let irq_was_enabled = interrupts_enabled();
        unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
        while self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Spin with the pause hint while another core holds it. (Interrupts
            // are already disabled on this core — the holder is on another core
            // and will release; on a single core the lock is never contended.)
            core::hint::spin_loop();
        }
        SpinGuard {
            lock: self,
            irq_was_enabled,
        }
    }
}

/// RAII guard: derefs to the protected value; releases the lock + restores
/// interrupts on drop.
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
    irq_was_enabled: bool,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}
impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}
impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.irq_was_enabled {
            unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        }
    }
}

/// Whether the interrupt flag (RFLAGS.IF, bit 9) is currently set on this core.
fn interrupts_enabled() -> bool {
    let flags: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) flags, options(nomem, preserves_flags));
    }
    flags & (1 << 9) != 0
}

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
// LOCK ORDERING (documented; enforced at runtime by the rank checker below, R5a):
// locks that may be held *while acquiring another lock* are assigned a **rank**,
// and must be acquired strictly low→high, never the reverse. A core that tries to
// acquire a lock whose rank is ≤ the highest rank it already holds panics with a
// lock-order violation *before* acquiring — catching the bug at the point of the
// reversal instead of letting two cores deadlock. Current ranked locks:
//   rank 1  LOCK_A   (nested-lock stress, outer)   — kernel/src/lockorder.rs
//   rank 2  LOCK_B   (nested-lock stress, inner)
// Every other `SpinLock` is a *leaf* (rank 0, unchecked): held only for a short
// critical section, never while acquiring a second lock. A leaf lock MAY be taken
// while holding a ranked lock (rank 0 is "no constraint"). New nesting locks get a
// rank here and use `with_rank`.

use crate::percpu::MAX_CPUS;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// `0` = leaf/unranked (no ordering check). Nonzero ranks are checked.
const LEAF: u8 = 0;

/// Highest lock rank each core currently holds (0 = none). Single-writer per core
/// (a core only updates its own slot, with interrupts disabled inside `lock`), so
/// `Relaxed` is sufficient.
static HELD_RANK: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];

/// An IRQ-safe test-and-set spinlock with an optional acquire-order rank.
pub struct SpinLock<T> {
    locked: AtomicBool,
    rank: u8,
    data: UnsafeCell<T>,
}

// Safe to share across cores: access to `data` is serialized by `locked`.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// A leaf lock (rank 0): never held while acquiring another lock. Not checked.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            rank: LEAF,
            data: UnsafeCell::new(value),
        }
    }

    /// A ranked lock that may be held while acquiring higher-ranked locks. The
    /// rank checker enforces the global order (see the module header).
    pub const fn with_rank(value: T, rank: u8) -> Self {
        Self {
            locked: AtomicBool::new(false),
            rank,
            data: UnsafeCell::new(value),
        }
    }

    /// Acquire the lock, returning a guard. Interrupts are disabled on this core
    /// until the guard is dropped (and restored to their prior state then).
    pub fn lock(&self) -> SpinGuard<'_, T> {
        let irq_was_enabled = interrupts_enabled();
        unsafe { core::arch::asm!("cli", options(nomem, nostack)) };

        // Lock-order check (R5a). With interrupts now off, this core can't be
        // preempted mid-check. A ranked lock must rank strictly above every rank
        // this core already holds; otherwise it's an ordering violation that would
        // risk deadlock against another core acquiring in the opposite order.
        let mut saved_rank = LEAF;
        if self.rank != LEAF {
            let cpu = crate::percpu::this_cpu_index() as usize;
            saved_rank = HELD_RANK[cpu].load(Ordering::Relaxed);
            if self.rank <= saved_rank {
                panic!(
                    "lock order violation: acquiring rank {} while holding rank {}",
                    self.rank, saved_rank
                );
            }
            HELD_RANK[cpu].store(self.rank, Ordering::Relaxed);
        }

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
            saved_rank,
        }
    }
}

/// RAII guard: derefs to the protected value; releases the lock + restores
/// interrupts (and this core's held-rank) on drop.
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
    irq_was_enabled: bool,
    saved_rank: u8,
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
        // Restore this core's held-rank to what it was before this lock (R5a).
        // Interrupts are still disabled here (this lock is still held), so reading
        // this_cpu_index + the store is race-free on this core.
        if self.lock.rank != LEAF {
            let cpu = crate::percpu::this_cpu_index() as usize;
            HELD_RANK[cpu].store(self.saved_rank, Ordering::Relaxed);
        }
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

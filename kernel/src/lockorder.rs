// kernel/src/lockorder.rs
//
// Nested-lock deadlock-avoidance stress (R5a) — "beyond the leaf-lock stage."
// B7 Step 2 proved a single (leaf) `SpinLock` serializes a counter across cores.
// This exercises *nested* locking: two ranked locks acquired in a fixed global
// order on every core, the case where a wrong order would deadlock.
//
// `LOCK_A` (rank 1) is always acquired *before* `LOCK_B` (rank 2). Every core runs
// the same `A → B → bump both → release B → release A` loop. Because all cores
// acquire in the same order, two cores can never hold one lock while waiting on
// the other in opposite directions — no deadlock. And the `SpinLock` rank checker
// (see `spin.rs`) would *panic at the acquire* if any core tried `B → A`, catching
// the ordering bug before it could deadlock. The shared counters end at exactly
// `cores × ITERS` iff every increment was serialized with no lost update.

use crate::spin::SpinLock;
use core::sync::atomic::{AtomicUsize, Ordering};

// Ranked so the checker enforces A-before-B (see the LOCK ORDERING note in spin.rs).
static LOCK_A: SpinLock<u64> = SpinLock::with_rank(0, 1);
static LOCK_B: SpinLock<u64> = SpinLock::with_rank(0, 2);

const ITERS: u64 = 20_000;
static DONE: AtomicUsize = AtomicUsize::new(0);

/// Run the nested-lock stress on the calling core: take `A`, then `B` (the legal
/// high→low *nesting*, low→high *acquire* order), bump both counters, release.
/// The guards drop in reverse declaration order (B then A) — the correct unwind.
pub fn stress() {
    for _ in 0..ITERS {
        let mut a = LOCK_A.lock(); // outer, rank 1
        *a += 1;
        let mut b = LOCK_B.lock(); // inner, rank 2 — legal (2 > 1)
        *b += 1;
    }
    DONE.fetch_add(1, Ordering::SeqCst);
}

/// How many cores have finished the stress.
pub fn done_count() -> usize {
    DONE.load(Ordering::SeqCst)
}

/// The two counters' final values (read under their locks). Equal to
/// `cores × ITERS` iff every nested increment serialized correctly.
pub fn totals() -> (u64, u64) {
    let a = *LOCK_A.lock();
    let b = *LOCK_B.lock();
    (a, b)
}

/// Expected counter total for `cores` participating cores.
pub fn expected(cores: u64) -> u64 {
    cores * ITERS
}

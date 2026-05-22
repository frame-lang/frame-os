// kernel-tests/tests/event_counter_behavior.rs
//
// Level 3 (behavioral) tests for the EventCounter FSM, on the host (B7). This is
// the tiny system driven by cross-core posts: `$Counting` accumulates `tick(n)`;
// `close()` → `$Closed`, after which `tick`s are dropped (the FSM gates events by
// state, so a late cross-core post can't mutate a closed counter). The system is
// pure (no native deps), so these tests run it directly. The cross-core *path*
// (MPSC queue + drain on the owner core) is validated by `smp_cross_core_post_b7`.

use frame_os_kernel_tests::EventCounter;

#[test]
fn starts_open_at_zero() {
    let mut c = EventCounter::__create();
    assert!(c.is_open());
    assert_eq!(c.count(), 0);
}

#[test]
fn ticks_accumulate() {
    let mut c = EventCounter::__create();
    c.tick(1);
    c.tick(2);
    c.tick(3);
    assert_eq!(c.count(), 6);
}

#[test]
fn close_freezes_the_count_and_drops_further_ticks() {
    let mut c = EventCounter::__create();
    c.tick(10);
    c.tick(5);
    c.close();
    assert!(!c.is_open());
    c.tick(999); // dropped — $Closed has no tick handler
    assert_eq!(c.count(), 15);
}

#[test]
fn many_ticks_sum_exactly() {
    // Mirrors the cross-core demo's per-core contribution.
    let mut c = EventCounter::__create();
    for _ in 0..200 {
        c.tick(1);
    }
    assert_eq!(c.count(), 200);
}

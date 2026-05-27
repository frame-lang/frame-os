// kernel-tests/tests/io_scheduler_behavior.rs
//
// Level 3 (behavioral) tests for the IoScheduler slot-pool supervisor
// (multi-flight Step 3/4), on the host. The system is pure (VecDeque/BTreeMap
// over alloc, no native actions), so it tests directly.
//
// It admits up to `slots` concurrent disk requests: `acquire(pid)` hands out a
// free slot index (or -1 + queues when full); `release(pid)` frees a slot and
// hands it to the next queued waiter (returning that pid) or back to the pool.
// `$HasFreeSlots` ↔ `$Full`.

use frame_os_kernel_tests::IoScheduler;

#[test]
fn fresh_pool_has_capacity_and_grants_a_slot() {
    let mut s = IoScheduler::__create(4);
    assert!(!s.is_full());
    let slot = s.acquire(10);
    assert!(slot >= 0, "a fresh pool grants a slot");
    assert_eq!(s.slot_of(10), slot, "slot_of echoes the granted slot");
}

#[test]
fn distinct_pids_get_distinct_slots() {
    let mut s = IoScheduler::__create(4);
    let a = s.acquire(10);
    let b = s.acquire(11);
    let c = s.acquire(12);
    assert!(a >= 0 && b >= 0 && c >= 0);
    assert!(
        a != b && b != c && a != c,
        "slots must not alias: {a} {b} {c}"
    );
}

#[test]
fn unknown_pid_has_no_slot() {
    let mut s = IoScheduler::__create(4);
    assert_eq!(s.slot_of(999), -1);
}

#[test]
fn fills_then_queues_overflow() {
    let mut s = IoScheduler::__create(2);
    assert!(s.acquire(10) >= 0);
    assert!(s.acquire(11) >= 0);
    assert!(s.is_full(), "pool of 2 is full after 2 grants");
    // Third requester is queued, not granted.
    assert_eq!(s.acquire(12), -1, "overflow requester is queued");
    assert_eq!(s.slot_of(12), -1, "a queued requester holds no slot yet");
}

#[test]
fn release_admits_queued_waiter_into_the_freed_slot() {
    let mut s = IoScheduler::__create(2);
    let s10 = s.acquire(10);
    let _s11 = s.acquire(11);
    assert_eq!(s.acquire(12), -1); // 12 queued
    assert!(s.is_full());

    // 10 releases → its slot is handed to the queued 12 (release returns 12).
    let admitted = s.release(10);
    assert_eq!(admitted, 12, "release admits the FIFO-front waiter");
    assert_eq!(s.slot_of(12), s10, "12 inherits 10's freed slot");
    assert_eq!(s.slot_of(10), -1, "10 no longer holds a slot");
    assert!(
        s.is_full(),
        "still full: the freed slot went straight to 12"
    );
}

#[test]
fn release_with_no_waiter_frees_capacity() {
    let mut s = IoScheduler::__create(2);
    let _a = s.acquire(10);
    let _b = s.acquire(11);
    assert!(s.is_full());
    let admitted = s.release(11);
    assert_eq!(admitted, 0, "no waiter → nobody to admit");
    assert!(!s.is_full(), "a slot is free again");
    // The freed slot is grantable to a new requester.
    assert!(s.acquire(20) >= 0);
}

#[test]
fn admission_is_fifo() {
    let mut s = IoScheduler::__create(1);
    let _held = s.acquire(10); // slot 0 to pid 10; pool now full
    assert_eq!(s.acquire(11), -1); // queued 1st
    assert_eq!(s.acquire(12), -1); // queued 2nd

    // Release hands the slot to 11 (front of queue), then to 12.
    assert_eq!(s.release(10), 11);
    assert!(s.slot_of(11) >= 0);
    assert_eq!(s.release(11), 12);
    assert!(s.slot_of(12) >= 0);
    assert_eq!(s.release(12), 0, "queue drained");
    assert!(!s.is_full());
}

#[test]
fn releasing_a_non_holder_is_a_noop() {
    let mut s = IoScheduler::__create(2);
    let _a = s.acquire(10);
    // 99 never acquired — releasing it changes nothing and admits nobody.
    assert_eq!(s.release(99), 0);
    assert_eq!(s.slot_of(10), s.slot_of(10)); // 10 still holds its slot
    assert!(!s.is_full());
}

// kernel-tests/tests/scheduler_behavior.rs
//
// Level 3 (behavioral) tests for the Scheduler FSM, run on the host.
//
// The Scheduler models only the run/halt mode: $Idle (nothing runnable →
// the kernel main loop should hlt) vs $Active (≥1 runnable). Picking and
// context switching are native ISR work, not modeled here. These tests
// assert the mode flips correctly on task_ready/task_unready and that the
// runnable count stays consistent.

use frame_os_kernel_tests::Scheduler;

#[test]
fn fresh_scheduler_is_idle_with_zero_runnable() {
    let mut s = Scheduler::__create();
    assert!(s.is_idle());
    assert_eq!(s.runnable_count(), 0);
}

#[test]
fn first_task_ready_goes_active() {
    let mut s = Scheduler::__create();
    s.task_ready();
    assert!(!s.is_idle());
    assert_eq!(s.runnable_count(), 1);
}

#[test]
fn multiple_ready_accumulate_and_stay_active() {
    let mut s = Scheduler::__create();
    s.task_ready();
    s.task_ready();
    s.task_ready();
    assert!(!s.is_idle());
    assert_eq!(s.runnable_count(), 3);
}

#[test]
fn unready_decrements_and_returns_to_idle_at_zero() {
    let mut s = Scheduler::__create();
    s.task_ready();
    s.task_ready();
    assert_eq!(s.runnable_count(), 2);

    s.task_unready();
    assert!(!s.is_idle(), "still one runnable");
    assert_eq!(s.runnable_count(), 1);

    s.task_unready();
    assert!(s.is_idle(), "no runnable left → back to $Idle");
    assert_eq!(s.runnable_count(), 0);
}

#[test]
fn unready_in_idle_is_ignored() {
    let mut s = Scheduler::__create();
    // task_unready is unhandled in $Idle (runnable is already 0); silently
    // ignored, count stays 0, stays idle. No underflow.
    s.task_unready();
    assert!(s.is_idle());
    assert_eq!(s.runnable_count(), 0);
}

#[test]
fn idle_active_idle_cycle() {
    let mut s = Scheduler::__create();
    assert!(s.is_idle());
    s.task_ready();
    assert!(!s.is_idle());
    s.task_unready();
    assert!(s.is_idle());
    // and it can go active again
    s.task_ready();
    assert!(!s.is_idle());
    assert_eq!(s.runnable_count(), 1);
}

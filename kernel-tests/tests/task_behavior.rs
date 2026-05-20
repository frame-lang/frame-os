// kernel-tests/tests/task_behavior.rs
//
// Level 3 (behavioral) tests for the Task FSM, run on the host.
//
// Task models the coarse, normal-context lifecycle of a kernel thread:
// $Created → $Ready ⇄ $Blocked → $Terminated. There is intentionally no
// $Running state (see frame/task.frs): "on the CPU" is native scheduler
// state, not a Frame transition. These tests assert the lifecycle and that
// the terminal state is a sink.

use frame_os_kernel_tests::Task;

#[test]
fn fresh_task_is_created_not_ready_blocked_or_terminated() {
    let mut t = Task::__create(7);
    assert_eq!(t.id(), 7, "constructor id should be preserved");
    assert!(!t.is_ready());
    assert!(!t.is_blocked());
    assert!(!t.is_terminated());
}

#[test]
fn make_ready_admits_to_ready() {
    let mut t = Task::__create(1);
    t.make_ready();
    assert!(t.is_ready());
    assert!(!t.is_blocked());
    assert!(!t.is_terminated());
}

#[test]
fn block_then_unblock_round_trips_ready_blocked() {
    let mut t = Task::__create(1);
    t.make_ready();
    t.block();
    assert!(t.is_blocked());
    assert!(!t.is_ready());
    t.unblock();
    assert!(t.is_ready());
    assert!(!t.is_blocked());
}

#[test]
fn terminate_from_ready_is_terminal() {
    let mut t = Task::__create(1);
    t.make_ready();
    t.terminate();
    assert!(t.is_terminated());
    assert!(!t.is_ready());
}

#[test]
fn terminate_from_blocked_is_terminal() {
    let mut t = Task::__create(1);
    t.make_ready();
    t.block();
    t.terminate();
    assert!(t.is_terminated());
    assert!(!t.is_blocked());
}

#[test]
fn terminate_from_created_is_terminal() {
    let mut t = Task::__create(1);
    t.terminate();
    assert!(t.is_terminated());
}

#[test]
fn terminated_is_a_sink() {
    let mut t = Task::__create(1);
    t.make_ready();
    t.terminate();
    assert!(t.is_terminated());
    // Further lifecycle events are unhandled in $Terminated and silently
    // ignored (explicit-only-forwarding) — we stay terminated.
    t.make_ready();
    t.block();
    t.unblock();
    assert!(
        t.is_terminated(),
        "no event should resurrect a terminated task"
    );
    assert!(!t.is_ready());
    assert!(!t.is_blocked());
}

#[test]
fn id_is_independent_of_state() {
    let mut t = Task::__create(42);
    assert_eq!(t.id(), 42);
    t.make_ready();
    assert_eq!(t.id(), 42);
    t.terminate();
    assert_eq!(t.id(), 42);
}

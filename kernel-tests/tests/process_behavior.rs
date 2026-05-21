// kernel-tests/tests/process_behavior.rs
//
// Level 3 (behavioral) tests for the Process HSM, on the host. Process is the
// B3 successor to Task: same coarse lifecycle plus $Zombie (exited, awaiting
// reap) and $Reaped (status collected). No native dependencies, so no
// test-double is needed — the system is pure lifecycle bookkeeping.
//
// The HSM payload under test is the kill() funnel: kill() is handled once on
// the $Alive parent and forwarded to from $Created/$Ready/$Blocked via `=> $^`.
// These tests exercise that every live state reaches $Zombie on kill(), and
// that exit()/reap() carry the status through.

use frame_os_kernel_tests::Process;

#[test]
fn fresh_process_is_created() {
    let mut p = Process::__create(7);
    assert_eq!(p.pid(), 7);
    assert_eq!(p.state_name(), "Created");
    assert!(!p.is_ready());
    assert!(!p.is_blocked());
    assert!(!p.is_zombie());
    assert!(!p.is_reaped());
    assert_eq!(p.exit_code(), 0);
}

#[test]
fn make_ready_admits_to_ready() {
    let mut p = Process::__create(1);
    p.make_ready();
    assert!(p.is_ready());
    assert_eq!(p.state_name(), "Ready");
}

#[test]
fn block_then_unblock_round_trips() {
    let mut p = Process::__create(1);
    p.make_ready();
    p.block();
    assert!(p.is_blocked());
    assert_eq!(p.state_name(), "Blocked");
    p.unblock();
    assert!(p.is_ready());
}

#[test]
fn voluntary_exit_records_code_and_zombifies() {
    let mut p = Process::__create(1);
    p.make_ready();
    p.exit(42);
    assert!(p.is_zombie());
    assert_eq!(p.state_name(), "Zombie");
    assert_eq!(p.exit_code(), 42);
}

#[test]
fn kill_from_created_zombifies_with_sentinel() {
    let mut p = Process::__create(1);
    p.kill(); // forwarded $Created => $Alive
    assert!(p.is_zombie());
    assert_eq!(p.exit_code(), -1, "kill records the -1 killed sentinel");
}

#[test]
fn kill_from_ready_zombifies_via_parent() {
    let mut p = Process::__create(1);
    p.make_ready();
    p.kill(); // forwarded $Ready => $Alive
    assert!(p.is_zombie());
    assert_eq!(p.exit_code(), -1);
}

#[test]
fn kill_from_blocked_zombifies_via_parent() {
    let mut p = Process::__create(1);
    p.make_ready();
    p.block();
    p.kill(); // forwarded $Blocked => $Alive
    assert!(p.is_zombie());
    assert_eq!(p.exit_code(), -1);
}

#[test]
fn reap_collects_status_and_moves_to_reaped() {
    let mut p = Process::__create(1);
    p.make_ready();
    p.exit(99);
    let code = p.reap();
    assert_eq!(code, 99, "reap returns the recorded exit code");
    assert!(p.is_reaped());
    assert_eq!(p.state_name(), "Reaped");
}

#[test]
fn zombie_ignores_lifecycle_events() {
    let mut p = Process::__create(1);
    p.make_ready();
    p.exit(5);
    // Per explicit-only-forwarding, these are unhandled in $Zombie → ignored.
    p.make_ready();
    p.block();
    p.kill();
    assert!(p.is_zombie(), "a zombie stays a zombie until reaped");
    assert_eq!(p.exit_code(), 5, "exit code is preserved");
}

#[test]
fn reaped_is_a_terminal_sink() {
    let mut p = Process::__create(1);
    p.make_ready();
    p.exit(0);
    p.reap();
    // Everything is ignored in the terminal sink.
    p.make_ready();
    p.kill();
    p.reap();
    assert!(p.is_reaped());
    assert!(!p.is_zombie());
}

#[test]
fn pid_is_independent_of_state() {
    let mut p = Process::__create(123);
    assert_eq!(p.pid(), 123);
    p.make_ready();
    assert_eq!(p.pid(), 123);
    p.exit(1);
    assert_eq!(p.pid(), 123);
    p.reap();
    assert_eq!(p.pid(), 123);
}

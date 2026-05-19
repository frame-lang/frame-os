// shell/tests/job_behavior.rs
//
// Level 3 (behavioral) tests for the Job Frame system.
//
// Job tracks one OS process from spawn through its eventual exit or
// kill. These tests construct Job directly (via `Job::__create(id)` —
// Frame system params are passed to the constructor), spawn real
// children via std::process::Command, and assert on the state graph
// transitions.
//
// Most tests are Unix-only because they invoke POSIX-specific binaries
// (/bin/sleep, /usr/bin/true). The Job FSM itself compiles on
// Windows but stop/resume are no-ops there (`#[cfg(unix)]` gates in the
// .frs actions).
//
// Test commands chosen for portability + speed:
//   /usr/bin/true     — exits immediately with code 0
//   /usr/bin/false    — exits immediately with code 1
//   /bin/sleep N  — runs for N seconds; used to keep a job alive
//                       long enough to test stop/kill transitions
//
// Why poll() in a sleep loop: try_wait is non-blocking, so we have to
// retry until the child actually exits. Real shell driving will be
// done by JobControl in H3 Step 2 and won't poll-loop in tests.

#![cfg(unix)]

use frame_os_shell::Job;
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const POLL_TIMEOUT: Duration = Duration::from_secs(3);

/// Drive Job.poll() in a loop until the job reaches $Done or we time out.
/// Returns true if the job became $Done before timeout.
fn wait_done(job: &mut Job) -> bool {
    let start = Instant::now();
    while !job.is_done() {
        if start.elapsed() > POLL_TIMEOUT {
            return false;
        }
        job.poll();
        thread::sleep(POLL_INTERVAL);
    }
    true
}

// ---------------------------------------------------------------------------
// Construction & initial state
// ---------------------------------------------------------------------------

#[test]
fn job_starts_in_created() {
    let mut job = Job::__create(7);
    assert!(!job.is_done(), "fresh job is in $Created, not $Done");
    assert_eq!(job.id(), 7, "constructor id propagates to domain");
    assert_eq!(job.pid(), 0, "no PID yet — nothing spawned");
    assert_eq!(job.state_name(), "Pending");
}

// ---------------------------------------------------------------------------
// spawn() — happy path and failure
// ---------------------------------------------------------------------------

#[test]
fn spawn_transitions_to_foreground_with_running_state() {
    let mut job = Job::__create(1);
    job.spawn("/bin/sleep".to_string(), vec!["0.5".to_string()]);
    assert!(!job.is_done());
    assert!(job.pid() > 0, "PID should be set after spawn");
    assert_eq!(job.state_name(), "Running");
    // Clean up — kill the sleep so it doesn't outlive the test.
    job.kill();
    let _ = wait_done(&mut job);
}

#[test]
fn spawn_failure_jumps_to_done_with_error() {
    let mut job = Job::__create(2);
    job.spawn("/this/binary/definitely/does/not/exist".to_string(), vec![]);
    assert!(job.is_done(), "spawn failure should land directly in $Done");
    assert!(
        job.state_name().starts_with("Failed"),
        "state should report failure, got: {}",
        job.state_name()
    );
}

// ---------------------------------------------------------------------------
// poll() — natural exit detection
// ---------------------------------------------------------------------------

#[test]
fn poll_after_immediate_exit_transitions_to_done_with_code_zero() {
    let mut job = Job::__create(3);
    job.spawn("/usr/bin/true".to_string(), vec![]);
    assert!(wait_done(&mut job), "/usr/bin/true should exit promptly");
    assert_eq!(job.exit_code(), 0);
    assert_eq!(job.state_name(), "Done");
}

#[test]
fn poll_after_nonzero_exit_surfaces_code() {
    let mut job = Job::__create(4);
    job.spawn("/usr/bin/false".to_string(), vec![]);
    assert!(wait_done(&mut job));
    assert_eq!(job.exit_code(), 1, "/usr/bin/false exits with code 1");
}

#[test]
fn poll_in_foreground_returns_false_for_long_running() {
    let mut job = Job::__create(5);
    job.spawn("/bin/sleep".to_string(), vec!["10".to_string()]);
    // Several polls; child won't exit during these.
    for _ in 0..3 {
        job.poll();
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!job.is_done(), "sleep 10 should still be running");
    assert_eq!(job.state_name(), "Running");
    job.kill();
    let _ = wait_done(&mut job);
}

// ---------------------------------------------------------------------------
// kill() — terminal transition from any non-done state
// ---------------------------------------------------------------------------

#[test]
fn kill_in_foreground_transitions_to_done() {
    let mut job = Job::__create(6);
    job.spawn("/bin/sleep".to_string(), vec!["30".to_string()]);
    assert!(!job.is_done());
    job.kill();
    assert!(wait_done(&mut job), "after kill the job should reach $Done");
}

#[test]
fn kill_in_done_is_idempotent() {
    let mut job = Job::__create(7);
    job.spawn("/usr/bin/true".to_string(), vec![]);
    assert!(wait_done(&mut job));
    // Already done; kill should not panic or change state.
    job.kill();
    assert!(job.is_done());
}

// ---------------------------------------------------------------------------
// stop() / resume — SIGTSTP and SIGCONT
// ---------------------------------------------------------------------------

#[test]
fn stop_in_foreground_transitions_to_stopped() {
    let mut job = Job::__create(8);
    job.spawn("/bin/sleep".to_string(), vec!["30".to_string()]);
    job.stop();
    assert_eq!(
        job.state_name(),
        "Stopped",
        "after stop() should be in $Stopped"
    );
    // Cleanup: kill the stopped job.
    job.kill();
    let _ = wait_done(&mut job);
}

#[test]
fn to_foreground_from_stopped_resumes_into_foreground() {
    let mut job = Job::__create(9);
    job.spawn("/bin/sleep".to_string(), vec!["30".to_string()]);
    job.stop();
    assert_eq!(job.state_name(), "Stopped");
    job.to_foreground();
    assert_eq!(
        job.state_name(),
        "Running",
        "after to_foreground from $Stopped should be back in $Foreground"
    );
    job.kill();
    let _ = wait_done(&mut job);
}

#[test]
fn to_background_from_stopped_resumes_into_background() {
    let mut job = Job::__create(10);
    job.spawn("/bin/sleep".to_string(), vec!["30".to_string()]);
    job.stop();
    job.to_background();
    assert_eq!(job.state_name(), "Running");
    job.kill();
    let _ = wait_done(&mut job);
}

// ---------------------------------------------------------------------------
// to_background() / to_foreground() — context switch without stop
// ---------------------------------------------------------------------------

#[test]
fn to_background_from_foreground_changes_state() {
    let mut job = Job::__create(11);
    job.spawn("/bin/sleep".to_string(), vec!["30".to_string()]);
    job.to_background();
    // Both $Foreground and $Background report state_name "Running" — the
    // distinction is internal. Verify is_done is still false and we can
    // transition further from here.
    assert!(!job.is_done());
    job.to_foreground();
    assert!(
        !job.is_done(),
        "transitioned back to foreground; still running"
    );
    job.kill();
    let _ = wait_done(&mut job);
}

// ---------------------------------------------------------------------------
// $Done — terminal state
// ---------------------------------------------------------------------------

#[test]
fn done_state_ignores_all_lifecycle_events() {
    let mut job = Job::__create(12);
    job.spawn("/usr/bin/true".to_string(), vec![]);
    assert!(wait_done(&mut job));

    // All these should be no-ops; nothing should change state, panic,
    // or otherwise misbehave.
    let final_state = job.state_name();
    let final_code = job.exit_code();
    job.stop();
    job.to_foreground();
    job.to_background();
    job.kill();
    job.poll();
    assert!(job.is_done());
    assert_eq!(job.state_name(), final_state);
    assert_eq!(job.exit_code(), final_code);
}

// ---------------------------------------------------------------------------
// cmd_str() — display formatting
// ---------------------------------------------------------------------------

#[test]
fn cmd_str_handles_no_args() {
    let mut job = Job::__create(13);
    job.spawn("/usr/bin/true".to_string(), vec![]);
    assert_eq!(job.cmd_str(), "/usr/bin/true");
    let _ = wait_done(&mut job);
}

#[test]
fn cmd_str_joins_args_with_spaces() {
    let mut job = Job::__create(14);
    job.spawn("/bin/sleep".to_string(), vec!["0".to_string()]);
    assert_eq!(job.cmd_str(), "/bin/sleep 0");
    let _ = wait_done(&mut job);
}

#[test]
fn id_propagates_through_lifecycle() {
    let mut job = Job::__create(99);
    assert_eq!(job.id(), 99);
    job.spawn("/usr/bin/true".to_string(), vec![]);
    assert_eq!(job.id(), 99);
    let _ = wait_done(&mut job);
    assert_eq!(job.id(), 99);
}

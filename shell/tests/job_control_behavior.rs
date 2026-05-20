// shell/tests/job_control_behavior.rs
//
// Level 3 (behavioral) tests for the JobControl Frame system.
//
// JobControl holds Vec<Job> and routes per-job operations through its
// interface. These tests drive it via the public methods and assert on
// the published state via is_idle() / is_running_foreground() / jobs().
//
// Unix-only because the underlying Job FSM uses POSIX binaries and
// signal delivery.

#![cfg(unix)]

use frame_os_shell::JobControl;
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Initial state
// ---------------------------------------------------------------------------

#[test]
fn fresh_job_control_is_idle_with_empty_job_list() {
    let mut jc = JobControl::__create();
    assert!(jc.is_idle());
    assert!(!jc.is_running_foreground());
    assert!(jc.jobs().is_empty());
    assert_eq!(jc.next_job_id(), 1);
    assert_eq!(jc.foreground_id(), 0);
}

// ---------------------------------------------------------------------------
// spawn_foreground — happy path and failure
// ---------------------------------------------------------------------------

#[test]
fn spawn_foreground_transitions_to_running_foreground() {
    let mut jc = JobControl::__create();
    jc.spawn_foreground("/bin/sleep".to_string(), vec!["10".to_string()]);
    assert!(!jc.is_idle());
    assert!(jc.is_running_foreground());
    assert_eq!(jc.foreground_id(), 1);
    assert_eq!(jc.jobs().len(), 1);
    // Cleanup
    jc.kill_job(1);
    jc.wait_foreground();
}

#[test]
fn spawn_foreground_failure_lands_in_idle_after_wait() {
    let mut jc = JobControl::__create();
    jc.spawn_foreground("/this/binary/does/not/exist".to_string(), vec![]);
    // Always transitions to $RunningForeground (even on spawn failure).
    assert!(jc.is_running_foreground());
    // wait_foreground sees the failed Job is already $Done on the first
    // poll iteration and returns immediately.
    jc.wait_foreground();
    assert!(jc.is_idle());
    let summaries = jc.jobs();
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].state.starts_with("Failed"));
}

#[test]
fn spawn_foreground_zero_exit_completes_via_wait_foreground() {
    let mut jc = JobControl::__create();
    jc.spawn_foreground("/usr/bin/true".to_string(), vec![]);
    assert!(jc.is_running_foreground());
    jc.wait_foreground();
    assert!(jc.is_idle(), "after foreground completes, back to $Idle");
    let summaries = jc.jobs();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].state, "Done");
}

// ---------------------------------------------------------------------------
// spawn_background — never leaves $Idle
// ---------------------------------------------------------------------------

#[test]
fn spawn_background_stays_idle_and_adds_to_job_list() {
    let mut jc = JobControl::__create();
    jc.spawn_background("/bin/sleep".to_string(), vec!["10".to_string()]);
    assert!(jc.is_idle());
    assert_eq!(jc.jobs().len(), 1);
    assert_eq!(jc.foreground_id(), 0, "no foreground assigned");
    // Cleanup
    jc.kill_job(1);
    jc.tick();
    thread::sleep(Duration::from_millis(50));
    jc.tick();
}

#[test]
fn multiple_background_jobs_get_distinct_ids() {
    let mut jc = JobControl::__create();
    jc.spawn_background("/bin/sleep".to_string(), vec!["10".to_string()]);
    jc.spawn_background("/bin/sleep".to_string(), vec!["10".to_string()]);
    jc.spawn_background("/bin/sleep".to_string(), vec!["10".to_string()]);
    let summaries = jc.jobs();
    assert_eq!(summaries.len(), 3);
    assert_eq!(summaries[0].id, 1);
    assert_eq!(summaries[1].id, 2);
    assert_eq!(summaries[2].id, 3);
    assert_eq!(jc.next_job_id(), 4);
    // Cleanup
    for id in 1..=3 {
        jc.kill_job(id);
    }
}

// ---------------------------------------------------------------------------
// fg / bg
// ---------------------------------------------------------------------------

#[test]
fn fg_brings_background_job_to_foreground() {
    let mut jc = JobControl::__create();
    jc.spawn_background("/bin/sleep".to_string(), vec!["10".to_string()]);
    assert!(jc.is_idle());
    jc.fg(1);
    assert!(jc.is_running_foreground());
    assert_eq!(jc.foreground_id(), 1);
    // Cleanup
    jc.kill_job(1);
    jc.wait_foreground();
}

#[test]
fn fg_with_nonexistent_id_stays_idle() {
    let mut jc = JobControl::__create();
    jc.fg(99);
    assert!(jc.is_idle());
}

#[test]
fn fg_with_done_job_does_not_transition() {
    let mut jc = JobControl::__create();
    jc.spawn_foreground("/usr/bin/true".to_string(), vec![]);
    jc.wait_foreground();
    // Job 1 is now Done. fg(1) should NOT bring it back.
    jc.fg(1);
    assert!(jc.is_idle());
}

#[test]
fn bg_resumes_stopped_job_in_background() {
    let mut jc = JobControl::__create();
    jc.spawn_foreground("/bin/sleep".to_string(), vec!["10".to_string()]);
    jc.stop_foreground();
    assert!(jc.is_idle());
    // The job is in $Stopped. Resume it in the background.
    jc.bg(1);
    assert!(jc.is_idle());
    // Job should now be Running (in background) again.
    let summaries = jc.jobs();
    assert_eq!(summaries[0].state, "Running");
    // Cleanup
    jc.kill_job(1);
    jc.tick();
    thread::sleep(Duration::from_millis(50));
    jc.tick();
}

// ---------------------------------------------------------------------------
// stop_foreground — SIGTSTP path
// ---------------------------------------------------------------------------

#[test]
fn stop_foreground_transitions_to_idle_with_job_stopped() {
    let mut jc = JobControl::__create();
    jc.spawn_foreground("/bin/sleep".to_string(), vec!["10".to_string()]);
    jc.stop_foreground();
    assert!(jc.is_idle(), "stop_foreground returns to $Idle");
    let summaries = jc.jobs();
    assert_eq!(summaries[0].state, "Stopped");
    // Cleanup
    jc.kill_job(1);
    jc.tick();
    thread::sleep(Duration::from_millis(50));
    jc.tick();
}

#[test]
fn stop_foreground_in_idle_is_noop() {
    let mut jc = JobControl::__create();
    jc.stop_foreground();
    assert!(jc.is_idle());
}

// ---------------------------------------------------------------------------
// kill_job — works from either state
// ---------------------------------------------------------------------------

#[test]
fn kill_job_in_idle_kills_background_job() {
    let mut jc = JobControl::__create();
    jc.spawn_background("/bin/sleep".to_string(), vec!["10".to_string()]);
    jc.kill_job(1);
    // Reap
    for _ in 0..50 {
        jc.tick();
        if jc.jobs()[0].state == "Done" {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let summaries = jc.jobs();
    assert_eq!(summaries[0].state, "Done");
}

#[test]
fn kill_job_of_foreground_transitions_to_idle() {
    let mut jc = JobControl::__create();
    jc.spawn_foreground("/bin/sleep".to_string(), vec!["10".to_string()]);
    assert!(jc.is_running_foreground());
    jc.kill_job(1);
    assert!(jc.is_idle());
}

// ---------------------------------------------------------------------------
// wait_for — block on a specific job
// ---------------------------------------------------------------------------

#[test]
fn wait_for_blocks_until_specified_background_job_done() {
    let mut jc = JobControl::__create();
    jc.spawn_background("/usr/bin/true".to_string(), vec![]);
    jc.wait_for(1);
    let summaries = jc.jobs();
    assert_eq!(summaries[0].state, "Done");
}

#[test]
fn wait_for_nonexistent_id_returns_immediately() {
    let mut jc = JobControl::__create();
    // No jobs; wait_for finds nothing and bails. Doesn't hang.
    jc.wait_for(99);
    assert!(jc.is_idle());
}

// ---------------------------------------------------------------------------
// tick — reap done background jobs
// ---------------------------------------------------------------------------

#[test]
fn tick_reaps_completed_background_jobs() {
    let mut jc = JobControl::__create();
    jc.spawn_background("/usr/bin/true".to_string(), vec![]);
    // /usr/bin/true exits immediately; tick a few times until reaped.
    for _ in 0..50 {
        jc.tick();
        if jc.jobs()[0].state == "Done" {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(jc.jobs()[0].state, "Done");
}

// ---------------------------------------------------------------------------
// jobs() summary listing
// ---------------------------------------------------------------------------

#[test]
fn jobs_summary_includes_id_state_and_cmd() {
    let mut jc = JobControl::__create();
    jc.spawn_background("/bin/sleep".to_string(), vec!["10".to_string()]);
    let summaries = jc.jobs();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, 1);
    assert_eq!(summaries[0].cmd, "/bin/sleep 10");
    assert_eq!(summaries[0].state, "Running");
    // Cleanup
    jc.kill_job(1);
}

#[test]
fn jobs_summary_reflects_mixed_states() {
    let mut jc = JobControl::__create();
    // Job 1: long-running background
    jc.spawn_background("/bin/sleep".to_string(), vec!["10".to_string()]);
    // Job 2: stopped foreground
    jc.spawn_foreground("/bin/sleep".to_string(), vec!["10".to_string()]);
    jc.stop_foreground();
    // Job 3: completed
    jc.spawn_background("/usr/bin/true".to_string(), vec![]);
    for _ in 0..50 {
        jc.tick();
        if jc.jobs()[2].state == "Done" {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let summaries = jc.jobs();
    assert_eq!(summaries.len(), 3);
    assert_eq!(summaries[0].state, "Running");
    assert_eq!(summaries[1].state, "Stopped");
    assert_eq!(summaries[2].state, "Done");

    // Cleanup
    jc.kill_job(1);
    jc.kill_job(2);
}

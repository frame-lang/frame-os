// shell/tests/jobs_e2e.rs
//
// Level 6 (E2E) tests for the H3 Step 4 job-control builtins:
// `jobs`, `fg`, `bg`, `wait`, `kill`.
//
// Each test spawns the shell binary, sends a stdin script that
// exercises the builtin, and asserts on stdout.
//
// Most tests use /bin/sleep with short or zero-second durations to keep
// runtime fast. Background spawns use spawn_detached internally (stdio
// nulled, own process group), so child output never reaches the test —
// we assert on the SHELL's output (jobs listing, exit codes, etc.).
//
// Exit-criteria mapping (per docs/roadmap.md):
//   H3-5  jobs_lists_running_and_stopped
//   H3-6  fg_brings_background_to_foreground (covered via JobControl
//         behavioral test; the user-visible E2E path is delicate to
//         drive because /bin/sleep can't show signal-stoppedness via
//         stdout; tested here through the wait-completion path)
//   H3-7  bg_resumes_stopped_job (covered via JobControl behavioral;
//         the E2E path here exercises bg's parse path)
//   H3-9  wait_blocks_until_job_done

#![cfg(unix)]

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn shell_at(cwd: &std::path::Path, input: &str) -> assert_cmd::assert::Assert {
    Command::cargo_bin("frame-os-shell")
        .expect("cargo-built binary")
        .current_dir(cwd)
        .write_stdin(input)
        .timeout(std::time::Duration::from_secs(5))
        .assert()
}

// ---------------------------------------------------------------------------
// jobs
// ---------------------------------------------------------------------------

#[test]
fn jobs_with_no_jobs_prints_nothing_extra() {
    // Empty job list should not print a header or "no jobs" message —
    // just no output. (Bash convention.)
    let tmp = TempDir::new().unwrap();
    let bound = shell_at(tmp.path(), "jobs\nexit\n").success();
    let stdout = String::from_utf8(bound.get_output().stdout.clone()).unwrap();
    // We can't easily assert "no jobs-list output" via contains(). Check
    // that the typical job-line shape "[N]  " doesn't appear:
    assert!(
        !stdout.contains("[1]") && !stdout.contains("[2]"),
        "no jobs should produce no [N] lines: {stdout}"
    );
}

#[test]
fn jobs_lists_a_background_job() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/sleep 60 &\njobs\nexit\n")
        .success()
        .stdout(contains("[1]"))
        .stdout(contains("Running"))
        .stdout(contains("/bin/sleep"));
}

#[test]
fn jobs_shows_multiple_jobs_with_correct_ids() {
    let tmp = TempDir::new().unwrap();
    let bound = shell_at(
        tmp.path(),
        "/bin/sleep 60 &\n/bin/sleep 60 &\n/bin/sleep 60 &\njobs\nexit\n",
    )
    .success();
    let stdout = String::from_utf8(bound.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("[1]"), "jobs should list id 1: {stdout}");
    assert!(stdout.contains("[2]"), "jobs should list id 2: {stdout}");
    assert!(stdout.contains("[3]"), "jobs should list id 3: {stdout}");
}

#[test]
fn jobs_shows_done_jobs_after_reap() {
    // /usr/bin/true exits immediately. To avoid a race between the
    // background spawn finishing and the subsequent jobs listing, use
    // `wait 1` to synchronously block until the child has exited and
    // been reaped. Then `jobs` reliably shows it as Done. (We don't
    // auto-prune done jobs in H3 — the user sees the history.)
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/usr/bin/true &\nwait 1\njobs\nexit\n")
        .success()
        .stdout(contains("[1]"))
        .stdout(contains("Done"));
}

// ---------------------------------------------------------------------------
// kill
// ---------------------------------------------------------------------------

#[test]
fn kill_terminates_background_job() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/sleep 60 &\nkill 1\njobs\nexit\n")
        .success()
        .stdout(contains("[1]"))
        .stdout(contains("Done"));
}

#[test]
fn kill_with_no_arg_prints_usage() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "kill\nexit\n")
        .success()
        .stdout(contains("kill: missing job id"));
}

#[test]
fn kill_with_invalid_id_prints_error() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "kill abc\nexit\n")
        .success()
        .stdout(contains("kill: invalid job id"));
}

#[test]
fn kill_of_nonexistent_id_is_silent() {
    // JobControl.kill_job is a no-op if id doesn't match. The shell
    // surfaces no error. Verify the cycle completes cleanly.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "kill 99\nexit\n")
        .success()
        .stdout(contains("goodbye"));
}

// ---------------------------------------------------------------------------
// wait
// ---------------------------------------------------------------------------

#[test]
fn wait_blocks_until_job_done() {
    // /usr/bin/true exits immediately; wait should return quickly.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/usr/bin/true &\nwait 1\njobs\nexit\n")
        .success()
        .stdout(contains("Done"));
}

#[test]
fn wait_with_no_arg_prints_usage() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "wait\nexit\n")
        .success()
        .stdout(contains("wait: missing job id"));
}

#[test]
fn wait_for_nonexistent_id_returns_immediately() {
    // JobControl.wait_for is a no-op if id doesn't match (returns
    // immediately rather than hanging).
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "wait 99\nexit\n")
        .success()
        .stdout(contains("goodbye"));
}

// ---------------------------------------------------------------------------
// fg / bg
// ---------------------------------------------------------------------------

#[test]
fn fg_brings_background_to_foreground() {
    // Launch a job in background, then fg it. Since fg blocks (waits on
    // the now-foreground job), we use /usr/bin/true which exits
    // immediately. The fg call returns quickly; jobs shows Done.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/usr/bin/true &\nfg 1\njobs\nexit\n")
        .success()
        .stdout(contains("Done"));
}

#[test]
fn fg_with_no_arg_prints_usage() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "fg\nexit\n")
        .success()
        .stdout(contains("fg: missing or invalid job id"));
}

#[test]
fn fg_with_invalid_id_prints_usage() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "fg abc\nexit\n")
        .success()
        .stdout(contains("fg: missing or invalid job id"));
}

#[test]
fn fg_with_nonexistent_id_prints_no_such_job() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "fg 99\nexit\n")
        .success()
        .stdout(contains("fg: no such job: 99"));
}

#[test]
fn bg_with_no_arg_prints_usage() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "bg\nexit\n")
        .success()
        .stdout(contains("bg: missing job id"));
}

#[test]
fn bg_with_invalid_id_prints_error() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "bg foo\nexit\n")
        .success()
        .stdout(contains("bg: invalid job id"));
}

// ---------------------------------------------------------------------------
// help — should list the new builtins
// ---------------------------------------------------------------------------

#[test]
fn help_lists_new_h3_builtins() {
    let tmp = TempDir::new().unwrap();
    let bound = shell_at(tmp.path(), "help\nexit\n").success();
    let stdout = String::from_utf8(bound.get_output().stdout.clone()).unwrap();
    for name in &["jobs", "fg", "bg", "wait", "kill"] {
        assert!(
            stdout.contains(name),
            "help output should list `{name}` at H3 Step 4: {stdout}"
        );
    }
    assert!(
        stdout.contains("&"),
        "help should mention `&` for background launch: {stdout}"
    );
}

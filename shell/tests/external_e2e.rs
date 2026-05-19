// shell/tests/external_e2e.rs
//
// Level 6 (E2E) tests for H2's $RunningExternal state — non-builtin input
// is routed to std::process::Command via the shell's run_external action.
//
// Exit-criteria mapping (per docs/roadmap.md):
//   H2-2  external_command_runs_via_state_machine (uses /bin/echo to bypass
//         the builtin `echo`)
//   H2-3  external_command_stdout_passes_through
//   H2-4  external_command_nonzero_exit_surfaces_code
//   H2-5  nonexistent_command_prints_not_found
//
// What we do NOT cover automatically:
//   - Ctrl-C in $RunningExternal kills the child (H2-7). Sending SIGINT
//     to a spawned subprocess from assert_cmd isn't supported; the
//     mechanism is documented in shell.frs and covered by code review.
//     A future framework-tests refactor can drive this via direct
//     `nix::sys::signal::kill(Pid::from_raw(shell_pid), SIGINT)`.
//
// Most tests are Unix-only because they invoke POSIX-specific binaries
// (/bin/echo, /bin/false, /bin/sh). Windows coverage lands when the H2
// Windows path is built out (currently the SIG_IGN handler is gated on
// cfg(unix); Windows builds compile but interrupt-during-external isn't
// validated).

#![cfg(unix)]

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
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

#[test]
fn external_command_stdout_passes_through() {
    // /bin/echo is the POSIX echo binary. Using the absolute path
    // sidesteps Frame OS's builtin `echo` — the Shell sees "/bin/echo"
    // (not a known builtin name), classifies as Unknown, routes to
    // $RunningExternal, spawns /bin/echo, and the child's stdout flows
    // through to ours.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/echo hello from external\nexit\n")
        .success()
        .stdout(contains("hello from external"));
}

#[test]
fn external_command_zero_exit_no_extra_output() {
    // /usr/bin/true exits 0 and produces no output. Shell shouldn't add
    // an [exit code: ...] line on success.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/usr/bin/true\nexit\n")
        .success()
        .stdout(contains("goodbye"))
        .stdout(predicates::str::contains("exit code").not());
}

#[test]
fn external_command_nonzero_exit_surfaces_code() {
    // /usr/bin/false exits 1. Shell should print "[exit code: 1]".
    // /usr/bin is POSIX-portable; /bin/false exists on Linux but macOS
    // puts it only under /usr/bin.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/usr/bin/false\nexit\n")
        .success()
        .stdout(contains("[exit code: 1]"));
}

#[test]
fn external_command_specific_exit_code_surfaced() {
    // Run a shell-out with a deterministic non-zero exit. Coverage that
    // the surfaced code matches the child's actual exit value, not just
    // "non-zero."
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/sh -c 'exit 7'\nexit\n")
        .success()
        .stdout(contains("[exit code: 7]"));
}

#[test]
fn nonexistent_command_prints_not_found() {
    // Spawn fails with ErrorKind::NotFound; shell prints "<cmd>: command
    // not found" and returns to $Prompting without crashing.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "frame-os-totally-not-a-real-command\nexit\n")
        .success()
        .stdout(contains("frame-os-totally-not-a-real-command"))
        .stdout(contains("command not found"));
}

#[test]
fn external_command_then_builtin_still_works() {
    // After an external command runs, the shell must return cleanly to
    // $Prompting and process subsequent builtins. This is the round-trip
    // check that $RunningExternal → $Prompting transition fires.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/echo first\necho second\nexit\n")
        .success()
        .stdout(contains("first"))
        .stdout(contains("second"));
}

#[test]
fn multiple_externals_in_sequence() {
    let tmp = TempDir::new().unwrap();
    shell_at(
        tmp.path(),
        "/bin/echo one\n/bin/echo two\n/bin/echo three\nexit\n",
    )
    .success()
    .stdout(contains("one"))
    .stdout(contains("two"))
    .stdout(contains("three"));
}

// shell/tests/e2e.rs
//
// Level 6 (hosted-shell end-to-end) tests for Frame OS.
//
// These spawn the `frame-os-shell` binary as a subprocess, feed it input via
// stdin, and assert on stdout/stderr/exit code. They verify user-visible
// behavior, not the internals of the state machine.
//
// What we test here:
//   - The binary launches and prints its banner
//   - Typing 'exit' produces 'goodbye' and exits 0
//   - Typing 'quit' produces 'goodbye' and exits 0
//   - Closing stdin (EOF) exits cleanly
//   - Unknown commands produce the "unknown command" message
//
// What we DON'T test here:
//   - Internal state transitions (covered by shell_behavior.rs)
//   - Generated state graphs (covered by state_graphs.rs)

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

/// Helper: build a command that runs the shell with the given stdin.
fn shell_with_input(input: &str) -> assert_cmd::assert::Assert {
    Command::cargo_bin("frame-os-shell")
        .expect("cargo-built frame-os-shell binary should exist")
        .write_stdin(input)
        .timeout(std::time::Duration::from_secs(5))
        .assert()
}

#[test]
fn prints_banner_on_startup() {
    shell_with_input("exit\n")
        .success()
        .stdout(contains("Frame OS shell"));
}

#[test]
fn prints_prompt() {
    shell_with_input("exit\n")
        .success()
        .stdout(contains("frame-os>"));
}

#[test]
fn exit_command_exits_cleanly() {
    shell_with_input("exit\n")
        .success()
        .stdout(contains("goodbye"));
}

#[test]
fn quit_command_exits_cleanly() {
    shell_with_input("quit\n")
        .success()
        .stdout(contains("goodbye"));
}

#[test]
fn eof_exits_cleanly() {
    // No input at all → immediate EOF → shell should exit 0.
    shell_with_input("").success().stdout(contains("goodbye"));
}

#[test]
fn unknown_command_prints_message() {
    shell_with_input("xyzzy\nexit\n")
        .success()
        .stdout(contains("unknown command"))
        .stdout(contains("xyzzy"));
}

#[test]
fn empty_lines_dont_crash() {
    // Repeated empty input should not produce an unknown-command message.
    shell_with_input("\n\n\nexit\n")
        .success()
        .stdout(contains("goodbye"))
        // We didn't run any commands, so there should be no unknown-command line.
        .stdout(predicates::str::contains("unknown command").not());
}

#[test]
fn multiple_commands_before_exit() {
    shell_with_input("foo\nbar\nbaz\nexit\n")
        .success()
        .stdout(contains("foo"))
        .stdout(contains("bar"))
        .stdout(contains("baz"))
        .stdout(contains("goodbye"));
}

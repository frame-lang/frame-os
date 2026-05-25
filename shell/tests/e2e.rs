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
//   - Closing stdin (EOF / Ctrl-D) exits cleanly
//   - Unknown input is routed to $RunningExternal which surfaces a
//     "command not found" message from the OS path lookup (H2 behavior;
//     H1 had this as a Builtin::Unknown print)
//
// What we DON'T test here:
//   - Internal state transitions (covered by shell_behavior.rs)
//   - Generated state graphs (covered by state_graphs.rs)
//   - Per-builtin behavior (covered by builtins_e2e.rs)
//   - External-command execution success path (covered by external_e2e.rs)

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
    // Piped (non-TTY) input: the Shell's print_prompt() emits "$> " to stdout
    // (on a TTY, rustyline renders it instead — see shell/src/main.rs).
    shell_with_input("exit\n").success().stdout(contains("$>"));
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
fn unknown_command_prints_not_found() {
    // H2: unknown input flows through $Parsing → $RunningExternal which
    // tries to spawn `xyzzy`; spawn fails with NotFound and the shell
    // prints "xyzzy: command not found". H1's "unknown command: xyzzy"
    // message is replaced by this OS-shaped form.
    shell_with_input("xyzzy\nexit\n")
        .success()
        .stdout(contains("xyzzy"))
        .stdout(contains("command not found"));
}

#[test]
fn empty_lines_dont_crash() {
    // Repeated empty input should not produce any command-related output.
    shell_with_input("\n\n\nexit\n")
        .success()
        .stdout(contains("goodbye"))
        // No spawn attempts, so no "command not found" lines.
        .stdout(predicates::str::contains("command not found").not());
}

#[test]
fn multiple_known_builtins_before_exit() {
    // Run several known builtins (no subprocess spawns) and confirm we
    // cycle through to exit cleanly.
    shell_with_input("pwd\nhelp\necho hello\nexit\n")
        .success()
        .stdout(contains("hello"))
        .stdout(contains("goodbye"));
}

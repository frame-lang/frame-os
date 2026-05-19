// shell/tests/shell_behavior.rs
//
// Level 3 (behavioral) tests for the Shell Frame system.
//
// These tests construct the Shell directly and exercise its interface
// methods. They verify the committed state-event pairs:
//
//   $Prompting + line("exit")    → $Exiting       (is_done becomes true)
//   $Prompting + line("quit")    → $Exiting       (is_done becomes true)
//   $Prompting + line("")        → stays in $Prompting (is_done stays false)
//   $Prompting + line("xyzzy")   → stays in $Prompting (unknown-command path)
//   $Prompting + interrupt()     → $Exiting       (Ctrl-C / Ctrl-D path)
//   $Exiting   + line(anything)  → stays in $Exiting   (is_done remains true)
//   $Exiting   + interrupt()     → stays in $Exiting   (already done)
//
// What we DON'T test here:
//   - Output text (prompt formatting, goodbye message). That's E2E territory
//     and lives in shell/tests/e2e.rs.
//   - Generated state graph structure. That's a snapshot test and lives in
//     shell/tests/state_graphs.rs.

use frame_os_shell::Shell;

#[test]
fn shell_starts_not_done() {
    let mut shell = Shell::__create();
    assert!(
        !shell.is_done(),
        "fresh Shell should be in $Prompting, not $Exiting"
    );
}

#[test]
fn exit_command_transitions_to_exiting() {
    let mut shell = Shell::__create();
    shell.line("exit");
    assert!(shell.is_done(), "after 'exit', Shell should be in $Exiting");
}

#[test]
fn quit_command_transitions_to_exiting() {
    let mut shell = Shell::__create();
    shell.line("quit");
    assert!(shell.is_done(), "after 'quit', Shell should be in $Exiting");
}

#[test]
fn exit_with_trailing_newline_works() {
    // The host loop passes lines with the trailing newline still attached.
    // The Shell trims internally.
    let mut shell = Shell::__create();
    shell.line("exit\n");
    assert!(
        shell.is_done(),
        "'exit\\n' should be treated the same as 'exit'"
    );
}

#[test]
fn exit_with_surrounding_whitespace_works() {
    let mut shell = Shell::__create();
    shell.line("   exit   ");
    assert!(
        shell.is_done(),
        "'  exit  ' should be treated the same as 'exit'"
    );
}

#[test]
fn empty_line_does_not_exit() {
    let mut shell = Shell::__create();
    shell.line("");
    assert!(!shell.is_done(), "empty line should stay in $Prompting");
}

#[test]
fn whitespace_only_line_does_not_exit() {
    let mut shell = Shell::__create();
    shell.line("   \t  ");
    assert!(
        !shell.is_done(),
        "whitespace-only line should stay in $Prompting"
    );
}

#[test]
fn unknown_command_does_not_exit() {
    let mut shell = Shell::__create();
    shell.line("xyzzy");
    assert!(
        !shell.is_done(),
        "unknown command should stay in $Prompting"
    );
}

#[test]
fn exiting_state_ignores_further_lines() {
    let mut shell = Shell::__create();
    shell.line("exit");
    assert!(shell.is_done(), "should be exiting");

    // Further input shouldn't take us back out of $Exiting.
    shell.line("anything");
    shell.line("exit");
    shell.line("");
    assert!(shell.is_done(), "Shell in $Exiting must stay in $Exiting");
}

#[test]
fn interrupt_in_prompting_transitions_to_exiting() {
    // H0 success criterion: Ctrl-C exits cleanly. The host loop maps
    // ReadlineError::Interrupted to shell.interrupt(); we test the
    // transition here without involving rustyline.
    let mut shell = Shell::__create();
    shell.interrupt();
    assert!(
        shell.is_done(),
        "interrupt in $Prompting should transition to $Exiting"
    );
}

#[test]
fn interrupt_in_exiting_is_idempotent() {
    let mut shell = Shell::__create();
    shell.line("exit");
    assert!(shell.is_done(), "should be in $Exiting");

    // A second interrupt (e.g. user mashes Ctrl-C while shutdown is in
    // progress) must not panic, regress state, or otherwise misbehave.
    shell.interrupt();
    assert!(
        shell.is_done(),
        "still in $Exiting after redundant interrupt"
    );
}

#[test]
fn interrupt_after_unknown_commands_still_exits() {
    let mut shell = Shell::__create();
    shell.line("foo");
    shell.line("bar");
    assert!(!shell.is_done(), "should still be in $Prompting");
    shell.interrupt();
    assert!(
        shell.is_done(),
        "interrupt after unknown commands should exit"
    );
}

#[test]
fn many_unknown_commands_before_exit() {
    // Stress-ish: confirm we can stay in $Prompting indefinitely.
    let mut shell = Shell::__create();
    for cmd in ["foo", "bar", "baz", "", "  ", "what", "help"] {
        shell.line(cmd);
        assert!(!shell.is_done(), "should still be prompting after '{cmd}'");
    }
    shell.line("exit");
    assert!(shell.is_done());
}

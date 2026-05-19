// shell/tests/shell_behavior.rs
//
// Level 3 (behavioral) tests for the Shell Frame system.
//
// These tests construct the Shell directly and exercise its interface
// methods. They verify the committed state-event pairs.
//
// H2 semantic changes (inverted from H0/H1):
//   - interrupt() in $Prompting STAYS in $Prompting (was: → $Exiting).
//     The "abort this input" interpretation; rustyline cleared the line
//     and we just re-prompt.
//   - The new eof() event is what transitions $Prompting → $Exiting
//     (Ctrl-D path); host loop routes ReadlineError::Eof here.
//
// What we DON'T test here:
//   - Output text (prompt formatting, goodbye message). That's E2E territory.
//   - Generated state graph structure. That's a snapshot test.
//   - External-command execution. That's E2E in builtins_e2e.rs and
//     external_e2e.rs; running subprocesses is slow and not the point of
//     behavioral tests.

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
fn interrupt_in_prompting_stays_prompting() {
    // H2: Ctrl-C at the prompt is "abort this input" — Frame stays in
    // $Prompting. The host loop's main.rs maps ReadlineError::Interrupted
    // to shell.interrupt(); rustyline has already cleared the line buffer
    // by the time we get here.
    let mut shell = Shell::__create();
    shell.interrupt();
    assert!(
        !shell.is_done(),
        "interrupt in $Prompting should stay in $Prompting (not transition to $Exiting)"
    );
}

#[test]
fn eof_in_prompting_transitions_to_exiting() {
    // H2: Ctrl-D / EOF is what transitions to $Exiting. Host loop maps
    // ReadlineError::Eof to shell.eof().
    let mut shell = Shell::__create();
    shell.eof();
    assert!(
        shell.is_done(),
        "eof in $Prompting should transition to $Exiting"
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
fn eof_in_exiting_is_idempotent() {
    let mut shell = Shell::__create();
    shell.line("exit");
    assert!(shell.is_done());
    shell.eof();
    assert!(shell.is_done(), "redundant eof in $Exiting is a no-op");
}

#[test]
fn interrupt_repeats_at_prompting_dont_exit() {
    // Stress: Ctrl-C several times in a row should never accidentally exit.
    let mut shell = Shell::__create();
    for _ in 0..5 {
        shell.interrupt();
        assert!(!shell.is_done());
    }
    shell.eof();
    assert!(shell.is_done(), "eof eventually exits cleanly");
}

#[test]
fn many_known_builtins_before_exit() {
    // Stress: stay in $Prompting through many known-builtin invocations.
    // (Unknown commands would now spawn subprocesses via $RunningExternal,
    // which is slow and not the point of behavioral tests — we use known
    // builtins here so the cycle is `$Prompting → $Parsing → $RunningBuiltin
    // → $Prompting` purely in-process.)
    let mut shell = Shell::__create();
    for cmd in ["pwd", "help", "echo foo", "echo bar", "history"] {
        shell.line(cmd);
        assert!(!shell.is_done(), "should still be prompting after '{cmd}'");
    }
    shell.line("exit");
    assert!(shell.is_done());
}

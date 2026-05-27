// shell/tests/pipeline_e2e.rs
//
// Level 6 (E2E) tests for M1 (H↔B parity): pipes and I/O redirection driven
// end-to-end through the shell binary. The Parser tags operators, the Pipeline
// FSM folds them into a command pipeline, and shell/src/exec.rs runs it.
//
// Exit-criteria intent (the hosted side of H↔B parity — same user-visible
// behavior the bare-metal `ish` already has):
//   - `cmd > f` / `cmd >> f`        output redirection (builtins + external)
//   - `cmd < f`                     input redirection (external)
//   - `a | b`                       pipelines (external stages)
//   - syntax errors surfaced, shell stays alive
//
// Unix-only: the tests invoke POSIX absolute-path binaries (/bin/echo,
// /bin/cat) and builtin output redirection uses a Unix fd redirect.

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

// ---------------------------------------------------------------------------
// Output redirection — builtins (Unix fd redirect)
// ---------------------------------------------------------------------------

#[test]
fn builtin_echo_output_redirection_writes_file() {
    // `echo` is a hosted builtin. `echo hi > f` must still write the file
    // (user-visible parity with ish, where echo is /bin/echo). The fd-redirect
    // guard repoints stdout at the file around execute().
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "echo hello-redir > out.txt\nexit\n").success();
    let body = std::fs::read_to_string(tmp.path().join("out.txt")).unwrap();
    assert_eq!(body, "hello-redir\n");
}

#[test]
fn builtin_redirected_output_does_not_reach_terminal() {
    // The redirected output goes to the file, NOT to the shell's stdout.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "echo secret-line > out.txt\nexit\n")
        .success()
        .stdout(contains("secret-line").not());
}

#[test]
fn builtin_append_redirection_appends() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "echo first > f\necho second >> f\nexit\n").success();
    let body = std::fs::read_to_string(tmp.path().join("f")).unwrap();
    assert_eq!(body, "first\nsecond\n");
}

#[test]
fn builtin_truncate_redirection_overwrites() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "echo one > f\necho two > f\nexit\n").success();
    let body = std::fs::read_to_string(tmp.path().join("f")).unwrap();
    assert_eq!(body, "two\n");
}

#[test]
fn builtin_pwd_redirection_writes_cwd() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "pwd > where.txt\nexit\n").success();
    let body = std::fs::read_to_string(tmp.path().join("where.txt")).unwrap();
    // pwd prints the shell's cwd (the tmp dir) followed by a newline.
    assert!(body
        .trim()
        .ends_with(tmp.path().file_name().unwrap().to_str().unwrap()));
}

// ---------------------------------------------------------------------------
// Output / input redirection — external commands
// ---------------------------------------------------------------------------

#[test]
fn external_output_redirection_writes_file() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/echo ext-redir > out.txt\nexit\n").success();
    let body = std::fs::read_to_string(tmp.path().join("out.txt")).unwrap();
    assert_eq!(body, "ext-redir\n");
}

#[test]
fn external_input_redirection_reads_file() {
    // Write a file with the builtin echo, then feed it to /bin/cat via `< f`.
    // /bin/cat reads stdin (the builtin cat reads an arg, not stdin — input
    // redirection is meaningful for external commands).
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "echo piped-in > f\n/bin/cat < f\nexit\n")
        .success()
        .stdout(contains("piped-in"));
}

// ---------------------------------------------------------------------------
// Pipelines
// ---------------------------------------------------------------------------

#[test]
fn two_stage_pipeline_passes_data() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/echo piped-output | /bin/cat\nexit\n")
        .success()
        .stdout(contains("piped-output"));
}

#[test]
fn three_stage_pipeline_passes_data() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/echo abc | /bin/cat | /bin/cat\nexit\n")
        .success()
        .stdout(contains("abc"));
}

#[test]
fn pipeline_with_output_redirection_on_last_stage() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/echo to-file | /bin/cat > out.txt\nexit\n").success();
    let body = std::fs::read_to_string(tmp.path().join("out.txt")).unwrap();
    assert!(body.contains("to-file"));
}

// ---------------------------------------------------------------------------
// Syntax errors — shell surfaces them and stays alive
// ---------------------------------------------------------------------------

#[test]
fn trailing_pipe_is_a_syntax_error() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "ls |\nexit\n")
        .success()
        .stdout(contains("syntax error near '|'"))
        .stdout(contains("goodbye")); // shell survived to process `exit`
}

#[test]
fn missing_redirection_target_is_a_syntax_error() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/echo hi >\nexit\n")
        .success()
        .stdout(contains("missing redirection target"))
        .stdout(contains("goodbye"));
}

#[test]
fn quoted_pipe_is_not_an_operator() {
    // A quoted "|" is a literal argument, not the pipe operator — so this is a
    // single (external) command whose arg is "|", not a pipeline.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "/bin/echo \"a | b\"\nexit\n")
        .success()
        .stdout(contains("a | b"));
}

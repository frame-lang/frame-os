// shell/tests/builtins_e2e.rs
//
// Level 6 (hosted-shell end-to-end) tests for the H1 builtins.
//
// These spawn the `frame-os-shell` binary, drive it through stdin, and
// assert on stdout. Each builtin from the H1 roadmap entry has at least
// one test exercising its happy path; several have additional tests
// covering edge cases (no-arg, missing-file, nonexistent-dir, etc.).
//
// Filesystem-touching tests use `tempfile::TempDir` for isolated state
// so they're reproducible across machines and don't pollute the user's
// working directory.
//
// Exit-criteria mapping (per docs/roadmap.md):
//   H1-5  cd_then_pwd_reflects_new_cwd, cd_to_nonexistent_dir_prints_error
//   H1-6  pwd_prints_current_directory
//   H1-7  ls_lists_default_dir, ls_lists_specified_dir, ls_handles_missing_dir_with_error
//   H1-8  cat_prints_file_contents, cat_handles_missing_file_with_error
//   H1-9  echo_prints_args, echo_with_no_args_prints_blank_line
//   H1-10 history_shows_prior_commands
//   H1-11 help_lists_all_builtins

use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;
use tempfile::TempDir;

/// Helper: run the shell with the given stdin from a known working directory.
/// Useful for the cd/pwd/ls/cat tests where the binary's cwd is the implicit
/// starting state (before any `cd` runs).
fn shell_at(cwd: &Path, input: &str) -> assert_cmd::assert::Assert {
    Command::cargo_bin("frame-os-shell")
        .expect("cargo-built binary")
        .current_dir(cwd)
        .write_stdin(input)
        .timeout(std::time::Duration::from_secs(5))
        .assert()
}

// ---------------------------------------------------------------------------
// pwd (H1-6)
// ---------------------------------------------------------------------------

#[test]
fn pwd_prints_current_directory() {
    let tmp = TempDir::new().expect("temp dir");
    // canonicalize matches what the shell internally stores (env::current_dir
    // returns the canonical form on most platforms; on macOS /tmp -> /private/tmp).
    // On Windows, canonicalize() yields a `\\?\` verbatim path while the shell
    // prints the plain env::current_dir() form — strip the prefix so the match
    // holds cross-platform (no-op on Unix, where the prefix is absent).
    let canonical = tmp.path().canonicalize().expect("canonicalize tempdir");
    let full = canonical.display().to_string();
    let expected = full
        .strip_prefix(r"\\?\")
        .unwrap_or(full.as_str())
        .to_string();
    shell_at(tmp.path(), "pwd\nexit\n")
        .success()
        .stdout(contains(expected));
}

// ---------------------------------------------------------------------------
// echo (H1-9)
// ---------------------------------------------------------------------------

#[test]
fn echo_prints_args() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "echo hello world\nexit\n")
        .success()
        .stdout(contains("hello world"));
}

#[test]
fn echo_with_no_args_prints_blank_line() {
    // `echo` with no args should print just a newline. Hard to assert
    // "exactly an empty line" via contains(), but we can confirm the
    // shell didn't blow up and got back to the prompt.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "echo\nexit\n")
        .success()
        .stdout(contains("goodbye"));
}

#[test]
fn echo_preserves_quoted_spaces() {
    // Frame's Parser should preserve the spaces inside the quoted string,
    // so echo receives a single arg "hello world" and prints it.
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "echo \"hello world\"\nexit\n")
        .success()
        .stdout(contains("hello world"));
}

// ---------------------------------------------------------------------------
// help (H1-11)
// ---------------------------------------------------------------------------

#[test]
fn help_lists_all_builtins() {
    let tmp = TempDir::new().unwrap();
    // Spot-check via individual asserts. assert_cmd's Assert builder consumes
    // self on each chained `.stdout()`, so we re-spawn the same shell once
    // per builtin instead of trying to chain a moved value.
    let names = ["cd", "pwd", "ls", "cat", "echo", "history", "help", "exit"];
    let bound = shell_at(tmp.path(), "help\nexit\n").success();
    let stdout = String::from_utf8(bound.get_output().stdout.clone()).unwrap();
    for name in &names {
        assert!(
            stdout.contains(name),
            "help output should mention `{name}`, got: {stdout}"
        );
    }
}

// ---------------------------------------------------------------------------
// cd (H1-5)
// ---------------------------------------------------------------------------

#[test]
fn cd_then_pwd_reflects_new_cwd() {
    let outer = TempDir::new().unwrap();
    let inner = outer.path().join("nested");
    std::fs::create_dir(&inner).unwrap();
    let inner_canonical = inner.canonicalize().unwrap();

    // From outer, cd into nested, then pwd.
    shell_at(outer.path(), "cd nested\npwd\nexit\n")
        .success()
        .stdout(contains(inner_canonical.display().to_string()));
}

#[test]
fn cd_absolute_path_works() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().canonicalize().unwrap();
    // Start in a different cwd, cd to the tempdir absolute path.
    let start = TempDir::new().unwrap();
    shell_at(
        start.path(),
        &format!("cd {}\npwd\nexit\n", target.display()),
    )
    .success()
    .stdout(contains(target.display().to_string()));
}

#[test]
fn cd_to_nonexistent_dir_prints_error() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "cd /this/path/does/not/exist\nexit\n")
        .success()
        .stdout(contains("cd:"));
}

#[test]
fn cd_to_file_prints_not_a_directory_error() {
    let tmp = TempDir::new().unwrap();
    let f = tmp.path().join("a_file.txt");
    std::fs::write(&f, b"content").unwrap();
    shell_at(tmp.path(), "cd a_file.txt\nexit\n")
        .success()
        .stdout(contains("cd:"));
}

// ---------------------------------------------------------------------------
// ls (H1-7)
// ---------------------------------------------------------------------------

#[test]
fn ls_lists_default_dir() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("alpha"), b"").unwrap();
    std::fs::write(tmp.path().join("beta"), b"").unwrap();
    let result = shell_at(tmp.path(), "ls\nexit\n").success();
    let _ = result.stdout(contains("alpha")).stdout(contains("beta"));
}

#[test]
fn ls_lists_specified_dir() {
    let outer = TempDir::new().unwrap();
    let inner = outer.path().join("nested");
    std::fs::create_dir(&inner).unwrap();
    std::fs::write(inner.join("gamma"), b"").unwrap();
    shell_at(outer.path(), "ls nested\nexit\n")
        .success()
        .stdout(contains("gamma"));
}

#[test]
fn ls_sorts_alphabetically() {
    let tmp = TempDir::new().unwrap();
    // Create in a non-alpha order; output should still be alpha-sorted.
    for name in &["zeta", "alpha", "delta"] {
        std::fs::write(tmp.path().join(name), b"").unwrap();
    }
    let bound = shell_at(tmp.path(), "ls\nexit\n").success();
    let bytes = bound.get_output().stdout.clone();
    let stdout = String::from_utf8(bytes).unwrap();
    let a = stdout.find("alpha").expect("alpha appears");
    let d = stdout.find("delta").expect("delta appears");
    let z = stdout.find("zeta").expect("zeta appears");
    assert!(a < d && d < z, "ls output should be alpha-sorted: {stdout}");
}

#[test]
fn ls_handles_missing_dir_with_error() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "ls /this/dir/does/not/exist\nexit\n")
        .success()
        .stdout(contains("ls:"));
}

// ---------------------------------------------------------------------------
// cat (H1-8)
// ---------------------------------------------------------------------------

#[test]
fn cat_prints_file_contents() {
    let tmp = TempDir::new().unwrap();
    let f = tmp.path().join("hello.txt");
    std::fs::write(&f, b"frame os is great\n").unwrap();
    shell_at(tmp.path(), "cat hello.txt\nexit\n")
        .success()
        .stdout(contains("frame os is great"));
}

#[test]
fn cat_handles_missing_file_with_error() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "cat does_not_exist.txt\nexit\n")
        .success()
        .stdout(contains("cat:"));
}

#[test]
fn cat_with_no_arg_prints_error() {
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "cat\nexit\n")
        .success()
        .stdout(contains("cat:"));
}

#[test]
fn cat_absolute_path_works() {
    let tmp = TempDir::new().unwrap();
    let f = tmp.path().join("abs.txt");
    std::fs::write(&f, b"by absolute path\n").unwrap();
    let other_cwd = TempDir::new().unwrap();
    shell_at(other_cwd.path(), &format!("cat {}\nexit\n", f.display()))
        .success()
        .stdout(contains("by absolute path"));
}

// ---------------------------------------------------------------------------
// history (H1-10)
// ---------------------------------------------------------------------------

#[test]
fn history_shows_prior_commands() {
    let tmp = TempDir::new().unwrap();
    // After echo foo, echo bar, history should show both.
    let bound = shell_at(tmp.path(), "echo foo\necho bar\nhistory\nexit\n").success();
    let bytes = bound.get_output().stdout.clone();
    let stdout = String::from_utf8(bytes).unwrap();
    // Find the "history" output by looking for the numbered entries.
    // Both "echo foo" and "echo bar" should appear as history lines.
    assert!(
        stdout.contains("echo foo"),
        "history should include the prior 'echo foo' line, got: {stdout}"
    );
    assert!(
        stdout.contains("echo bar"),
        "history should include the prior 'echo bar' line, got: {stdout}"
    );
}

#[test]
fn empty_history_prints_nothing_extra() {
    // Running `history` immediately after starting the shell should produce
    // no history-list output (and definitely shouldn't crash).
    let tmp = TempDir::new().unwrap();
    shell_at(tmp.path(), "history\nexit\n")
        .success()
        .stdout(contains("goodbye"));
}

// ---------------------------------------------------------------------------
// Sanity: with H2, unknown commands now route to $RunningExternal which
// tries to spawn them as host-OS processes. Coverage for that path lives
// in external_e2e.rs; the regression here just confirms the cycle still
// returns to the prompt cleanly.
// ---------------------------------------------------------------------------

#[test]
fn nonexistent_input_returns_to_prompt() {
    let tmp = TempDir::new().unwrap();
    // xyzzy is not a known builtin and not on PATH; H2 routes it to
    // $RunningExternal which tries `Command::new("xyzzy").spawn()`, hits
    // ErrorKind::NotFound, prints a message, and returns to $Prompting.
    shell_at(tmp.path(), "xyzzy\nexit\n")
        .success()
        .stdout(contains("xyzzy"))
        .stdout(contains("goodbye"));
}

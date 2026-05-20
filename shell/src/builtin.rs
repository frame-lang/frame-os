// shell/src/builtin.rs
//
// Builtin commands for the H1 hosted shell.
//
// A Builtin is a classified piece of user input — the result of Parser
// tokenizing a line, mapped to a known builtin variant or
// Builtin::Unknown. Shell's $Parsing state calls classify(); $RunningBuiltin
// calls execute().
//
// The Shell state machine doesn't know what each builtin does — that's
// native data manipulation, the kind of work architecture.md assigns to
// Rust, not Frame. Shell only decides *when* execute() runs (in
// $RunningBuiltin's enter handler, between $Parsing and the return to
// $Prompting). This split keeps the Frame system focused on lifecycle
// dispatch and leaves the data work to ordinary Rust.

use crate::JobControl;
use std::path::{Path, PathBuf};

/// A classified user command, ready for execution.
///
/// `Empty` is the explicit no-op variant for "user typed whitespace
/// only" — the classifier returns `Empty` rather than panicking on an
/// empty token list.
///
/// `Unknown(name, args)` carries the original command and is routed by
/// Shell.$Parsing.$> to JobControl.spawn_foreground or
/// JobControl.spawn_background (depending on trailing `&`) at H2+.
///
/// `Jobs`/`Fg`/`Bg`/`Wait`/`Kill` arrived at H3 Step 4. They take an
/// `Option<String>` for the job id arg so `execute_*` can parse and
/// surface usage errors uniformly.
#[derive(Debug, Clone)]
pub enum Builtin {
    Cd(Option<String>),
    Pwd,
    Ls(Option<String>),
    Cat(Option<String>),
    Echo(Vec<String>),
    History,
    Help,
    Jobs,
    Fg(Option<String>),
    Bg(Option<String>),
    Wait(Option<String>),
    Kill(Option<String>),
    Empty,
    Unknown(String, Vec<String>),
}

/// True iff the classified command was not matched against any known builtin.
/// Used by `$Parsing.$>` to route to `$RunningForeground` (H3) or to
/// JobControl.spawn_background, instead of `$RunningBuiltin`.
pub fn is_unknown(b: &Builtin) -> bool {
    matches!(b, Builtin::Unknown(_, _))
}

/// Extract the cmd and args from `Builtin::Unknown(cmd, args)`. Callers
/// should gate with `is_unknown(...)` first; otherwise this returns an
/// empty pair, which JobControl will treat as a spawn failure.
pub fn unknown_parts(b: &Builtin) -> (String, Vec<String>) {
    match b {
        Builtin::Unknown(c, a) => (c.clone(), a.clone()),
        _ => (String::new(), Vec::new()),
    }
}

/// Classify a token vector into a Builtin.
///
/// Tokens come from Parser. The first token is the command name; the rest
/// are arguments. Commands not matching a known builtin become
/// `Unknown(name, args)`.
pub fn classify(tokens: Vec<String>) -> Builtin {
    let mut iter = tokens.into_iter();
    let cmd = match iter.next() {
        Some(c) => c,
        None => return Builtin::Empty,
    };
    let args: Vec<String> = iter.collect();
    let first_arg = || args.first().cloned();
    match cmd.as_str() {
        "cd" => Builtin::Cd(first_arg()),
        "pwd" => Builtin::Pwd,
        "ls" => Builtin::Ls(first_arg()),
        "cat" => Builtin::Cat(first_arg()),
        "echo" => Builtin::Echo(args),
        "history" => Builtin::History,
        "help" => Builtin::Help,
        "jobs" => Builtin::Jobs,
        "fg" => Builtin::Fg(first_arg()),
        "bg" => Builtin::Bg(first_arg()),
        "wait" => Builtin::Wait(first_arg()),
        "kill" => Builtin::Kill(first_arg()),
        _ => Builtin::Unknown(cmd, args),
    }
}

/// True iff this builtin is the foreground-resuming kind that needs Shell
/// to transition into `$RunningForeground` to wait on it. Currently just
/// `Fg`. Used by Shell.$Parsing.$> to gate the special-case routing for
/// resumed-foreground builtins; everything else flows through `execute()`
/// and the normal `$RunningBuiltin` → `$Prompting` cycle.
pub fn is_fg(b: &Builtin) -> bool {
    matches!(b, Builtin::Fg(_))
}

/// Execute a Builtin.
///
/// `cwd` is mutable because `Cd` updates it. `history` is read-only —
/// appending happens in Shell's `$RunningBuiltin.$>` after `execute()`
/// returns. `job_control` is mutable because the H3 Step 4 builtins
/// (`jobs`, `bg`, `wait`, `kill`) operate against it. `Fg` is NOT
/// dispatched here — it has its own path in Shell.$Parsing.$> because
/// it needs to transition Shell to `$RunningForeground`.
pub fn execute(
    builtin: &Builtin,
    cwd: &mut PathBuf,
    history: &[String],
    job_control: &mut JobControl,
) {
    match builtin {
        Builtin::Cd(path) => execute_cd(path.as_deref(), cwd),
        Builtin::Pwd => println!("{}", cwd.display()),
        Builtin::Ls(path) => execute_ls(path.as_deref(), cwd),
        Builtin::Cat(path) => execute_cat(path.as_deref(), cwd),
        Builtin::Echo(args) => println!("{}", args.join(" ")),
        Builtin::History => execute_history(history),
        Builtin::Help => execute_help(),
        Builtin::Jobs => execute_jobs(job_control),
        Builtin::Bg(arg) => execute_bg(arg.as_deref(), job_control),
        Builtin::Wait(arg) => execute_wait(arg.as_deref(), job_control),
        Builtin::Kill(arg) => execute_kill(arg.as_deref(), job_control),
        Builtin::Empty => {
            // Nothing to do. Reached when the user input parsed to zero
            // tokens (e.g. whitespace-only that somehow got past the
            // empty-trim check in $Prompting). Documented as a deliberate
            // no-op so future readers don't add an error path.
        }
        Builtin::Fg(_) => {
            // Unreachable: $Parsing.$> special-cases Builtin::Fg before
            // routing through $RunningBuiltin. It calls JobControl.fg(id)
            // directly and either transitions to $RunningForeground or
            // prints a usage error and stays $Prompting. If we got here,
            // something is mis-wired.
            unreachable!("Builtin::Fg should be handled by Shell.$Parsing.$>, not execute()");
        }
        Builtin::Unknown(_, _) => {
            unreachable!("Builtin::Unknown should be handled by $RunningForeground, not execute()");
        }
    }
}

/// Resolve a user-supplied path against `cwd`. Absolute paths are returned
/// as-is; relative paths are joined onto `cwd`. The result is not yet
/// canonicalized — callers do that when they need it.
fn resolve(path: &str, cwd: &Path) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

fn execute_cd(arg: Option<&str>, cwd: &mut PathBuf) {
    let target = match arg {
        None => match std::env::var("HOME") {
            Ok(home) => PathBuf::from(home),
            Err(_) => {
                println!("cd: HOME not set");
                return;
            }
        },
        Some(p) => resolve(p, cwd),
    };
    match target.canonicalize() {
        Ok(canonical) if canonical.is_dir() => {
            *cwd = canonical;
        }
        Ok(_) => {
            println!("cd: not a directory: {}", target.display());
        }
        Err(e) => {
            println!("cd: {}: {e}", target.display());
        }
    }
}

fn execute_ls(arg: Option<&str>, cwd: &Path) {
    let dir = match arg {
        None => cwd.to_path_buf(),
        Some(p) => resolve(p, cwd),
    };
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            for name in names {
                println!("{name}");
            }
        }
        Err(e) => {
            println!("ls: {}: {e}", dir.display());
        }
    }
}

fn execute_cat(arg: Option<&str>, cwd: &Path) {
    let path = match arg {
        None => {
            println!("cat: missing file argument");
            return;
        }
        Some(p) => resolve(p, cwd),
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            // Print verbatim; don't add a trailing newline if the file
            // doesn't have one. Shells differ here — bash cats with no
            // implicit newline. Match that.
            print!("{contents}");
        }
        Err(e) => {
            println!("cat: {}: {e}", path.display());
        }
    }
}

fn execute_history(history: &[String]) {
    // Numbered list, 1-based, right-aligned 4-wide. Matches bash's `history`
    // output shape roughly. The current `history` command itself is NOT in
    // the list — Shell appends to history AFTER execute() returns, so each
    // invocation sees only the lines that completed before it.
    for (i, line) in history.iter().enumerate() {
        // line is the raw input as typed (including trailing newline if
        // any); trim for display.
        println!("{:4}  {}", i + 1, line.trim_end());
    }
}

fn execute_help() {
    // Keep this list in sync with the Builtin enum variants. Order is
    // navigation → listing → reading → output → history → job control → meta.
    println!("Available commands:");
    println!("  cd [path]      change directory (defaults to $HOME)");
    println!("  pwd            print current working directory");
    println!("  ls [path]      list directory contents (sorted)");
    println!("  cat <file>     print file contents");
    println!("  echo <args...> print arguments separated by spaces");
    println!("  history        show command history");
    println!("  jobs           list running, stopped, and completed jobs");
    println!("  fg <id>        bring job <id> to the foreground");
    println!("  bg <id>        resume stopped job <id> in the background");
    println!("  wait <id>      block until job <id> completes");
    println!("  kill <id>      SIGKILL job <id>");
    println!("  help           show this list");
    println!("  exit | quit    leave the shell");
    println!();
    println!("Append `&` to any external command to launch it in the background.");
}

fn execute_jobs(job_control: &mut JobControl) {
    let summaries = job_control.jobs();
    if summaries.is_empty() {
        return;
    }
    for s in summaries.iter() {
        // bash-ish layout: "[N]  State  cmd". State column is fixed-width
        // for alignment across rows.
        println!("[{}]  {:<10}  {}", s.id, s.state, s.cmd);
    }
}

/// Parse a "fg/bg/wait/kill <id>" argument string. Returns the id on
/// success; prints a usage/error message and returns None on failure.
fn parse_job_id(arg: Option<&str>, builtin_name: &str) -> Option<u32> {
    let raw = match arg {
        Some(s) => s,
        None => {
            println!("{builtin_name}: missing job id");
            return None;
        }
    };
    match raw.parse::<u32>() {
        Ok(id) => Some(id),
        Err(_) => {
            println!("{builtin_name}: invalid job id: {raw}");
            None
        }
    }
}

fn execute_bg(arg: Option<&str>, job_control: &mut JobControl) {
    let id = match parse_job_id(arg, "bg") {
        Some(id) => id,
        None => return,
    };
    job_control.bg(id);
    // JobControl.bg is a no-op if id doesn't exist or is Done. We can't
    // distinguish "resumed" from "no-op" without checking; for H3 minimum
    // we don't bother — user can run `jobs` to see the current state.
}

fn execute_wait(arg: Option<&str>, job_control: &mut JobControl) {
    let id = match parse_job_id(arg, "wait") {
        Some(id) => id,
        None => return,
    };
    job_control.wait_for(id);
}

fn execute_kill(arg: Option<&str>, job_control: &mut JobControl) {
    let id = match parse_job_id(arg, "kill") {
        Some(id) => id,
        None => return,
    };
    job_control.kill_job(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn classifies_empty_tokens_as_empty() {
        assert!(matches!(classify(Vec::new()), Builtin::Empty));
    }

    #[test]
    fn classifies_pwd() {
        assert!(matches!(classify(toks(&["pwd"])), Builtin::Pwd));
    }

    #[test]
    fn classifies_history() {
        assert!(matches!(classify(toks(&["history"])), Builtin::History));
    }

    #[test]
    fn classifies_help() {
        assert!(matches!(classify(toks(&["help"])), Builtin::Help));
    }

    #[test]
    fn classifies_cd_with_path() {
        match classify(toks(&["cd", "/tmp"])) {
            Builtin::Cd(Some(p)) => assert_eq!(p, "/tmp"),
            other => panic!("expected Cd(Some), got {other:?}"),
        }
    }

    #[test]
    fn classifies_cd_with_no_arg() {
        assert!(matches!(classify(toks(&["cd"])), Builtin::Cd(None)));
    }

    #[test]
    fn classifies_ls_with_arg() {
        match classify(toks(&["ls", "/etc"])) {
            Builtin::Ls(Some(p)) => assert_eq!(p, "/etc"),
            other => panic!("expected Ls(Some), got {other:?}"),
        }
    }

    #[test]
    fn classifies_ls_no_arg() {
        assert!(matches!(classify(toks(&["ls"])), Builtin::Ls(None)));
    }

    #[test]
    fn classifies_cat_with_arg() {
        match classify(toks(&["cat", "file.txt"])) {
            Builtin::Cat(Some(p)) => assert_eq!(p, "file.txt"),
            other => panic!("expected Cat(Some), got {other:?}"),
        }
    }

    #[test]
    fn classifies_echo_with_args() {
        match classify(toks(&["echo", "hello", "world"])) {
            Builtin::Echo(args) => assert_eq!(args, vec!["hello", "world"]),
            other => panic!("expected Echo, got {other:?}"),
        }
    }

    #[test]
    fn classifies_echo_with_no_args() {
        match classify(toks(&["echo"])) {
            Builtin::Echo(args) => assert!(args.is_empty()),
            other => panic!("expected Echo, got {other:?}"),
        }
    }

    #[test]
    fn classifies_unknown_with_args() {
        match classify(toks(&["xyzzy", "a", "b"])) {
            Builtin::Unknown(name, args) => {
                assert_eq!(name, "xyzzy");
                assert_eq!(args, vec!["a", "b"]);
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    // H3 Step 4 — job-control builtin variants

    #[test]
    fn classifies_jobs() {
        assert!(matches!(classify(toks(&["jobs"])), Builtin::Jobs));
    }

    #[test]
    fn classifies_fg_with_id() {
        match classify(toks(&["fg", "3"])) {
            Builtin::Fg(Some(id)) => assert_eq!(id, "3"),
            other => panic!("expected Fg(Some), got {other:?}"),
        }
    }

    #[test]
    fn classifies_fg_no_arg() {
        assert!(matches!(classify(toks(&["fg"])), Builtin::Fg(None)));
    }

    #[test]
    fn classifies_bg_with_id() {
        match classify(toks(&["bg", "2"])) {
            Builtin::Bg(Some(id)) => assert_eq!(id, "2"),
            other => panic!("expected Bg(Some), got {other:?}"),
        }
    }

    #[test]
    fn classifies_wait_with_id() {
        match classify(toks(&["wait", "5"])) {
            Builtin::Wait(Some(id)) => assert_eq!(id, "5"),
            other => panic!("expected Wait(Some), got {other:?}"),
        }
    }

    #[test]
    fn classifies_kill_with_id() {
        match classify(toks(&["kill", "1"])) {
            Builtin::Kill(Some(id)) => assert_eq!(id, "1"),
            other => panic!("expected Kill(Some), got {other:?}"),
        }
    }

    #[test]
    fn is_fg_distinguishes_fg_from_other_builtins() {
        assert!(is_fg(&Builtin::Fg(Some("1".to_string()))));
        assert!(is_fg(&Builtin::Fg(None)));
        assert!(!is_fg(&Builtin::Bg(None)));
        assert!(!is_fg(&Builtin::Jobs));
        assert!(!is_fg(&Builtin::Pwd));
        assert!(!is_fg(&Builtin::Unknown("x".to_string(), vec![])));
    }
}

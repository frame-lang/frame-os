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

use std::path::{Path, PathBuf};

/// A classified user command, ready for execution.
///
/// `Empty` is the explicit no-op variant for "user typed whitespace
/// only" — the classifier returns `Empty` rather than panicking on an
/// empty token list.
///
/// `Unknown` carries the original command name (for the error message)
/// and the args. At H1 it just prints "unknown command: <name>". At H2
/// it will hand `(name, args)` to `std::process::Command` for external
/// execution.
#[derive(Debug, Clone)]
pub enum Builtin {
    Cd(Option<String>),
    Pwd,
    Ls(Option<String>),
    Cat(Option<String>),
    Echo(Vec<String>),
    History,
    Help,
    Empty,
    Unknown(String, Vec<String>),
}

/// True iff the classified command was not matched against any known builtin.
/// Used by `$Parsing.$>` to route to `$RunningExternal` instead of
/// `$RunningBuiltin`. Lifted out as a function so the .frs handler body
/// stays a single-line condition.
pub fn is_unknown(b: &Builtin) -> bool {
    matches!(b, Builtin::Unknown(_, _))
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
        _ => Builtin::Unknown(cmd, args),
    }
}

/// Execute a Builtin.
///
/// `cwd` is mutable because `Cd` updates it. `history` is read-only
/// because the `History` builtin only displays it — appending happens
/// in Shell's `$RunningBuiltin.$>` after `execute()` returns.
pub fn execute(builtin: &Builtin, cwd: &mut PathBuf, history: &[String]) {
    match builtin {
        Builtin::Cd(path) => execute_cd(path.as_deref(), cwd),
        Builtin::Pwd => println!("{}", cwd.display()),
        Builtin::Ls(path) => execute_ls(path.as_deref(), cwd),
        Builtin::Cat(path) => execute_cat(path.as_deref(), cwd),
        Builtin::Echo(args) => println!("{}", args.join(" ")),
        Builtin::History => execute_history(history),
        Builtin::Help => execute_help(),
        Builtin::Empty => {
            // Nothing to do. Reached when the user input parsed to zero
            // tokens (e.g. whitespace-only that somehow got past the
            // empty-trim check in $Prompting). Documented as a deliberate
            // no-op so future readers don't add an error path.
        }
        Builtin::Unknown(_, _) => {
            // Unreachable: $Parsing routes Builtin::Unknown to
            // $RunningExternal at H2, which handles it via
            // Shell::run_external(). The dispatcher here is the
            // $RunningBuiltin code path, so this arm should never be hit.
            // Kept as a defensive panic so a future refactor that
            // accidentally routes Unknown back through $RunningBuiltin
            // surfaces immediately.
            unreachable!("Builtin::Unknown should be handled by $RunningExternal, not execute()");
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
    // Keep this list in sync with the Builtin enum variants. Order is the
    // order a reader is likely to need them: navigation, listing, reading,
    // output, history, meta.
    println!("Available commands:");
    println!("  cd [path]      change directory (defaults to $HOME)");
    println!("  pwd            print current working directory");
    println!("  ls [path]      list directory contents (sorted)");
    println!("  cat <file>     print file contents");
    println!("  echo <args...> print arguments separated by spaces");
    println!("  history        show command history");
    println!("  help           show this list");
    println!("  exit | quit    leave the shell");
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
}

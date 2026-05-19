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
//
// H1 Step 2 (this commit): structure only. execute() is stub-only —
// each variant prints a "(todo: <name>)" placeholder. Step 3 fills in
// the real behavior, one builtin at a time, each with its own E2E test.
// The interface for execute() stabilizes here so Step 3 is purely
// per-builtin work.

use std::path::PathBuf;

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
/// in Shell's `$RunningBuiltin.$>` after `execute()` returns, so each
/// invocation can see every prior line including the one just executed.
///
/// H1 Step 2: stubs. Step 3 fills these in.
pub fn execute(builtin: &Builtin, _cwd: &mut PathBuf, _history: &[String]) {
    match builtin {
        Builtin::Cd(_) => println!("(todo: cd)"),
        Builtin::Pwd => println!("(todo: pwd)"),
        Builtin::Ls(_) => println!("(todo: ls)"),
        Builtin::Cat(_) => println!("(todo: cat)"),
        Builtin::Echo(_) => println!("(todo: echo)"),
        Builtin::History => println!("(todo: history)"),
        Builtin::Help => println!("(todo: help)"),
        Builtin::Empty => {
            // Nothing to do. Reached when the user input parsed to zero
            // tokens (e.g. whitespace-only that somehow got past the
            // empty-trim check in $Prompting). Documented as a deliberate
            // no-op so future readers don't add an error path.
        }
        Builtin::Unknown(cmd, _) => {
            println!("unknown command: {cmd} (try 'exit')");
        }
    }
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

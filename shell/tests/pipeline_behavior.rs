// shell/tests/pipeline_behavior.rs
//
// Level 3 (behavioral) tests for the Pipeline Frame system.
//
// Pipeline folds the Parser's token stream into an executable command pipeline
// (commands joined by `|`, each with optional `< > >>` redirection, plus an
// optional trailing `&`). These tests drive it via consume(kind, text) +
// finalize() and assert on commands() / is_background() / error().
//
// Every committed state-event pair has at least one test:
//
//   $ReadingCommand + Word            — push word, stays
//   $ReadingCommand + < / > / >>      — → $ExpectingTarget (record pending)
//   $ReadingCommand + |               — commit + start new command (or error)
//   $ReadingCommand + &               — set background, → $TrailingAmp
//   $ReadingCommand + finalize()      — commit, → $Done (or trailing-| error)
//
//   $ExpectingTarget + Word           — set redir target, → $ReadingCommand
//   $ExpectingTarget + operator       — → $Error
//   $ExpectingTarget + finalize()     — → $Error (missing target)
//
//   $TrailingAmp + any token          — → $Error
//   $TrailingAmp + finalize()         — commit, → $Done

use frame_os_shell::{Command, Pipeline, Token, TokenKind};

/// Split a Parser `Token` into the (kind, text) pair the Pipeline consumes.
/// This is exactly the glue the Shell uses when wiring Parser → Pipeline.
fn split(t: Token) -> (TokenKind, String) {
    match t {
        Token::Word(w) => (TokenKind::Word, w),
        Token::Pipe => (TokenKind::Pipe, String::new()),
        Token::RedirIn => (TokenKind::RedirIn, String::new()),
        Token::RedirOut => (TokenKind::RedirOut, String::new()),
        Token::RedirAppend => (TokenKind::RedirAppend, String::new()),
        Token::Amp => (TokenKind::Amp, String::new()),
    }
}

/// Feed a slice of typed tokens through a fresh Pipeline and finalize.
fn run(tokens: &[Token]) -> Pipeline {
    let mut p = Pipeline::__create();
    for t in tokens {
        let (kind, text) = split(t.clone());
        p.consume(kind, text);
    }
    p.finalize();
    p
}

fn word(s: &str) -> Token {
    Token::Word(s.to_string())
}

// ---------------------------------------------------------------------------
// Single command, no operators
// ---------------------------------------------------------------------------

#[test]
fn empty_input_is_a_valid_empty_pipeline() {
    let mut p = run(&[]);
    assert!(p.is_complete());
    assert_eq!(p.error(), "");
    assert_eq!(p.commands(), Vec::<Command>::new());
    assert!(!p.is_background());
}

#[test]
fn single_command_collects_words() {
    let mut p = run(&[word("ls"), word("-l"), word("/tmp")]);
    assert!(p.is_complete());
    assert_eq!(p.error(), "");
    let cmds = p.commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].words, vec!["ls", "-l", "/tmp"]);
    assert_eq!(cmds[0].redir_in, None);
    assert_eq!(cmds[0].redir_out, None);
    assert!(!cmds[0].append);
}

// ---------------------------------------------------------------------------
// Redirection ($ReadingCommand + redir → $ExpectingTarget + Word)
// ---------------------------------------------------------------------------

#[test]
fn output_redirection_truncate() {
    let mut p = run(&[word("echo"), word("hi"), Token::RedirOut, word("out.txt")]);
    let cmds = p.commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].words, vec!["echo", "hi"]);
    assert_eq!(cmds[0].redir_out.as_deref(), Some("out.txt"));
    assert!(!cmds[0].append);
    assert_eq!(p.error(), "");
}

#[test]
fn output_redirection_append() {
    let mut p = run(&[word("echo"), word("hi"), Token::RedirAppend, word("log")]);
    let cmds = p.commands();
    assert_eq!(cmds[0].redir_out.as_deref(), Some("log"));
    assert!(cmds[0].append);
}

#[test]
fn input_redirection() {
    let mut p = run(&[word("cat"), Token::RedirIn, word("in.txt")]);
    let cmds = p.commands();
    assert_eq!(cmds[0].words, vec!["cat"]);
    assert_eq!(cmds[0].redir_in.as_deref(), Some("in.txt"));
}

#[test]
fn input_and_output_redirection_on_one_command() {
    let mut p = run(&[
        word("sort"),
        Token::RedirIn,
        word("in"),
        Token::RedirOut,
        word("out"),
    ]);
    let cmds = p.commands();
    assert_eq!(cmds[0].redir_in.as_deref(), Some("in"));
    assert_eq!(cmds[0].redir_out.as_deref(), Some("out"));
}

// ---------------------------------------------------------------------------
// Pipes ($ReadingCommand + | → commit, start new)
// ---------------------------------------------------------------------------

#[test]
fn two_stage_pipeline() {
    let mut p = run(&[word("ls"), Token::Pipe, word("grep"), word("frame")]);
    let cmds = p.commands();
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].words, vec!["ls"]);
    assert_eq!(cmds[1].words, vec!["grep", "frame"]);
    assert_eq!(p.error(), "");
}

#[test]
fn three_stage_pipeline_with_redirection_on_last() {
    let mut p = run(&[
        word("cat"),
        Token::RedirIn,
        word("f"),
        Token::Pipe,
        word("sort"),
        Token::Pipe,
        word("uniq"),
        Token::RedirOut,
        word("out"),
    ]);
    let cmds = p.commands();
    assert_eq!(cmds.len(), 3);
    assert_eq!(cmds[0].words, vec!["cat"]);
    assert_eq!(cmds[0].redir_in.as_deref(), Some("f"));
    assert_eq!(cmds[1].words, vec!["sort"]);
    assert_eq!(cmds[2].words, vec!["uniq"]);
    assert_eq!(cmds[2].redir_out.as_deref(), Some("out"));
}

// ---------------------------------------------------------------------------
// Background ($ReadingCommand + & → $TrailingAmp)
// ---------------------------------------------------------------------------

#[test]
fn trailing_ampersand_sets_background() {
    let mut p = run(&[word("sleep"), word("5"), Token::Amp]);
    assert!(p.is_background());
    let cmds = p.commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].words, vec!["sleep", "5"]);
    assert_eq!(p.error(), "");
}

#[test]
fn lone_ampersand_is_a_noop_empty_pipeline() {
    let mut p = run(&[Token::Amp]);
    assert!(p.is_complete());
    assert!(p.is_background());
    assert_eq!(p.commands(), Vec::<Command>::new());
    assert_eq!(p.error(), "");
}

// ---------------------------------------------------------------------------
// Syntax errors
// ---------------------------------------------------------------------------

#[test]
fn leading_pipe_is_an_error() {
    let mut p = run(&[Token::Pipe, word("grep")]);
    assert!(p.is_complete());
    assert_eq!(p.error(), "syntax error near '|'");
}

#[test]
fn trailing_pipe_is_an_error() {
    let mut p = run(&[word("ls"), Token::Pipe]);
    assert!(p.is_complete());
    assert_eq!(p.error(), "syntax error near '|'");
}

#[test]
fn redirection_without_target_is_an_error() {
    let mut p = run(&[word("cat"), Token::RedirIn]);
    assert!(p.is_complete());
    assert_eq!(p.error(), "missing redirection target");
}

#[test]
fn two_redirection_operators_in_a_row_is_an_error() {
    let mut p = run(&[word("cat"), Token::RedirOut, Token::RedirOut, word("f")]);
    assert!(p.is_complete());
    assert_eq!(p.error(), "expected filename after redirection operator");
}

#[test]
fn token_after_ampersand_is_an_error() {
    let mut p = run(&[word("sleep"), word("5"), Token::Amp, word("echo")]);
    assert!(p.is_complete());
    assert_eq!(p.error(), "unexpected token after '&'");
}

#[test]
fn error_pipeline_has_no_usable_commands() {
    // After an error the consumer checks error() first; commands() is whatever
    // was committed before the failure, but the contract is "check error()".
    let mut p = run(&[word("ls"), Token::Pipe]);
    assert!(!p.error().is_empty());
}

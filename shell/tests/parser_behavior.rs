// shell/tests/parser_behavior.rs
//
// Level 3 (behavioral) tests for the Parser Frame system.
//
// Parser is a per-char event-driven tokenizer. These tests drive it via
// consume(c) and finalize() and assert on tokens() / error() / is_complete().
//
// Every committed state-event pair has at least one test:
//
//   $ReadingWord + consume(ws)        — stays in $ReadingWord
//   $ReadingWord + consume('"')       — → $InQuotedString (double quote)
//   $ReadingWord + consume("'")       — → $InQuotedString (single quote)
//   $ReadingWord + consume(other)     — → $InWord
//   $ReadingWord + finalize()         — → $Done (no tokens collected)
//
//   $InWord + consume(ws)             — push token, → $ReadingWord
//   $InWord + consume(other)          — append, stays in $InWord
//   $InWord + finalize()              — push token, → $Done
//
//   $InQuotedString + consume(match)  — push token, → $ReadingWord
//   $InQuotedString + consume(other)  — append, stays in $InQuotedString
//   $InQuotedString + finalize()      — → $Failed (unterminated)
//
//   $Done + consume(c)                — ignored (terminal)
//   $Done + finalize()                — idempotent (terminal)
//   $Failed + consume(c)              — ignored (terminal)
//   $Failed + finalize()              — idempotent (terminal)

use frame_os_shell::{Parser, Token};

/// Helper: feed an entire string through Parser and finalize.
fn parse(input: &str) -> Parser {
    let mut p = Parser::__create();
    for c in input.chars() {
        p.consume(c);
    }
    p.finalize();
    p
}

// ---------------------------------------------------------------------------
// $ReadingWord behavior
// ---------------------------------------------------------------------------

#[test]
fn parses_empty_input() {
    let mut p = parse("");
    assert!(p.is_complete());
    assert_eq!(p.tokens(), Vec::<String>::new());
    assert_eq!(p.error(), "");
}

#[test]
fn parses_whitespace_only_input() {
    let mut p = parse("   \t   ");
    assert!(p.is_complete());
    assert_eq!(p.tokens(), Vec::<String>::new());
    assert_eq!(p.error(), "");
}

#[test]
fn parses_leading_whitespace() {
    let mut p = parse("   hello");
    assert_eq!(p.tokens(), vec!["hello".to_string()]);
}

#[test]
fn parses_trailing_whitespace() {
    let mut p = parse("hello   ");
    assert_eq!(p.tokens(), vec!["hello".to_string()]);
}

// ---------------------------------------------------------------------------
// $InWord behavior
// ---------------------------------------------------------------------------

#[test]
fn parses_single_word() {
    let mut p = parse("hello");
    assert_eq!(p.tokens(), vec!["hello".to_string()]);
    assert!(p.is_complete());
    assert_eq!(p.error(), "");
}

#[test]
fn parses_multiple_words() {
    let mut p = parse("cd /tmp foo");
    assert_eq!(
        p.tokens(),
        vec!["cd".to_string(), "/tmp".to_string(), "foo".to_string()]
    );
}

#[test]
fn parses_tab_separated_words() {
    let mut p = parse("a\tb\tc");
    assert_eq!(
        p.tokens(),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
}

#[test]
fn collapses_runs_of_whitespace() {
    // Multiple consecutive whitespace chars between tokens act as a single
    // separator. No empty tokens emitted.
    let mut p = parse("foo     bar");
    assert_eq!(p.tokens(), vec!["foo".to_string(), "bar".to_string()]);
}

#[test]
fn parses_word_with_punctuation() {
    // Non-quote, non-whitespace chars are all part of the word.
    let mut p = parse("/usr/local/bin");
    assert_eq!(p.tokens(), vec!["/usr/local/bin".to_string()]);
}

// ---------------------------------------------------------------------------
// $InQuotedString behavior
// ---------------------------------------------------------------------------

#[test]
fn parses_double_quoted_string_with_spaces() {
    let mut p = parse(r#""hello world""#);
    assert_eq!(p.tokens(), vec!["hello world".to_string()]);
}

#[test]
fn parses_single_quoted_string_with_spaces() {
    let mut p = parse("'hello world'");
    assert_eq!(p.tokens(), vec!["hello world".to_string()]);
}

#[test]
fn parses_double_quoted_empty_string() {
    let mut p = parse(r#""""#);
    assert_eq!(p.tokens(), vec!["".to_string()]);
}

#[test]
fn parses_single_quoted_empty_string() {
    let mut p = parse("''");
    assert_eq!(p.tokens(), vec!["".to_string()]);
}

#[test]
fn parses_single_quote_inside_double_quoted() {
    // The other quote character is literal inside a quoted string.
    let mut p = parse(r#""it's me""#);
    assert_eq!(p.tokens(), vec!["it's me".to_string()]);
}

#[test]
fn parses_double_quote_inside_single_quoted() {
    let mut p = parse(r#"'say "hi"'"#);
    assert_eq!(p.tokens(), vec![r#"say "hi""#.to_string()]);
}

#[test]
fn parses_consecutive_quoted_tokens() {
    let mut p = parse(r#""foo" "bar baz""#);
    assert_eq!(p.tokens(), vec!["foo".to_string(), "bar baz".to_string()]);
}

#[test]
fn parses_mixed_quoted_and_unquoted() {
    let mut p = parse(r#"cat "my file.txt" /tmp"#);
    assert_eq!(
        p.tokens(),
        vec![
            "cat".to_string(),
            "my file.txt".to_string(),
            "/tmp".to_string(),
        ]
    );
}

#[test]
fn parses_quoted_token_at_start() {
    let mut p = parse(r#""hello" world"#);
    assert_eq!(p.tokens(), vec!["hello".to_string(), "world".to_string()]);
}

// ---------------------------------------------------------------------------
// $Failed (unterminated quote) behavior
// ---------------------------------------------------------------------------

#[test]
fn unterminated_double_quote_fails() {
    let mut p = parse(r#"cat "missing close"#);
    assert!(p.is_complete(), "Failed is a terminal state");
    assert!(
        !p.error().is_empty(),
        "error message should describe the unterminated quote"
    );
    assert!(
        p.error().contains('"'),
        "error should mention which quote character was unterminated"
    );
}

#[test]
fn unterminated_single_quote_fails() {
    let mut p = parse("echo 'oops");
    assert!(p.is_complete());
    assert!(!p.error().is_empty());
    assert!(p.error().contains('\''));
}

#[test]
fn failed_state_preserves_partial_tokens() {
    // Tokens that were successfully completed before the failure are still
    // reported. The Failed state isn't a panic — it's a documented terminal
    // for callers to handle with error() being non-empty.
    let mut p = parse(r#"good "bad"#);
    assert!(p.is_complete());
    assert!(!p.error().is_empty());
    assert_eq!(
        p.tokens(),
        vec!["good".to_string()],
        "the completed token before the unterminated quote is preserved"
    );
}

// ---------------------------------------------------------------------------
// Terminal states ($Done, $Failed) behavior
// ---------------------------------------------------------------------------

#[test]
fn is_complete_starts_false() {
    let mut p = Parser::__create();
    assert!(!p.is_complete(), "fresh parser is in $ReadingWord");
}

#[test]
fn is_complete_false_during_scanning() {
    let mut p = Parser::__create();
    p.consume('h');
    p.consume('i');
    assert!(!p.is_complete(), "still in $InWord");
}

#[test]
fn is_complete_true_after_finalize() {
    let mut p = Parser::__create();
    p.finalize();
    assert!(p.is_complete());
}

#[test]
fn done_state_ignores_further_consume() {
    let mut p = parse("hello");
    assert!(p.is_complete());
    p.consume('x');
    p.consume('y');
    p.consume('z');
    assert_eq!(
        p.tokens(),
        vec!["hello".to_string()],
        "$Done ignores further consume()"
    );
}

#[test]
fn done_state_finalize_is_idempotent() {
    let mut p = parse("hello");
    let tokens_before = p.tokens();
    p.finalize();
    p.finalize();
    assert_eq!(p.tokens(), tokens_before);
}

#[test]
fn failed_state_ignores_further_consume() {
    let mut p = parse(r#"good "bad"#); // unterminated
    assert!(p.is_complete());
    let tokens_before = p.tokens();
    p.consume('x');
    assert_eq!(p.tokens(), tokens_before);
}

// ---------------------------------------------------------------------------
// Sanity checks combining several rules
// ---------------------------------------------------------------------------

#[test]
fn parses_shell_command_with_args() {
    let mut p = parse("echo hello world from frame");
    assert_eq!(
        p.tokens(),
        vec![
            "echo".to_string(),
            "hello".to_string(),
            "world".to_string(),
            "from".to_string(),
            "frame".to_string(),
        ]
    );
}

#[test]
fn parses_realistic_cat_invocation() {
    let mut p = parse(r#"cat "/Users/me/My Documents/notes.txt""#);
    assert_eq!(
        p.tokens(),
        vec![
            "cat".to_string(),
            "/Users/me/My Documents/notes.txt".to_string(),
        ]
    );
}

#[test]
fn parses_many_short_tokens() {
    let mut p = parse("a b c d e f g h i j");
    let toks = p.tokens();
    assert_eq!(toks.len(), 10);
    assert_eq!(toks[0], "a");
    assert_eq!(toks[9], "j");
}

// ---------------------------------------------------------------------------
// Typed tokens (M1) — operator recognition + legacy reconstruction.
//
// typed_tokens() tags |, <, >, >>, & as operators when they appear as bare
// (unquoted) whitespace-separated tokens; everything else is a Word. The
// legacy tokens() view must keep reconstructing literal text so the bare-metal
// `ish` (which still reads tokens()) is byte-identical.
// ---------------------------------------------------------------------------

#[test]
fn typed_tokens_tags_pipe_operator() {
    let mut p = parse("ls | grep frame");
    assert_eq!(
        p.typed_tokens(),
        vec![
            Token::Word("ls".to_string()),
            Token::Pipe,
            Token::Word("grep".to_string()),
            Token::Word("frame".to_string()),
        ]
    );
}

#[test]
fn typed_tokens_tags_redirection_operators() {
    let mut p = parse("cat < in > out >> log");
    assert_eq!(
        p.typed_tokens(),
        vec![
            Token::Word("cat".to_string()),
            Token::RedirIn,
            Token::Word("in".to_string()),
            Token::RedirOut,
            Token::Word("out".to_string()),
            Token::RedirAppend,
            Token::Word("log".to_string()),
        ]
    );
}

#[test]
fn typed_tokens_tags_trailing_ampersand() {
    let mut p = parse("sleep 5 &");
    assert_eq!(
        p.typed_tokens(),
        vec![
            Token::Word("sleep".to_string()),
            Token::Word("5".to_string()),
            Token::Amp,
        ]
    );
}

#[test]
fn quoted_operator_stays_a_word() {
    // The whole point of putting operator recognition in the scanner: a quoted
    // "|" is a literal Word, not the pipe operator.
    let mut p = parse(r#"echo "|" '>' "&""#);
    assert_eq!(
        p.typed_tokens(),
        vec![
            Token::Word("echo".to_string()),
            Token::Word("|".to_string()),
            Token::Word(">".to_string()),
            Token::Word("&".to_string()),
        ]
    );
}

#[test]
fn operator_chars_inside_a_word_are_not_operators() {
    // Operators must be their own whitespace-separated token. `a|b` is one
    // Word (matching ish's parser), not Pipe.
    let mut p = parse("a|b c>d");
    assert_eq!(
        p.typed_tokens(),
        vec![
            Token::Word("a|b".to_string()),
            Token::Word("c>d".to_string())
        ]
    );
}

#[test]
fn legacy_tokens_reconstructs_operator_literals() {
    // tokens() (the flat view ish reads) is byte-identical to the pre-M1
    // behavior: operators come back as their literal strings.
    let mut p = parse("ls | grep x > out &");
    assert_eq!(
        p.tokens(),
        vec![
            "ls".to_string(),
            "|".to_string(),
            "grep".to_string(),
            "x".to_string(),
            ">".to_string(),
            "out".to_string(),
            "&".to_string(),
        ]
    );
}

#[test]
fn legacy_tokens_reconstructs_quoted_operator_literal() {
    // A quoted "|" reconstructs to "|" in the flat view too — so ish's existing
    // (quote-blind) behavior is unchanged.
    let mut p = parse(r#"echo "|""#);
    assert_eq!(p.tokens(), vec!["echo".to_string(), "|".to_string()]);
}

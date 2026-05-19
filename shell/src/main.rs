// shell/src/main.rs
//
// The hosted-mode Frame OS shell entry point.
//
// This is the native loop that drives the Frame Shell state machine. The
// state machine itself lives in frame/shell.frs and is regenerated as Rust
// by the build script.
//
// The loop:
//   1. Construct the Shell. Its $Prompting.$> handler prints the first prompt
//      to stdout via the print_prompt() native action.
//   2. Call rustyline's readline() with an empty prompt argument (the Shell
//      already printed the prompt to stdout; we don't want rustyline to
//      double-print). Rustyline still owns line editing and history.
//   3. Map readline outcomes to Frame events:
//        Ok(line)                   -> shell.line(&line)
//        Err(Interrupted)  (Ctrl-C) -> shell.interrupt()
//        Err(Eof)          (Ctrl-D) -> shell.interrupt()
//   4. Loop while !shell.is_done(). Exit cleanly when the Shell reports done.
//
// Why all input maps to Shell events rather than to direct control-flow
// decisions in this loop: Frame owns control flow (architecture.md). The
// state machine decides what each event means; this loop just routes the
// events.

use std::process::ExitCode;

use frame_os_shell::Shell;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

fn main() -> ExitCode {
    // Banner — printed once at startup, before the Shell prints its first
    // prompt. (Shell::__create() runs $Prompting.$> which prints "frame-os> ".)
    println!("Frame OS shell — H1");
    println!("type 'exit' or 'quit' to leave (Ctrl-C or Ctrl-D also work)");

    let mut shell = Shell::__create();

    let mut editor = match DefaultEditor::new() {
        Ok(e) => e,
        Err(err) => {
            eprintln!("could not initialize line editor: {err}");
            return ExitCode::from(2);
        }
    };

    while !shell.is_done() {
        // Pass empty prompt to rustyline — the Shell's print_prompt() action
        // already wrote "frame-os> " to stdout. Rustyline still handles line
        // editing, history, and Ctrl-C/Ctrl-D interception.
        match editor.readline("") {
            Ok(line) => {
                shell.line(&line);
            }
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                // Newline so "goodbye" isn't glued to the abandoned prompt.
                println!();
                shell.interrupt();
            }
            Err(err) => {
                eprintln!("read error: {err}");
                return ExitCode::from(2);
            }
        }
    }

    ExitCode::SUCCESS
}

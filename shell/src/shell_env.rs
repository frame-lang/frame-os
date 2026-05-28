// shell/src/shell_env.rs
//
// Hosted environment for the Shell FSM (M3b, H↔B parity).
//
// The Shell Frame system owns the *control flow* (the $Prompting → $Parsing →
// $Running* → $Prompting state graph). Everything target-specific — printing,
// classifying a command, running builtins / externals / pipelines, spawning and
// waiting on jobs — lives behind the `ShellEnv` trait (declared in the
// shell.frs native prolog). This is the hosted implementation: it wraps the
// existing `Builtin` classify/execute, the `exec` module (std::process pipes +
// redirection), and the `JobControl` Frame system, plus the shell's tracked
// `cwd`. The ring-3 `IshShellEnv` (in the user crate) implements the same trait
// with syscalls, so the *same* Shell state graph drives a shell on Linux and on
// bare metal — the M2 process-backend pattern applied to the whole shell.

use crate::builtin::{classify, execute, Builtin};
use crate::frame_systems::{Command, CommandKind, JobControl, ShellEnv};
use std::path::PathBuf;

/// The hosted shell environment: owns the cwd, the job table (`JobControl`),
/// and the id of the most recent foreground job (for exit-code reporting).
pub struct StdShellEnv {
    cwd: PathBuf,
    job_control: JobControl,
    last_foreground_id: u32,
}

impl StdShellEnv {
    pub fn new() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_default(),
            job_control: JobControl::__create(),
            last_foreground_id: 0,
        }
    }
}

impl Default for StdShellEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellEnv for StdShellEnv {
    fn println(&mut self, s: &str) {
        println!("{s}");
    }

    fn print_prompt(&mut self) {
        use std::io::{IsTerminal, Write};
        // On an interactive TTY the line editor (rustyline) renders the prompt,
        // so printing it here too would be erased on the first redraw. Only emit
        // it ourselves for non-TTY (piped) input.
        if !std::io::stdin().is_terminal() {
            print!("$> ");
            let _ = std::io::stdout().flush();
        }
    }

    fn print_goodbye(&mut self) {
        println!("goodbye");
    }

    fn tick(&mut self) {
        self.job_control.tick();
    }

    fn classify(&self, words: &[String]) -> CommandKind {
        match classify(words.to_vec()) {
            Builtin::Unknown(_, _) => CommandKind::External,
            Builtin::Fg(arg) => CommandKind::Fg(arg),
            _ => CommandKind::Builtin,
        }
    }

    fn run_builtin(
        &mut self,
        words: &[String],
        redir_out: Option<String>,
        append: bool,
        history: &[String],
    ) {
        // Re-derive the Builtin from the words (the FSM already knows it's a
        // builtin via classify()). Apply output redirection with a Unix fd
        // redirect guard so e.g. `pwd > f` / `echo hi > f` write the file.
        let builtin = classify(words.to_vec());
        match redir_out {
            Some(path) => {
                let _guard = crate::exec::redirect_stdout(&path, append, &self.cwd);
                execute(&builtin, &mut self.cwd, history, &mut self.job_control);
            }
            None => {
                execute(&builtin, &mut self.cwd, history, &mut self.job_control);
            }
        }
    }

    fn run_foreground_redirected(&mut self, cmd: &Command) {
        crate::exec::run_foreground_redirected(cmd, &self.cwd);
    }

    fn spawn_background(&mut self, cmd: &Command) {
        let has_redir = cmd.redir_in.is_some() || cmd.redir_out.is_some();
        if has_redir {
            crate::exec::spawn_background_redirected(cmd, &self.cwd);
        } else {
            let c = cmd.words[0].clone();
            let a = cmd.words[1..].to_vec();
            self.job_control.spawn_background(c, a);
        }
    }

    fn spawn_foreground(&mut self, words: &[String]) {
        let c = words[0].clone();
        let a = words[1..].to_vec();
        self.job_control.spawn_foreground(c, a);
        self.last_foreground_id = self.job_control.foreground_id();
    }

    fn fg(&mut self, id: u32) -> bool {
        self.job_control.fg(id);
        if self.job_control.is_running_foreground() {
            self.last_foreground_id = self.job_control.foreground_id();
            true
        } else {
            false
        }
    }

    fn run_pipeline(&mut self, commands: &[Command]) {
        crate::exec::run_foreground_pipeline(commands, &self.cwd);
    }

    fn wait_foreground(&mut self) {
        // Drive the foreground job to a resting state (exits or stops).
        self.job_control.wait_foreground();

        // Surface non-zero exit codes / spawn failures (preserves H2's
        // "[exit code: N]" and "command not found" output), and note whether the
        // foreground job finished (vs stopped).
        let id = self.last_foreground_id;
        let mut finished = false;
        for s in self.job_control.jobs().iter() {
            if s.id == id {
                if s.state == "Done" {
                    finished = true;
                    if s.exit_code != 0 {
                        println!("[exit code: {}]", s.exit_code);
                    }
                } else if s.state.starts_with("Failed") {
                    finished = true;
                    let parsed: String = s
                        .state
                        .strip_prefix("Failed (")
                        .and_then(|x| x.strip_suffix(")"))
                        .map(|x| x.to_string())
                        .unwrap_or_default();
                    if parsed.contains("No such file") {
                        println!("{}: command not found", s.cmd);
                    } else {
                        println!("{}: {}", s.cmd, parsed);
                    }
                }
                break;
            }
        }
        // bash-correct ids (M4): a *plain* foreground command carried only a
        // TENTATIVE id (id == next_id, never committed). If it finished, remove
        // the $Done entry so the id is freed/reused. A `fg <id>`-resumed job had
        // an already-committed id (id < next_id) — leave its $Done entry (it
        // shows in `jobs`, and its id can't collide since next_id moved past it).
        if finished && id == self.job_control.next_job_id() {
            self.job_control.remove(id);
        }
    }
}

/// The environment a freshly-created `Shell` gets in the hosted crate. (The
/// ring-3 user crate supplies its own `default_env()` returning an
/// `IshShellEnv`.) Resolved through the generated module's `use super::*`.
pub fn default_env() -> Box<dyn ShellEnv> {
    Box::new(StdShellEnv::new())
}

// shell/src/exec.rs
//
// Native execution mechanism for pipes and I/O redirection (M1, H↔B parity).
//
// The `Pipeline` Frame system owns the *coordination/structure* — it parses a
// command line into a `Vec<Command>` (stages) plus a background flag, and
// validates the grammar. This module owns the *mechanism*: actually spawning
// the host processes, wiring their stdio through OS pipes, and pointing fds at
// files for redirection. That is exactly the FSM-owns-logic / native-owns-
// mechanism split the rest of Frame OS follows (cf. virtio_blk's backend seam).
//
// Scope (matches the bare-metal `ish` feature set this milestone reaches
// parity with):
//   - Pipelines run in the FOREGROUND, synchronously. A trailing `&` on a
//     pipeline is ignored (same as `ish`). Builtins are not piped (also `ish`).
//   - Redirection (`< > >>`) applies to a single command. For external
//     commands it is wired via std::process Stdio; for builtins, output
//     redirection is applied with a temporary fd redirect (Unix) so e.g.
//     `echo hi > f` writes the file.
//   - File paths and external commands resolve against the shell's tracked
//     `cwd` (the one `cd` updates), not the host process cwd.

use crate::Command;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as PCommand, Stdio};

/// Resolve a user-supplied path against the shell's `cwd`. Absolute paths pass
/// through; relative paths join onto `cwd`. (Mirrors builtin.rs::resolve.)
fn resolve(path: &str, cwd: &Path) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

/// Open a redirection output target (`> file` truncate / `>> file` append),
/// resolved against `cwd`. Prints a shell-shaped error and returns None on
/// failure.
fn open_output(target: &str, append: bool, cwd: &Path) -> Option<File> {
    let path = resolve(target, cwd);
    let mut opts = OpenOptions::new();
    opts.write(true).create(true);
    if append {
        opts.append(true);
    } else {
        opts.truncate(true);
    }
    match opts.open(&path) {
        Ok(f) => Some(f),
        Err(e) => {
            println!("frame-os: {}: {e}", path.display());
            None
        }
    }
}

/// Open a redirection input target (`< file`), resolved against `cwd`.
fn open_input(target: &str, cwd: &Path) -> Option<File> {
    let path = resolve(target, cwd);
    match File::open(&path) {
        Ok(f) => Some(f),
        Err(e) => {
            println!("frame-os: {}: {e}", path.display());
            None
        }
    }
}

/// Map a spawn error to a shell-shaped message (parity with H2's
/// "command not found").
fn report_spawn_error(cmd: &str, e: &std::io::Error) {
    if e.kind() == std::io::ErrorKind::NotFound {
        println!("{cmd}: command not found");
    } else {
        println!("{cmd}: {e}");
    }
}

/// Build a std::process::Command for one external stage: program + args, cwd,
/// and any file redirections this stage carries. Pipe wiring (stdin/stdout to
/// neighbouring stages) is applied by the caller.
fn base_command(stage: &Command, cwd: &Path) -> PCommand {
    let mut pc = PCommand::new(&stage.words[0]);
    pc.args(&stage.words[1..]);
    pc.current_dir(cwd);
    pc
}

/// Run a single external command in the foreground with optional redirection,
/// waiting for it to finish. Surfaces a non-zero exit code like H2.
pub fn run_foreground_redirected(stage: &Command, cwd: &Path) {
    if stage.words.is_empty() {
        return;
    }
    let mut pc = base_command(stage, cwd);

    if let Some(ref t) = stage.redir_in {
        match open_input(t, cwd) {
            Some(f) => {
                pc.stdin(f);
            }
            None => return,
        }
    }
    if let Some(ref t) = stage.redir_out {
        match open_output(t, stage.append, cwd) {
            Some(f) => {
                pc.stdout(f);
            }
            None => return,
        }
    }

    match pc.spawn() {
        Ok(mut child) => match child.wait() {
            Ok(status) => {
                let code = status.code().unwrap_or(-1);
                if code != 0 {
                    println!("[exit code: {code}]");
                }
            }
            Err(e) => println!("{}: {e}", stage.words[0]),
        },
        Err(e) => report_spawn_error(&stage.words[0], &e),
    }
}

/// Spawn a single external command in the background with optional redirection.
/// Detached (own process group, stdio defaulted to null where not redirected)
/// so it neither holds our pipes open nor receives terminal Ctrl-C. Not tracked
/// in the JobControl table (an M1 limitation: `jobs`/`fg`/`bg` don't see a
/// redirected background command — plain `cmd &` still is tracked).
pub fn spawn_background_redirected(stage: &Command, cwd: &Path) {
    if stage.words.is_empty() {
        return;
    }
    let mut pc = base_command(stage, cwd);

    match &stage.redir_in {
        Some(t) => match open_input(t, cwd) {
            Some(f) => {
                pc.stdin(f);
            }
            None => return,
        },
        None => {
            pc.stdin(Stdio::null());
        }
    }
    match &stage.redir_out {
        Some(t) => match open_output(t, stage.append, cwd) {
            Some(f) => {
                pc.stdout(f);
            }
            None => return,
        },
        None => {
            pc.stdout(Stdio::null());
        }
    }
    pc.stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        pc.process_group(0);
    }

    match pc.spawn() {
        Ok(child) => {
            // Detached: drop the handle. We don't reap it (an M1 limitation;
            // it becomes a zombie until the shell exits — acceptable for the
            // rare `cmd > f &` case).
            drop(child);
        }
        Err(e) => report_spawn_error(&stage.words[0], &e),
    }
}

/// Run a foreground pipeline of external commands (`a | b | c`), wiring each
/// stage's stdout to the next stage's stdin through OS pipes. The first stage
/// honours `< file`; the last stage honours `> file` / `>> file`. Waits for
/// every stage. Builtins are not piped (parity with `ish`); the caller routes
/// pipelines here only when every stage is external.
pub fn run_foreground_pipeline(stages: &[Command], cwd: &Path) {
    let n = stages.len();
    if n == 0 {
        return;
    }
    let mut children: Vec<Child> = Vec::new();
    let mut prev_stdout: Option<std::process::ChildStdout> = None;

    for (i, stage) in stages.iter().enumerate() {
        if stage.words.is_empty() {
            println!("frame-os: syntax error near '|'");
            // Reap anything already spawned so we don't leak children.
            for mut c in children {
                let _ = c.wait();
            }
            return;
        }
        let mut pc = base_command(stage, cwd);

        // stdin: first stage from a redirect (if any); later stages from the
        // previous stage's pipe.
        if i == 0 {
            if let Some(ref t) = stage.redir_in {
                match open_input(t, cwd) {
                    Some(f) => {
                        pc.stdin(f);
                    }
                    None => {
                        for mut c in children {
                            let _ = c.wait();
                        }
                        return;
                    }
                }
            }
        } else if let Some(out) = prev_stdout.take() {
            pc.stdin(Stdio::from(out));
        }

        // stdout: last stage to a redirect (if any); earlier stages into a pipe.
        if i == n - 1 {
            if let Some(ref t) = stage.redir_out {
                match open_output(t, stage.append, cwd) {
                    Some(f) => {
                        pc.stdout(f);
                    }
                    None => {
                        for mut c in children {
                            let _ = c.wait();
                        }
                        return;
                    }
                }
            }
        } else {
            pc.stdout(Stdio::piped());
        }

        match pc.spawn() {
            Ok(mut child) => {
                if i != n - 1 {
                    prev_stdout = child.stdout.take();
                }
                children.push(child);
            }
            Err(e) => {
                report_spawn_error(&stage.words[0], &e);
                // Drop the upstream pipe's read end BEFORE waiting, so the
                // previous stage sees EOF on its stdout and can exit instead of
                // blocking on a full pipe (which would deadlock our wait()).
                drop(prev_stdout.take());
                for mut c in children {
                    let _ = c.wait();
                }
                return;
            }
        }
    }

    for mut child in children {
        let _ = child.wait();
    }
}

/// RAII guard that temporarily points the process's stdout (fd 1) at a file,
/// restoring the original fd on drop. Lets builtins (which write via
/// `println!`) honour `> file` / `>> file` without a sink refactor. Unix only;
/// on other platforms `redirect_stdout` returns None and builtins write to the
/// terminal (output redirection of builtins is unsupported off-Unix at M1).
#[cfg(unix)]
pub struct StdoutGuard {
    saved_fd: i32,
}

#[cfg(unix)]
impl Drop for StdoutGuard {
    fn drop(&mut self) {
        use std::io::Write;
        // Flush Rust's buffered stdout to the redirected fd before restoring.
        let _ = std::io::stdout().flush();
        unsafe {
            libc::dup2(self.saved_fd, libc::STDOUT_FILENO);
            libc::close(self.saved_fd);
        }
    }
}

/// Begin redirecting the process's stdout to `target`. Returns a guard that
/// restores the original stdout when dropped, or None if redirection couldn't
/// be set up (off-Unix, or the file couldn't be opened — an error is printed).
#[cfg(unix)]
pub fn redirect_stdout(target: &str, append: bool, cwd: &Path) -> Option<StdoutGuard> {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;

    let file = open_output(target, append, cwd)?;
    // Flush anything already buffered for the real stdout first.
    let _ = std::io::stdout().flush();
    unsafe {
        let saved_fd = libc::dup(libc::STDOUT_FILENO);
        if saved_fd < 0 {
            return None;
        }
        if libc::dup2(file.as_raw_fd(), libc::STDOUT_FILENO) < 0 {
            libc::close(saved_fd);
            return None;
        }
        // `file` drops here, closing its fd — but fd 1 now refers to the same
        // open file description (dup2 duplicated it), so writes still land.
        Some(StdoutGuard { saved_fd })
    }
}

#[cfg(not(unix))]
pub struct StdoutGuard;

#[cfg(not(unix))]
pub fn redirect_stdout(_target: &str, _append: bool, _cwd: &Path) -> Option<StdoutGuard> {
    None
}

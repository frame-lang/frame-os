// shell/src/process_backend.rs
//
// Process backend seam (M2, H↔B parity).
//
// The `Job` Frame system owns the *lifecycle* — $Created → $Foreground /
// $Background / $Stopped → $Done, and the legal-move boundaries between them.
// The actual OS *mechanism* — spawning a child, reaping it, delivering signals
// — lives behind this `ProcessBackend` trait. `Job` calls the trait; it no
// longer mentions `std::process` or `libc` itself.
//
// Why a seam: the coordination is identical on both targets, but the mechanism
// differs — hosted spawns sibling processes via `std::process` + `libc::kill`;
// ring-3 Frame OS will `fork`/`exec`/`waitpid`/`kill` via syscalls (M3). Each
// crate that compiles `job.frs` supplies its own `default_backend()`; the FSM
// is backend-agnostic. (Same shape as virtio_blk's read/write backend seam and
// the RAM-disk backend.) At M2 only the hosted `StdProcessBackend` exists; M3
// adds the syscall backend when `ish` migrates onto the shared `Job` FSM.

/// The OS mechanism behind a `Job`. One backend instance per `Job`; it owns
/// whatever native handle the platform needs (a `std::process::Child` here).
pub trait ProcessBackend {
    /// Spawn the command in the foreground, inheriting the shell's stdio.
    /// Returns the child pid, or a shell-shaped error message (NotFound is
    /// normalized to "No such file or directory" so the shell's
    /// "command not found" mapping works cross-platform).
    fn spawn(&mut self, cmd: &str, args: &[String]) -> Result<u32, String>;

    /// Spawn detached for background launch: stdio redirected to null and the
    /// child placed in its own process group, so it neither holds the shell's
    /// pipes open nor receives terminal Ctrl-C. Same return contract as spawn.
    fn spawn_detached(&mut self, cmd: &str, args: &[String]) -> Result<u32, String>;

    /// Non-blocking reap. `Some(code)` if the process has exited (or there is
    /// no live child — already reaped / spawn failed); `None` if still running.
    fn try_reap(&mut self) -> Option<i32>;

    /// Deliver a stop (SIGTSTP), continue (SIGCONT), or kill (SIGKILL).
    fn signal_stop(&mut self);
    fn signal_continue(&mut self);
    fn signal_kill(&mut self);
}

/// The hosted backend: `std::process` to spawn/reap, `libc::kill` to signal.
/// This is exactly the mechanism that lived inline in `job.frs` before M2.
pub struct StdProcessBackend {
    child: Option<std::process::Child>,
    pid: u32,
    exit_code: i32,
}

impl StdProcessBackend {
    pub fn new() -> Self {
        Self {
            child: None,
            pid: 0,
            exit_code: 0,
        }
    }

    /// Normalize a spawn error: a missing command is "No such file or
    /// directory" on Unix but "The system cannot find the file specified" on
    /// Windows — normalize NotFound so the shell maps it to "command not found"
    /// uniformly.
    fn spawn_error_message(e: &std::io::Error) -> String {
        if e.kind() == std::io::ErrorKind::NotFound {
            "No such file or directory".to_string()
        } else {
            e.to_string()
        }
    }
}

impl Default for StdProcessBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessBackend for StdProcessBackend {
    fn spawn(&mut self, cmd: &str, args: &[String]) -> Result<u32, String> {
        match std::process::Command::new(cmd).args(args).spawn() {
            Ok(child) => {
                self.pid = child.id();
                self.child = Some(child);
                Ok(self.pid)
            }
            Err(e) => Err(Self::spawn_error_message(&e)),
        }
    }

    fn spawn_detached(&mut self, cmd: &str, args: &[String]) -> Result<u32, String> {
        let mut command = std::process::Command::new(cmd);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        match command.spawn() {
            Ok(child) => {
                self.pid = child.id();
                self.child = Some(child);
                Ok(self.pid)
            }
            Err(e) => Err(Self::spawn_error_message(&e)),
        }
    }

    fn try_reap(&mut self) -> Option<i32> {
        if let Some(ref mut c) = self.child {
            match c.try_wait() {
                Ok(Some(status)) => {
                    self.exit_code = status.code().unwrap_or(-1);
                    self.child = None;
                    Some(self.exit_code)
                }
                Ok(None) => None,
                Err(_) => {
                    // Unrecoverable wait error — treat as terminal.
                    self.exit_code = -1;
                    self.child = None;
                    Some(-1)
                }
            }
        } else {
            // No live child (already reaped, or spawn failed) — done.
            Some(self.exit_code)
        }
    }

    fn signal_stop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.pid as i32, libc::SIGTSTP);
        }
    }

    fn signal_continue(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.pid as i32, libc::SIGCONT);
        }
    }

    fn signal_kill(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.pid as i32, libc::SIGKILL);
        }
        #[cfg(not(unix))]
        {
            if let Some(ref mut c) = self.child {
                let _ = c.kill();
            }
        }
    }
}

/// The backend a freshly-created `Job` gets. Each crate that compiles
/// `job.frs` provides its own `default_backend()` (resolved through the
/// generated module's `use super::*`); the FSM stays backend-agnostic. The
/// hosted crate returns the `std::process` backend; the ring-3 crate will
/// return a syscall backend at M3.
pub fn default_backend() -> Box<dyn ProcessBackend> {
    Box::new(StdProcessBackend::new())
}

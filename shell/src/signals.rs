// shell/src/signals.rs
//
// Process-wide signal handling for the H3 hosted shell.
//
// We install one SIGTSTP handler at startup (Unix only). When the user
// hits Ctrl-Z, the OS delivers SIGTSTP to every process in the terminal
// foreground process group — that includes both the shell AND the
// foreground child. The child's default disposition is STOP (it pauses);
// the shell's default disposition is also STOP, which would freeze the
// whole interactive session. Our handler does TWO things:
//
//   1. Overrides the default disposition so the shell keeps running.
//   2. Sets a static atomic flag the JobControl polling loop checks each
//      iteration. When the flag is observed set, JobControl knows the
//      foreground was stopped externally, calls Job.stop() to record the
//      $Stopped transition, and returns to $Idle so the shell can resume
//      prompting.
//
// The flag pattern is the only async-signal-safe way to communicate from
// a signal handler back to ordinary code. We can't mutate Shell or
// JobControl from a signal handler — those involve Rust borrow rules,
// heap allocation, etc. all of which are unsafe in signal context.

#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};

/// True iff a SIGTSTP has been delivered since the last reset.
/// Read+reset by JobControl's polling loop via `take_suspend_flag()`.
#[cfg(unix)]
static SUSPEND_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Install the SIGTSTP handler. Call once at shell startup; safe to call
/// again (signal-hook handles re-registration cleanly).
#[cfg(unix)]
pub fn install_sigtstp_handler() -> std::io::Result<()> {
    // signal-hook's `register` returns a SigId we'd need to keep alive to
    // unregister later. We never unregister — the handler should be live
    // for the shell's whole lifetime — so we discard the SigId.
    //
    // The handler closure must be async-signal-safe. AtomicBool::store
    // with SeqCst ordering is safe; that's the only thing we do.
    unsafe {
        signal_hook::low_level::register(signal_hook::consts::SIGTSTP, || {
            SUSPEND_REQUESTED.store(true, Ordering::SeqCst);
        })?;
    }
    Ok(())
}

/// Atomic load+reset of the suspend flag. Returns true exactly once per
/// SIGTSTP delivery (subsequent calls return false until the next signal).
#[cfg(unix)]
pub fn take_suspend_flag() -> bool {
    SUSPEND_REQUESTED.swap(false, Ordering::SeqCst)
}

// On non-Unix, the suspend flag is permanently false and installing the
// handler is a no-op. JobControl's polling loop reads false and never
// triggers the stop path. The shell still works; it just doesn't honor
// Ctrl-Z (Windows has no SIGTSTP analog without Job Object suspend APIs).
#[cfg(not(unix))]
pub fn install_sigtstp_handler() -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub fn take_suspend_flag() -> bool {
    false
}

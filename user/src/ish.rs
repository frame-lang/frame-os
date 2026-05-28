// Frame OS interactive shell "ish" (B8).
//
// A real REPL, in ring 3: print a prompt, `read_line` from the console (the
// kernel echoes keystrokes + hands back a whole line), then parse it with the
// *same* Frame FSMs the hosted shell uses — `frame/parser.frs` tokenizes (tagging
// | < > >> & as typed tokens) and `frame/pipeline.frs` folds those into a command
// pipeline + redirection + background flag (M3a). One FSM source, two radically
// different targets. ish still owns the *execution* (fork/exec/dup2/pipe via
// syscalls). After parsing, dispatch:
//   - `exit`            → leave the shell (the kernel halts)
//   - `help`            → list builtins
//   - `cat <path>...`   → stream files to the console
//   - anything else     → fork + exec the program (`/bin/<cmd>`, or an absolute
//                         path) and wait for it — so the shell *survives* running
//                         a program (exec replaces the *child*, not the shell).
//
// Unlike the scripted `frameshell` (B4), this reads live input (read_line, B8) and
// uses fork/exec/wait so it loops forever instead of being replaced on the first
// exec. The Parser is the same generated FSM; only the I/O around it changed.
//
// Syscall ABI: 0=write_char 1=exit 2=fork 4=wait 5=open 6=read 7=close
//              8=exec_path(path_ptr,len) 9=read_line(buf_ptr,len)
//              11=exec_argv 19=chdir 20=getcwd

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::panic::PanicInfo;

mod frame_systems;
mod mem;

// All the shared Frame systems ish uses (Parser, Pipeline, Shell, Job,
// JobControl) are pulled in inside the shell_fsm / job_fsm modules; ish root
// imports none of them directly any more (IshJobs was retired at M4.3b).

#[inline(always)]
unsafe fn syscall3(num: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        inlateout("rax") num => ret,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        out("rcx") _,
        out("r11") _,
        options(nostack),
    );
    ret
}

fn write_char(b: u8) {
    unsafe { syscall3(0, b as u64, 0, 0) };
}
fn print(s: &[u8]) {
    for &b in s {
        write_char(b);
    }
}
/// Emit a whole line in ONE write() syscall. ish's `print` writes a byte at a
/// time (write_char, #0), so an async kernel log line (e.g. a signal-delivery
/// trace) can interleave *between* bytes and split the output. The console
/// write syscall emits the whole buffer atomically (a process's line is never
/// split mid-way), so job-control reports — which print exactly when signals are
/// firing — build the line in a buffer and emit it in one call.
fn emit(line: &[u8]) {
    unsafe { syscall3(12, 1, line.as_ptr() as u64, line.len() as u64) };
}
/// Append `n` in decimal to a byte buffer (no allocation in the hot path beyond
/// the Vec the caller already holds).
fn push_u64(out: &mut Vec<u8>, mut n: u64) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    while n > 0 {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    out.extend_from_slice(&tmp[i..]);
}

/// Parse a base-10 `u32` (job id). None on empty or any non-digit.
fn parse_u32(s: &str) -> Option<u32> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(v)
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}
fn fork() -> u64 {
    unsafe { syscall3(2, 0, 0, 0) }
}
fn waitpid(pid: u64) -> u64 {
    unsafe { syscall3(4, pid, 0, 0) }
}
/// Per-pid non-blocking reap: `reap_nohang` with a specific target (the kernel's
/// sys_reap_nohang reaps exactly `pid` when target != 0). Returns
/// `(pid<<32)|status` if that child has exited, else 0. The SyscallProcessBackend
/// (M4) uses this for Job.poll()'s per-pid try_reap.
fn reap_pid_nohang(pid: u64) -> u64 {
    unsafe { syscall3(28, pid, 0, 0) }
}
/// kill (#29): send signal `sig` to process `pid`. Returns u64::MAX if no such
/// process. The shell sends SIGTERM (15) for `kill %job` / `kill <pid>`.
fn kill(pid: u64, sig: u64) -> u64 {
    unsafe { syscall3(29, pid, sig, 0) }
}
/// set_foreground (#33): tell the kernel which pid is the foreground job, so the
/// console's Ctrl-C/Ctrl-Z routes the terminal signal to it. 0 = none (the shell
/// is back at its prompt).
fn set_foreground(pid: u64) {
    unsafe { syscall3(33, pid, 0, 0) };
}
/// waitpid returns this (bit 63) when the target stopped (job-control suspend,
/// POSIX WIFSTOPPED) instead of exiting. Must match usermode::WSTOPPED_FLAG.
const WSTOPPED: u64 = 1 << 63;
/// SIGCONT, sent to resume a stopped job (bg/fg).
const SIGCONT: u64 = 18;
fn open(path: &[u8]) -> u64 {
    unsafe { syscall3(5, path.as_ptr() as u64, path.len() as u64, 0) }
}
/// open for output (redirection): flags bit0 = write/truncate, bit1 = append.
fn open_out(path: &[u8], append: bool) -> u64 {
    let flags = if append { 3 } else { 1 };
    unsafe { syscall3(5, path.as_ptr() as u64, path.len() as u64, flags) }
}
/// dup2 (#22): repoint `newfd` at `oldfd`. Used to wire redirection in the child.
fn dup2(oldfd: u64, newfd: u64) -> u64 {
    unsafe { syscall3(22, oldfd, newfd, 0) }
}
/// pipe (#23): create an anonymous pipe → (read_fd, write_fd), or None on
/// failure. The kernel writes the two fds into the array we pass.
fn pipe() -> Option<(u64, u64)> {
    let mut fds = [0u64; 2];
    let r = unsafe { syscall3(23, fds.as_mut_ptr() as u64, 0, 0) };
    if r == u64::MAX {
        None
    } else {
        Some((fds[0], fds[1]))
    }
}
fn read(fd: u64, buf: &mut [u8]) -> u64 {
    unsafe { syscall3(6, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}
fn close(fd: u64) {
    unsafe { syscall3(7, fd, 0, 0) };
}
/// exec with arguments (B9-2): `buf` is `argc` NUL-terminated strings, `argv[0]`
/// is the program path. Only returns on failure (a bad path / load error).
fn exec_argv(buf: &[u8], argc: u64) -> u64 {
    unsafe { syscall3(11, buf.as_ptr() as u64, buf.len() as u64, argc) }
}
fn read_line(buf: &mut [u8]) -> usize {
    unsafe { syscall3(9, buf.as_mut_ptr() as u64, buf.len() as u64, 0) as usize }
}
/// chdir (#19): set this shell's cwd. Returns u64::MAX if the path isn't a dir.
fn chdir(path: &[u8]) -> u64 {
    unsafe { syscall3(19, path.as_ptr() as u64, path.len() as u64, 0) }
}
/// getcwd (#20): write the cwd into `buf`, returning its byte length (no NUL).
fn getcwd(buf: &mut [u8]) -> u64 {
    unsafe { syscall3(20, buf.as_mut_ptr() as u64, buf.len() as u64, 0) }
}

/// `cat`: stream a file's bytes to the console.
fn cat(path: &str) {
    let fd = open(path.as_bytes());
    if fd == u64::MAX {
        print(b"cat: cannot open ");
        print(path.as_bytes());
        write_char(b'\n');
        return;
    }
    let mut buf = [0u8; 64];
    loop {
        let n = read(fd, &mut buf);
        if n == 0 {
            break;
        }
        print(&buf[..n as usize]);
    }
    close(fd);
}

/// Run an external program with its arguments and optional I/O redirection.
/// Build a packed argv — `argv[0]` is the resolved disk path (`/bin/<cmd>` unless
/// `cmd` is already absolute), followed by the remaining tokens verbatim, each
/// NUL-terminated — then fork: the child applies redirection (open the target,
/// `dup2` it onto fd 0/1, close the temp) and execs from disk; the parent waits.
/// The shell survives because exec replaces the *child's* image, and the child's
/// redirected fd table is inherited across exec (per-process fds, S5). Error
/// messages use `write_char` (#0), which always reaches the console, so they're
/// visible even when stdout is redirected.
/// Build a packed argv for `toks`: `argv[0]` is the resolved disk path
/// (`/bin/<cmd>` unless `cmd` is absolute), followed by the remaining tokens
/// verbatim, each NUL-terminated. Returns (packed bytes, argc).
fn build_argv(toks: &[String]) -> (Vec<u8>, u64) {
    let cmd = toks[0].as_str();
    let mut argv: Vec<u8> = Vec::new();
    if !cmd.starts_with('/') {
        argv.extend_from_slice(b"/bin/");
    }
    argv.extend_from_slice(cmd.as_bytes());
    argv.push(0);
    for t in &toks[1..] {
        argv.extend_from_slice(t.as_bytes());
        argv.push(0);
    }
    (argv, toks.len() as u64)
}

/// Run an external command with I/O redirection, WITHOUT the job table (M4).
/// Used for redirected commands + pipeline stages, which bypass JobControl on
/// both targets (the hosted side runs these via exec.rs, not JobControl). The
/// child applies `< > >>` then execs from disk; for a foreground command the
/// parent registers it as the terminal foreground job and waits. (Plain,
/// non-redirected fg/bg commands go through the shared JobControl/Job FSM now —
/// see IshShellEnv.) A backgrounded redirected command is not tracked (rare;
/// not exercised by console-test).
fn run_external_untracked(
    toks: &[String],
    redir_in: &Option<String>,
    redir_out: &Option<(String, bool)>,
    background: bool,
) {
    let cmd = toks[0].as_str();
    let (argv, argc) = build_argv(toks);

    let child = fork();
    if child == 0 {
        // Child. Apply input redirection (`< file`) onto fd 0.
        if let Some(path) = redir_in {
            let fd = open(path.as_bytes());
            if fd == u64::MAX {
                print(b"ish: cannot open input: ");
                print(path.as_bytes());
                write_char(b'\n');
                exit(1);
            }
            dup2(fd, 0);
            close(fd);
        }
        // Apply output redirection (`> file` truncate / `>> file` append) onto fd 1.
        if let Some((path, append)) = redir_out {
            let fd = open_out(path.as_bytes(), *append);
            if fd == u64::MAX {
                print(b"ish: cannot open output: ");
                print(path.as_bytes());
                write_char(b'\n');
                exit(1);
            }
            dup2(fd, 1);
            close(fd);
        }
        // Become the program loaded from disk. exec only returns on failure.
        exec_argv(&argv, argc);
        print(b"ish: command not found: ");
        print(cmd.as_bytes());
        write_char(b'\n');
        exit(127);
    }
    if !background {
        // Foreground: this child owns the terminal while it runs, then we wait.
        set_foreground(child);
        let _ = waitpid(child);
        set_foreground(0);
    }
    // Background redirected command: don't wait (untracked).
}

/// Run `left | right`: connect left's stdout to right's stdin through a pipe
/// (S6). Fork the writer (stdout → pipe write end) and the reader (stdin → pipe
/// read end); the parent closes *both* pipe ends — otherwise the reader never
/// sees EOF, since the shell would still count as a writer — and reaps both.
/// Both sides are external programs (builtins aren't piped).
fn run_pipeline(left: &[String], right: &[String]) {
    let (lv, lc) = build_argv(left);
    let (rv, rc) = build_argv(right);
    let Some((rfd, wfd)) = pipe() else {
        print(b"ish: pipe failed\n");
        return;
    };
    // Writer: stdout → pipe write end.
    let wpid = fork();
    if wpid == 0 {
        dup2(wfd, 1);
        close(rfd);
        close(wfd);
        exec_argv(&lv, lc);
        print(b"ish: command not found: ");
        print(left[0].as_bytes());
        write_char(b'\n');
        exit(127);
    }
    // Reader: stdin → pipe read end.
    let rpid = fork();
    if rpid == 0 {
        dup2(rfd, 0);
        close(rfd);
        close(wfd);
        exec_argv(&rv, rc);
        print(b"ish: command not found: ");
        print(right[0].as_bytes());
        write_char(b'\n');
        exit(127);
    }
    // Parent: drop both ends so the reader gets EOF when the writer exits, then
    // wait for *both* children specifically (not `wait`-any) so the shell only
    // returns to the prompt once the whole pipeline has finished.
    close(rfd);
    close(wfd);
    waitpid(wpid);
    waitpid(rpid);
}

// (Token parsing — the `&` / `|` split and redirection extraction — used to
// live here as `parse_redirs` + a manual pipe scan in run_line. M3a replaced
// them with the shared Parser -> Pipeline FSM; the grammar is owned by
// frame/pipeline.frs now, the same source the hosted shell compiles.)

// (M4.3b retired the IshJobs-backed job functions — harvest, resolve_job_id,
// fg_job, bg_job, kill_cmd, list_jobs. The job table is now the shared
// JobControl FSM; IshShellEnv presents ish's bash-style job output over its
// snapshot + drives spawn/wait/fg/bg/kill through it. parse_signal stays —
// IshShellEnv's kill builtin uses it.)

/// Map a signal spec (name without the `-`, or a number) to its number.
/// Supports the job-control + terminate set the shell uses.
fn parse_signal(s: &str) -> Option<u64> {
    match s {
        "INT" | "2" => Some(2),
        "KILL" | "9" => Some(9),
        "TERM" | "15" => Some(15),
        "CONT" | "18" => Some(18),
        "STOP" | "19" => Some(19),
        "TSTP" | "20" => Some(20),
        _ => parse_u32(s).map(|n| n as u64),
    }
}

// (run_line — ish's hand-written Parser→Pipeline→dispatch loop — was retired at
// M3b.3. The control flow it encoded is now the shared frame/shell.frs FSM
// (shell_fsm below), and its per-command dispatch lives in IshShellEnv. ish's
// execution helpers above (run_external, run_pipeline, cat, the job builtins)
// are unchanged; IshShellEnv calls them.)

// ── Job-control FSM reuse (M4) ─────────────────────────────────────────────
//
// The SAME frame/job.frs + frame/job_control.frs the hosted shell compiles,
// here for ring 3 over a syscall ProcessBackend — unifying the job table across
// targets (replacing the ish-specific IshJobs). M4.2/.3-compile lands the
// *compilation* (the whole job-control stack builds for x86_64-unknown-none +
// SyscallProcessBackend is complete); the final M4.3 step rewires IshShellEnv
// onto JobControl and retires IshJobs. Until then this module is unused —
// #[allow(dead_code)] — and IshShellEnv keeps driving IshJobs.
// clippy::wrong_self_convention: the generated Job FSM has `to_foreground` /
// `to_background` taking `&mut self` (Frame interface methods, not conversions).
#[allow(dead_code, unused_imports, clippy::wrong_self_convention)]
mod job_fsm {
    // Names the generated job.rs / job_control.rs reference via `use super::*`.
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;

    // ish syscalls the SyscallProcessBackend drives.
    use super::{
        build_argv, exec_argv, exit, fork, kill, print, reap_pid_nohang, set_foreground, waitpid,
        write_char, SIGCONT, WSTOPPED,
    };

    // Ring-3 JobSummary — the snapshot row job_control.frs's jobs() builds.
    // Must match the fields job_control.frs constructs (id/state/cmd/exit_code),
    // mirroring the hosted shell/src/job_summary.rs.
    #[derive(Clone)]
    pub struct JobSummary {
        pub id: u32,
        pub pid: u32,
        pub state: String,
        pub cmd: String,
        pub exit_code: i32,
    }

    // ProcessBackend + FgOutcome come from job.rs's prolog; Job from job.rs;
    // JobControl from job_control.rs (uses Job + JobSummary).
    include!(concat!(env!("OUT_DIR"), "/job.rs"));
    include!(concat!(env!("OUT_DIR"), "/job_control.rs"));

    /// The ring-3 process backend: fork/exec/waitpid/kill syscalls behind the
    /// shared ProcessBackend trait. The blocking-waitpid counterpart of the
    /// hosted StdProcessBackend's poll loop (same FgOutcome, different mechanism).
    pub struct SyscallProcessBackend {
        pid: u32,
        exit_code: i32,
        foreground: bool,
    }

    impl SyscallProcessBackend {
        fn new() -> Self {
            Self {
                pid: 0,
                exit_code: 0,
                foreground: false,
            }
        }

        fn do_spawn(
            &mut self,
            cmd: &str,
            args: &[String],
            foreground: bool,
        ) -> Result<u32, String> {
            // build_argv wants [cmd, args...]; it resolves cmd to /bin/<cmd>
            // (unless absolute) and NUL-packs the rest.
            let mut toks: Vec<String> = Vec::with_capacity(args.len() + 1);
            toks.push(cmd.to_string());
            for a in args {
                toks.push(a.clone());
            }
            let (argv, argc) = build_argv(&toks);
            let child = fork();
            if child == 0 {
                exec_argv(&argv, argc);
                print(b"ish: command not found: ");
                print(cmd.as_bytes());
                write_char(b'\n');
                exit(127);
            }
            self.pid = child as u32;
            self.foreground = foreground;
            if foreground {
                // Route terminal Ctrl-C / Ctrl-Z to this child while it runs.
                set_foreground(child);
            }
            Ok(self.pid)
        }
    }

    impl ProcessBackend for SyscallProcessBackend {
        fn spawn(&mut self, cmd: &str, args: &[String]) -> Result<u32, String> {
            self.do_spawn(cmd, args, true)
        }

        fn spawn_detached(&mut self, cmd: &str, args: &[String]) -> Result<u32, String> {
            self.do_spawn(cmd, args, false)
        }

        fn try_reap(&mut self) -> Option<i32> {
            if self.pid == 0 {
                return Some(self.exit_code);
            }
            let v = reap_pid_nohang(self.pid as u64);
            if v == 0 {
                None
            } else {
                self.exit_code = (v & 0xffff_ffff) as u32 as i32;
                self.pid = 0;
                Some(self.exit_code)
            }
        }

        fn signal_stop(&mut self) {
            if self.pid != 0 {
                kill(self.pid as u64, 20); // SIGTSTP
            }
        }

        fn signal_continue(&mut self) {
            if self.pid != 0 {
                kill(self.pid as u64, SIGCONT);
            }
        }

        fn signal_kill(&mut self) {
            if self.pid != 0 {
                kill(self.pid as u64, 9); // SIGKILL
            }
        }

        fn wait_foreground(&mut self) -> FgOutcome {
            if self.pid == 0 {
                return FgOutcome::Exited(self.exit_code);
            }
            // Block in the kernel; it routes terminal Ctrl-Z to the foreground
            // child and returns WSTOPPED if it stopped rather than exited.
            let st = waitpid(self.pid as u64);
            if self.foreground {
                set_foreground(0);
            }
            if st & WSTOPPED != 0 {
                FgOutcome::Stopped
            } else {
                self.exit_code = (st & 0xffff_ffff) as u32 as i32;
                self.pid = 0;
                FgOutcome::Exited(self.exit_code)
            }
        }
    }

    /// The backend a ring-3 Job gets (job.rs's domain init calls this).
    pub fn default_backend() -> Box<dyn ProcessBackend> {
        Box::new(SyscallProcessBackend::new())
    }
}

// ── Shell control-flow FSM reuse (M3b) ─────────────────────────────────────
//
// The SAME frame/shell.frs control-flow FSM the hosted shell compiles, here for
// ring 3. Included in this LOCAL module (not the shared frame_systems, which
// other user bins include and shouldn't have to supply a ShellEnv). The Shell
// FSM owns the $Prompting → $Parsing → $Running* coordination; everything
// target-specific goes through the `ShellEnv` trait (declared in shell.frs's
// prolog), implemented here by `IshShellEnv` using ish's syscalls.
//
// M3b.2 lands the *compilation* (the whole Shell stack builds for
// x86_64-unknown-none + IshShellEnv is complete); M3b.3 rewires `_start` to
// drive the FSM. Until then the module is unused — hence #[allow(dead_code)] —
// and `_start` keeps the proven hand-written `run_line` loop.
mod shell_fsm {
    // Names the generated shell.rs references (its `mod _shell_framec` picks
    // these up via `use super::*`), mirroring frame_systems.rs's re-exports.
    pub use crate::frame_systems::{Command, Parser, Pipeline, Token, TokenKind};
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;

    // ish syscalls + the shared JobControl/Job FSMs (job_fsm, over the
    // SyscallProcessBackend). M4.3b retired the ish-specific IshJobs + the old
    // run_external; the job TABLE is now the shared JobControl — the SAME FSM the
    // hosted shell uses. Redirected commands + pipelines stay native
    // (run_external_untracked / run_pipeline), same as the hosted side.
    use super::job_fsm::JobControl;
    use super::{
        cat, chdir, emit, getcwd, kill, parse_signal, parse_u32, print, push_u64,
        run_external_untracked, run_pipeline, write_char,
    };

    include!(concat!(env!("OUT_DIR"), "/shell.rs"));

    /// Ring-3 environment for the shared Shell FSM. The job TABLE is now the
    /// shared `JobControl` FSM (over the SyscallProcessBackend) — the SAME FSM
    /// the hosted shell uses, replacing the ish-specific IshJobs (M4). ish keeps
    /// its own *presentation* (the bash-style report lines the console-test
    /// asserts) layered over JobControl's snapshot, and its native exec for
    /// redirected commands + pipelines (which bypass JobControl on both targets).
    pub struct IshShellEnv {
        jobs: JobControl,
    }

    impl IshShellEnv {
        fn new() -> Self {
            Self {
                jobs: JobControl::__create(),
            }
        }

        /// pid of a live (non-done) job by id, via JobControl's snapshot; 0 if
        /// none. Used to resolve `%N` job specs for kill/bg.
        fn live_pid(&mut self, id: u32) -> u32 {
            for s in self.jobs.jobs() {
                if s.id == id && s.state != "Done" && !s.state.starts_with("Failed") {
                    return s.pid;
                }
            }
            0
        }

        /// Report + remove a `JobSummary`-shaped row as a bash-style line.
        fn emit_job_line(prefix: &[u8], id: u32, mid: &[u8], cmd: &str, suffix: &[u8]) {
            let mut l = Vec::new();
            l.extend_from_slice(prefix);
            push_u64(&mut l, id as u64);
            l.extend_from_slice(mid);
            l.extend_from_slice(cmd.as_bytes());
            l.extend_from_slice(suffix);
            emit(&l);
        }
    }

    impl ShellEnv for IshShellEnv {
        fn println(&mut self, s: &str) {
            // One atomic write (a kernel log can't split it mid-line).
            let mut l = Vec::with_capacity(s.len() + 1);
            l.extend_from_slice(s.as_bytes());
            l.push(b'\n');
            emit(&l);
        }

        fn print_prompt(&mut self) {
            print(b"frameos$ ");
        }

        fn print_goodbye(&mut self) {
            // ish exits without a banner; the FSM's $Exiting + is_done() is what
            // stops the loop (see _start at M3b.3).
        }

        fn tick(&mut self) {
            // Reap finished background jobs (JobControl polls each via the
            // SyscallProcessBackend's per-pid reap), then report + remove the
            // ones now done — ish's bash-style "[id]+ Done   cmd" at the prompt.
            self.jobs.tick();
            for s in self.jobs.jobs() {
                if s.state == "Done" || s.state.starts_with("Failed") {
                    Self::emit_job_line(b"[", s.id, b"]+ Done   ", &s.cmd, b"\n");
                    self.jobs.remove(s.id);
                }
            }
        }

        fn classify(&self, words: &[String]) -> CommandKind {
            // exit/quit are intercepted by the FSM before classify(). The rest
            // split into ish's builtins vs disk programs.
            match words[0].as_str() {
                "fg" => CommandKind::Fg(words.get(1).cloned()),
                "help" | "jobs" | "bg" | "kill" | "cd" | "pwd" | "clear" | "cat" => {
                    CommandKind::Builtin
                }
                _ => CommandKind::External,
            }
        }

        fn run_builtin(
            &mut self,
            words: &[String],
            _redir_out: Option<String>,
            _append: bool,
            _history: &[String],
        ) {
            // ish builtins don't honor redirection (only externals do) — matches
            // pre-M3b ish; builtin redirection isn't exercised by console-test.
            match words[0].as_str() {
                "help" => {
                    print(b"ish builtins: help, exit, cd [dir], pwd, clear, cat <path>..., jobs, fg [id], bg [id], kill %<job>|<pid>\n");
                    print(b"on disk in /bin: ls, echo, rm, cp, touch, wc, head, tail, grep, date, mkdir, rmdir, mv, ps, ...\n");
                    print(b"redirection (external cmds): cmd > file, cmd >> file, cmd < file\n");
                    print(b"pipes: cmd1 | cmd2 (connect cmd1's stdout to cmd2's stdin)\n");
                    print(b"background: cmd &   (jobs/fg/bg manage; Ctrl-Z stops the foreground job, Ctrl-C interrupts)\n");
                }
                "jobs" => {
                    // ish's "[id] State pid cmd" listing, over JobControl's snapshot.
                    for s in self.jobs.jobs() {
                        let mut l = Vec::new();
                        l.push(b'[');
                        push_u64(&mut l, s.id as u64);
                        l.extend_from_slice(b"] ");
                        if s.state == "Stopped" {
                            l.extend_from_slice(b"Stopped ");
                        } else if s.state == "Done" || s.state.starts_with("Failed") {
                            l.extend_from_slice(b"Done    ");
                        } else {
                            l.extend_from_slice(b"Running ");
                        }
                        push_u64(&mut l, s.pid as u64);
                        l.push(b' ');
                        l.extend_from_slice(s.cmd.as_bytes());
                        l.push(b'\n');
                        emit(&l);
                    }
                }
                "bg" => {
                    // Resolve %N (or default to the highest stopped job), SIGCONT
                    // it into the background, echo "[id] cmd &".
                    let id = if let Some(a) = words.get(1) {
                        let spec = a.strip_prefix('%').unwrap_or(a);
                        parse_u32(spec).unwrap_or(0)
                    } else {
                        let mut best = 0u32;
                        for s in self.jobs.jobs() {
                            if s.state == "Stopped" && s.id > best {
                                best = s.id;
                            }
                        }
                        best
                    };
                    let mut cmd = String::new();
                    for s in self.jobs.jobs() {
                        if s.id == id && s.state != "Done" {
                            cmd = s.cmd.clone();
                            break;
                        }
                    }
                    if self.live_pid(id) == 0 {
                        print(b"bg: no such job\n");
                        return;
                    }
                    self.jobs.bg(id);
                    Self::emit_job_line(b"[", id, b"] ", &cmd, b" &\n");
                }
                "kill" => {
                    // [-SIG] %<job>|<pid>; default SIGTERM. %N → pid via snapshot.
                    let mut idx = 1;
                    let mut sig = 15u64;
                    if idx < words.len() && words[idx].starts_with('-') {
                        match parse_signal(&words[idx][1..]) {
                            Some(s) => sig = s,
                            None => {
                                print(b"kill: bad signal: ");
                                print(words[idx].as_bytes());
                                write_char(b'\n');
                                return;
                            }
                        }
                        idx += 1;
                    }
                    let arg = match words.get(idx) {
                        Some(a) => a.as_str(),
                        None => {
                            print(b"kill: usage: kill [-SIG] %<job> | <pid>\n");
                            return;
                        }
                    };
                    let pid = if let Some(spec) = arg.strip_prefix('%') {
                        let id = match parse_u32(spec) {
                            Some(n) => n,
                            None => {
                                print(b"kill: bad job spec\n");
                                return;
                            }
                        };
                        let p = self.live_pid(id);
                        if p == 0 {
                            print(b"kill: no such job\n");
                            return;
                        }
                        p as u64
                    } else {
                        match parse_u32(arg) {
                            Some(n) => n as u64,
                            None => {
                                print(b"kill: not a pid: ");
                                print(arg.as_bytes());
                                write_char(b'\n');
                                return;
                            }
                        }
                    };
                    if kill(pid, sig) == u64::MAX {
                        print(b"kill: no such process\n");
                    }
                }
                "cd" => {
                    let target = if words.len() > 1 {
                        words[1].as_str()
                    } else {
                        "/"
                    };
                    if chdir(target.as_bytes()) == u64::MAX {
                        print(b"cd: no such directory: ");
                        print(target.as_bytes());
                        write_char(b'\n');
                    }
                }
                "pwd" => {
                    let mut buf = [0u8; 256];
                    let n = getcwd(&mut buf);
                    if n != u64::MAX {
                        let mut l = Vec::new();
                        l.extend_from_slice(&buf[..n as usize]);
                        l.push(b'\n');
                        emit(&l);
                    }
                }
                "clear" => print(b"\x1b[2J\x1b[H"),
                "cat" => {
                    for path in &words[1..] {
                        cat(path);
                    }
                }
                _ => {}
            }
        }

        fn run_foreground_redirected(&mut self, cmd: &Command) {
            // Redirected commands bypass JobControl (same as the hosted side);
            // run synchronously, native.
            let redir_in = cmd.redir_in.clone();
            let redir_out = cmd.redir_out.clone().map(|f| (f, cmd.append));
            run_external_untracked(&cmd.words, &redir_in, &redir_out, false);
        }

        fn spawn_background(&mut self, cmd: &Command) {
            let has_redir = cmd.redir_in.is_some() || cmd.redir_out.is_some();
            if has_redir {
                // Redirected background: native + untracked (rare; not in
                // console-test). Matches the hosted spawn_background_redirected.
                let redir_in = cmd.redir_in.clone();
                let redir_out = cmd.redir_out.clone().map(|f| (f, cmd.append));
                run_external_untracked(&cmd.words, &redir_in, &redir_out, true);
                return;
            }
            let c = cmd.words[0].clone();
            let a = cmd.words[1..].to_vec();
            self.jobs.spawn_background(c, a);
            // Echo "[id] pid" (bash-style). The new bg job's id is next_id - 1.
            let id = self.jobs.next_job_id().saturating_sub(1);
            let mut pid = 0u32;
            for s in self.jobs.jobs() {
                if s.id == id {
                    pid = s.pid;
                    break;
                }
            }
            let mut l = Vec::new();
            l.push(b'[');
            push_u64(&mut l, id as u64);
            l.extend_from_slice(b"] ");
            push_u64(&mut l, pid as u64);
            l.push(b'\n');
            emit(&l);
        }

        fn spawn_foreground(&mut self, words: &[String]) {
            // A plain foreground external is now a real Job (via JobControl over
            // the SyscallProcessBackend) — the shared FSM, with a tentative id.
            let c = words[0].clone();
            let a = words[1..].to_vec();
            self.jobs.spawn_foreground(c, a);
        }

        fn fg(&mut self, id: u32) -> bool {
            self.jobs.fg(id);
            self.jobs.is_running_foreground()
        }

        fn run_pipeline(&mut self, commands: &[Command]) {
            // ish wires exactly two external stages (S6); reject 3+ explicitly.
            if commands.len() > 2 {
                self.println("ish: only 2-stage pipelines are supported");
                return;
            }
            run_pipeline(&commands[0].words, &commands[1].words);
        }

        fn wait_foreground(&mut self) {
            // Block on the foreground Job (JobControl → Job.await_rest →
            // SyscallProcessBackend's blocking waitpid). Then present ish's
            // result: a Ctrl-Z'd job becomes a tracked stopped job ("[id]+
            // Stopped   cmd"); a completed one is removed (no Done notice for a
            // foreground command — only background jobs get one, via tick()).
            let fid = self.jobs.foreground_id();
            self.jobs.wait_foreground();
            let mut stopped = false;
            let mut cmd = String::new();
            for s in self.jobs.jobs() {
                if s.id == fid {
                    if s.state == "Stopped" {
                        stopped = true;
                        cmd = s.cmd.clone();
                    }
                    break;
                }
            }
            if stopped {
                Self::emit_job_line(b"[", fid, b"]+ Stopped   ", &cmd, b"\n");
            } else {
                self.jobs.remove(fid);
            }
        }
    }

    /// The environment a freshly-created ring-3 `Shell` gets (the generated
    /// shell.rs domain init calls this via `use super::*`).
    pub fn default_env() -> Box<dyn ShellEnv> {
        Box::new(IshShellEnv::new())
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    mem::init();
    print(b"\nFrame OS interactive shell (ish). Type 'help'.\n");
    // M3b.3: the REPL is now driven by the SAME frame/shell.frs control-flow FSM
    // the hosted shell runs — compiled for ring 3 (shell_fsm). The FSM owns the
    // loop's structure: $Prompting.$> reaps background jobs (IshShellEnv::tick)
    // + prints the prompt, $Parsing tokenizes (Parser) + parses (Pipeline) +
    // classifies + dispatches through IshShellEnv (builtins / externals /
    // pipelines / fg-bg / wait), and `exit`/`quit` reach the terminal $Exiting.
    // Shell::__create() runs the first $Prompting enter (the first prompt)
    // before we read input. ish keeps owning execution (fork/exec/dup2/pipe via
    // syscalls) inside IshShellEnv — FSM-owns-logic / native-owns-mechanism.
    let mut shell = shell_fsm::Shell::__create();
    let mut buf = [0u8; 256];
    loop {
        let n = read_line(&mut buf);
        match core::str::from_utf8(&buf[..n]) {
            Ok(line) => shell.line(line),
            Err(_) => shell.line(""),
        }
        // `exit` / `quit` drove the FSM to $Exiting.
        if shell.is_done() {
            break;
        }
    }
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

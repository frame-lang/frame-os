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

// Parser/Pipeline/Token/TokenKind are used inside the shell_fsm module (which
// imports them from frame_systems directly); ish root only needs IshJobs (the
// job-table type its execution helpers + IshShellEnv use).
use frame_systems::IshJobs;

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
/// reap_nohang (#28): non-blocking reap of *any* exited child (POSIX
/// `waitpid(-1, WNOHANG)`). Returns `(pid << 32) | (status as u32)`, or 0 if no
/// child has exited yet (pid 0 is never a real child, so 0 is an unambiguous
/// "nothing to harvest"). The shell loops on this at the prompt to collect
/// finished `&` background jobs without blocking. Unlike `waitpid`, this never
/// stalls the prompt waiting on a still-running job.
fn reap_nohang() -> u64 {
    unsafe { syscall3(28, 0, 0, 0) }
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

fn run_external(
    toks: &[String],
    redir_in: &Option<String>,
    redir_out: &Option<(String, bool)>,
    background: bool,
    cmd_str: &str,
    jobs: &mut IshJobs,
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
    } else if background {
        // Backgrounded (`cmd &`): record the child in the IshJobs FSM and return
        // to the prompt *without* waiting. The FSM assigns a job id; echo the
        // `[id] pid` line bash-style. The child is harvested later by the
        // non-blocking reap sweep that runs before each prompt (see harvest()).
        jobs.launch_bg(child, String::from(cmd_str));
        let mut l = Vec::new();
        l.push(b'[');
        push_u64(&mut l, jobs.last_id() as u64);
        l.extend_from_slice(b"] ");
        push_u64(&mut l, child);
        l.push(b'\n');
        emit(&l);
    } else {
        // Foreground: drive the FSM into $Foreground for the duration, register
        // the child as the foreground job (so Ctrl-C/Ctrl-Z reach it), and wait
        // for *this* child specifically. Waiting on the exact pid (not `wait`-any)
        // keeps the shell from racing ahead of the command it just launched. The
        // FSM's $Foreground state encodes this window: the shell is blocked here
        // and won't touch the job table until fg_done().
        jobs.launch_fg();
        set_foreground(child);
        let st = waitpid(child);
        set_foreground(0);
        jobs.fg_done();
        if st & WSTOPPED != 0 {
            // Ctrl-Z (or SIGSTOP) suspended the job rather than ending it. It
            // becomes a tracked stopped background job, bash-style, so `jobs` /
            // `bg` / `fg` can manage it.
            jobs.launch_stopped(child, String::from(cmd_str));
            let mut l = Vec::new();
            l.push(b'[');
            push_u64(&mut l, jobs.last_id() as u64);
            l.extend_from_slice(b"]+ Stopped   ");
            l.extend_from_slice(cmd_str.as_bytes());
            l.push(b'\n');
            emit(&l);
        }
    }
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

/// Harvest finished background (`&`) jobs without blocking. Called once before
/// every prompt: loop the non-blocking reap syscall to collect every child that
/// has exited since the last prompt, tell the IshJobs FSM (mark_done), then print
/// a bash-style `[id]+ Done   cmd` line for each and drop the entry. Reaping at
/// the prompt — and only at the prompt — is exactly what the FSM's $Idle state
/// represents; while a foreground job runs we're parked in waitpid ($Foreground)
/// and never get here.
fn harvest(jobs: &mut IshJobs) {
    loop {
        let v = reap_nohang();
        if v == 0 {
            break;
        }
        let pid = v >> 32;
        let code = (v & 0xffff_ffff) as u32 as i32;
        jobs.mark_done(pid, code);
    }
    // Report + remove every entry now flagged done. snapshot() is a clone, so
    // calling jobs.remove() inside the loop doesn't disturb the iteration.
    for e in jobs.snapshot() {
        if e.done {
            let mut l = Vec::new();
            l.push(b'[');
            push_u64(&mut l, e.id as u64);
            l.extend_from_slice(b"]+ Done   ");
            l.extend_from_slice(e.cmd.as_bytes());
            l.push(b'\n');
            emit(&l);
            jobs.remove(e.id);
        }
    }
}

/// Resolve a job-spec argument (`%N`, bare `N`, or None=most-recent) to a job id.
/// `stopped_only` picks the highest *stopped* job when no argument is given
/// (bash's `bg`); otherwise the highest non-done job (bash's `fg`). 0 = none.
fn resolve_job_id(arg: Option<&str>, jobs: &mut IshJobs, stopped_only: bool) -> u32 {
    if let Some(s) = arg {
        let spec = s.strip_prefix('%').unwrap_or(s);
        return parse_u32(spec).unwrap_or(0);
    }
    let mut best = 0u32;
    for e in &jobs.snapshot() {
        if !e.done && (!stopped_only || e.stopped) && e.id > best {
            best = e.id;
        }
    }
    best
}

// (fg_job was retired at M3b.3 — its resume-then-wait logic is now split across
// IshShellEnv::fg (find job + resume + set_foreground) and
// IshShellEnv::wait_foreground (the blocking waitpid + stop/exit reporting),
// mapping ish's synchronous model onto the Shell FSM's spawn/wait phases.
// `fg <id>` now takes a plain numeric id, like the hosted shell; bg/kill keep
// the `%N` job-spec form via resolve_job_id.)

/// `bg [id]`: resume a stopped job in the background (SIGCONT) and leave it
/// running there. It's harvested at a later prompt when it exits.
fn bg_job(arg: Option<&str>, jobs: &mut IshJobs) {
    let id = resolve_job_id(arg, jobs, true);
    let mut pid = 0u64;
    let mut cmd = String::new();
    for e in &jobs.snapshot() {
        if e.id == id && !e.done {
            pid = e.pid;
            cmd = e.cmd.clone();
            break;
        }
    }
    if pid == 0 {
        print(b"bg: no such job\n");
        return;
    }
    kill(pid, SIGCONT);
    jobs.mark_running(id);
    let mut l = Vec::new();
    l.push(b'[');
    push_u64(&mut l, id as u64);
    l.extend_from_slice(b"] ");
    l.extend_from_slice(cmd.as_bytes());
    l.extend_from_slice(b" &\n");
    emit(&l);
}

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

/// `kill [-SIG] %<job>|<pid>`: send a signal (default SIGTERM). `-SIG` is a name
/// (`-STOP`, `-CONT`, `-KILL`, ...) or number (`-9`). A `%N` target is a job spec
/// resolved to a pid through the IshJobs FSM snapshot; otherwise it's a raw pid.
fn kill_cmd(toks: &[String], jobs: &mut IshJobs) {
    let mut idx = 1;
    let mut sig = 15u64; // SIGTERM default
    if idx < toks.len() && toks[idx].starts_with('-') {
        match parse_signal(&toks[idx][1..]) {
            Some(s) => sig = s,
            None => {
                print(b"kill: bad signal: ");
                print(toks[idx].as_bytes());
                write_char(b'\n');
                return;
            }
        }
        idx += 1;
    }
    let arg = match toks.get(idx) {
        Some(a) => a.as_str(),
        None => {
            print(b"kill: usage: kill [-SIG] %<job> | <pid>\n");
            return;
        }
    };
    let pid = if let Some(spec) = arg.strip_prefix('%') {
        // Job spec: resolve %N → pid via the FSM's table.
        let id = match parse_u32(spec) {
            Some(n) => n,
            None => {
                print(b"kill: bad job spec\n");
                return;
            }
        };
        let mut p = 0u64;
        for e in &jobs.snapshot() {
            if e.id == id && !e.done {
                p = e.pid;
                break;
            }
        }
        if p == 0 {
            print(b"kill: no such job\n");
            return;
        }
        p
    } else {
        // Raw pid.
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

/// `jobs`: list tracked background jobs (id, state, pid, command). Each row is
/// emitted in one write() so an async kernel log can't split it.
fn list_jobs(jobs: &mut IshJobs) {
    for e in jobs.snapshot() {
        let mut l = Vec::new();
        l.push(b'[');
        push_u64(&mut l, e.id as u64);
        l.extend_from_slice(b"] ");
        if e.done {
            l.extend_from_slice(b"Done    ");
        } else if e.stopped {
            l.extend_from_slice(b"Stopped ");
        } else {
            l.extend_from_slice(b"Running ");
        }
        push_u64(&mut l, e.pid);
        l.push(b' ');
        l.extend_from_slice(e.cmd.as_bytes());
        l.push(b'\n');
        emit(&l);
    }
}

// (run_line — ish's hand-written Parser→Pipeline→dispatch loop — was retired at
// M3b.3. The control flow it encoded is now the shared frame/shell.frs FSM
// (shell_fsm below), and its per-command dispatch lives in IshShellEnv. ish's
// execution helpers above (run_external, run_pipeline, cat, the job builtins)
// are unchanged; IshShellEnv calls them.)

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

    // ish's native syscalls / builtins / job table — the mechanism IshShellEnv
    // drives (the same code the hand-written run_line used).
    use super::harvest;
    use super::{
        bg_job, build_argv, cat, chdir, emit, exec_argv, exit, fork, getcwd, kill, kill_cmd,
        list_jobs, print, push_u64, run_external, run_pipeline, set_foreground, waitpid,
        write_char, IshJobs, SIGCONT, WSTOPPED,
    };

    include!(concat!(env!("OUT_DIR"), "/shell.rs"));

    // The foreground command awaiting a wait_foreground(). ish executes
    // synchronously (fork then waitpid), so the Shell FSM's spawn/wait split
    // maps to: spawn_foreground/fg start the child (or resume a job) and stash
    // it here; wait_foreground (in $RunningForeground) does the blocking
    // waitpid + reports stop/exit — a faithful split, not a no-op wait.
    enum FgPending {
        /// A freshly fork+exec'd external. On stop → becomes a tracked job.
        Fresh { pid: u64, cmd: String },
        /// A resumed `fg <id>` job. On stop → re-mark stopped; on exit → remove.
        Resumed { pid: u64, id: u32, cmd: String },
    }

    /// Ring-3 environment for the shared Shell FSM: ish's syscalls + the IshJobs
    /// job table behind the ShellEnv seam.
    pub struct IshShellEnv {
        jobs: IshJobs,
        fg_pending: Option<FgPending>,
    }

    impl IshShellEnv {
        fn new() -> Self {
            Self {
                jobs: IshJobs::__create(),
                fg_pending: None,
            }
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
            harvest(&mut self.jobs);
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
                "jobs" => list_jobs(&mut self.jobs),
                "bg" => bg_job(words.get(1).map(|s| s.as_str()), &mut self.jobs),
                "kill" => kill_cmd(words, &mut self.jobs),
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
            let redir_in = cmd.redir_in.clone();
            let redir_out = cmd.redir_out.clone().map(|f| (f, cmd.append));
            let cmd_str = cmd.words.join(" ");
            run_external(
                &cmd.words,
                &redir_in,
                &redir_out,
                false,
                &cmd_str,
                &mut self.jobs,
            );
        }

        fn spawn_background(&mut self, cmd: &Command) {
            let redir_in = cmd.redir_in.clone();
            let redir_out = cmd.redir_out.clone().map(|f| (f, cmd.append));
            let cmd_str = cmd.words.join(" ");
            run_external(
                &cmd.words,
                &redir_in,
                &redir_out,
                true,
                &cmd_str,
                &mut self.jobs,
            );
        }

        fn spawn_foreground(&mut self, words: &[String]) {
            // Fork+exec now; block in wait_foreground (the $RunningForeground
            // state). No redirection on this path (that's run_foreground_redirected).
            let cmd_str = words.join(" ");
            let (argv, argc) = build_argv(words);
            let child = fork();
            if child == 0 {
                exec_argv(&argv, argc);
                print(b"ish: command not found: ");
                print(words[0].as_bytes());
                write_char(b'\n');
                exit(127);
            }
            self.jobs.launch_fg();
            set_foreground(child);
            self.fg_pending = Some(FgPending::Fresh {
                pid: child,
                cmd: cmd_str,
            });
        }

        fn fg(&mut self, id: u32) -> bool {
            let mut pid = 0u64;
            let mut was_stopped = false;
            let mut cmd = String::new();
            for e in &self.jobs.snapshot() {
                if e.id == id && !e.done {
                    pid = e.pid;
                    was_stopped = e.stopped;
                    cmd = e.cmd.clone();
                    break;
                }
            }
            if pid == 0 {
                return false;
            }
            // Echo the command being foregrounded (bash-style).
            self.println(&cmd);
            if was_stopped {
                kill(pid, SIGCONT);
                self.jobs.mark_running(id);
            }
            self.jobs.launch_fg();
            set_foreground(pid);
            self.fg_pending = Some(FgPending::Resumed { pid, id, cmd });
            true
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
            let pending = match self.fg_pending.take() {
                Some(p) => p,
                None => return,
            };
            match pending {
                FgPending::Fresh { pid, cmd } => {
                    let st = waitpid(pid);
                    set_foreground(0);
                    self.jobs.fg_done();
                    if st & WSTOPPED != 0 {
                        // Ctrl-Z'd: becomes a tracked stopped job (bash-style).
                        self.jobs.launch_stopped(pid, cmd.clone());
                        let mut l = Vec::new();
                        l.push(b'[');
                        push_u64(&mut l, self.jobs.last_id() as u64);
                        l.extend_from_slice(b"]+ Stopped   ");
                        l.extend_from_slice(cmd.as_bytes());
                        l.push(b'\n');
                        emit(&l);
                    }
                }
                FgPending::Resumed { pid, id, cmd } => {
                    let st = waitpid(pid);
                    set_foreground(0);
                    self.jobs.fg_done();
                    if st & WSTOPPED != 0 {
                        self.jobs.mark_stopped(id);
                        let mut l = Vec::new();
                        l.push(b'[');
                        push_u64(&mut l, id as u64);
                        l.extend_from_slice(b"]+ Stopped   ");
                        l.extend_from_slice(cmd.as_bytes());
                        l.push(b'\n');
                        emit(&l);
                    } else {
                        self.jobs.remove(id);
                    }
                }
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

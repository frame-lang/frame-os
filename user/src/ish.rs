// Frame OS interactive shell "ish" (B8).
//
// A real REPL, in ring 3: print a prompt, `read_line` from the console (the
// kernel echoes keystrokes + hands back a whole line), tokenize the line with the
// *same* `frame/parser.frs` FSM the hosted shell uses, then dispatch:
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

use frame_systems::{IshJobs, Parser};

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
/// Print an unsigned integer in decimal (no allocation). Used for job-control
/// echoes like `[1] 7` and the `jobs` listing.
fn print_u64(mut n: u64) {
    if n == 0 {
        write_char(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    print(&buf[i..]);
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
        write_char(b'[');
        print_u64(jobs.last_id() as u64);
        print(b"] ");
        print_u64(child);
        write_char(b'\n');
    } else {
        // Foreground: drive the FSM into $Foreground for the duration, wait for
        // *this* child specifically, then back to $Idle. Waiting on the exact pid
        // (not `wait`-any) is what keeps the shell from racing ahead of the
        // command it just launched — a bare `wait` could reap an unrelated older
        // child and return while this one is still running, so a following builtin
        // (e.g. `cat`) would see stale state. The FSM's $Foreground state encodes
        // exactly this window: the shell is blocked here and won't touch the job
        // table until fg_done().
        jobs.launch_fg();
        waitpid(child);
        jobs.fg_done();
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

/// Split tokens into (command words, input redirect, output redirect). The
/// redirection operators must be their own whitespace-separated tokens:
/// `< file`, `> file`, `>> file`. The operator + its filename are removed from
/// the returned command words; `>>` sets the append flag on the output redirect.
fn parse_redirs(toks: &[String]) -> (Vec<String>, Option<String>, Option<(String, bool)>) {
    let mut words: Vec<String> = Vec::new();
    let mut redir_in: Option<String> = None;
    let mut redir_out: Option<(String, bool)> = None;
    let mut i = 0;
    while i < toks.len() {
        match toks[i].as_str() {
            ">" | ">>" => {
                let append = toks[i].as_str() == ">>";
                if i + 1 < toks.len() {
                    redir_out = Some((toks[i + 1].clone(), append));
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "<" => {
                if i + 1 < toks.len() {
                    redir_in = Some(toks[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                words.push(toks[i].clone());
                i += 1;
            }
        }
    }
    (words, redir_in, redir_out)
}

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
            write_char(b'[');
            print_u64(e.id as u64);
            print(b"]+ Done   ");
            print(e.cmd.as_bytes());
            write_char(b'\n');
            jobs.remove(e.id);
        }
    }
}

/// `fg <id>`: bring a tracked background job to the foreground — block until it
/// finishes, then drop it from the table. The pid is looked up in the FSM's
/// snapshot; the FSM tracks the $Idle→$Foreground→$Idle window around the wait.
fn fg_job(arg: Option<&str>, jobs: &mut IshJobs) {
    let snap = jobs.snapshot();
    // Default to the most-recently-launched still-running job (highest id),
    // matching bash's `fg` with no argument.
    let id = match arg {
        Some(s) => match parse_u32(s) {
            Some(n) => n,
            None => {
                print(b"fg: not a job id: ");
                print(s.as_bytes());
                write_char(b'\n');
                return;
            }
        },
        None => {
            let mut best = 0u32;
            for e in &snap {
                if !e.done && e.id > best {
                    best = e.id;
                }
            }
            best
        }
    };
    let mut pid = 0u64;
    for e in &snap {
        if e.id == id && !e.done {
            pid = e.pid;
            break;
        }
    }
    if pid == 0 {
        print(b"fg: no such job\n");
        return;
    }
    jobs.launch_fg();
    waitpid(pid);
    jobs.fg_done();
    jobs.remove(id);
}

/// `kill %<job>` | `kill <pid>`: send SIGTERM. A `%N` argument is a job spec
/// resolved to a pid through the IshJobs FSM snapshot; otherwise the argument is
/// a raw pid. The killed background job is harvested at the next prompt (its
/// `[id]+ Done` line), same as a job that exits on its own.
fn kill_cmd(arg: Option<&str>, jobs: &mut IshJobs) {
    let arg = match arg {
        Some(a) => a,
        None => {
            print(b"kill: usage: kill %<job> | <pid>\n");
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
    if kill(pid, 15) == u64::MAX {
        print(b"kill: no such process\n");
    }
}

/// `jobs`: list tracked background jobs (id, state, pid, command).
fn list_jobs(jobs: &mut IshJobs) {
    for e in jobs.snapshot() {
        write_char(b'[');
        print_u64(e.id as u64);
        print(b"] ");
        if e.done {
            print(b"Done    ");
        } else {
            print(b"Running ");
        }
        print_u64(e.pid);
        write_char(b' ');
        print(e.cmd.as_bytes());
        write_char(b'\n');
    }
}

/// Tokenize one line with the Frame `Parser` FSM and dispatch the first token.
fn run_line(line: &str, jobs: &mut IshJobs) {
    let mut p = Parser::__create();
    for c in line.chars() {
        p.consume(c);
    }
    p.finalize();
    let mut raw: Vec<String> = p.tokens();
    if raw.is_empty() {
        return;
    }
    // Trailing `&` → run the command in the background (S10). Strip it here; the
    // remaining tokens are the command. `&` on its own line is a no-op.
    let background = raw.last().map(|t| t == "&").unwrap_or(false);
    if background {
        raw.pop();
        if raw.is_empty() {
            return;
        }
    }
    // Pipeline: a single `|` connects two external commands (S6). Handled before
    // redirection/builtins — `left | right` runs both with a pipe between them.
    // (Pipelines always run in the foreground at S10; a trailing `&` on a
    // pipeline is currently ignored.)
    if let Some(pos) = raw.iter().position(|t| t == "|") {
        let left = &raw[..pos];
        let right = &raw[pos + 1..];
        if left.is_empty() || right.is_empty() {
            print(b"ish: syntax error near '|'\n");
        } else {
            run_pipeline(left, right);
        }
        return;
    }
    // Strip I/O redirection (`> file`, `>> file`, `< file`) from the token list;
    // it's applied (for external commands) in the forked child before exec.
    let (toks, redir_in, redir_out) = parse_redirs(&raw);
    if toks.is_empty() {
        return;
    }
    // The command line as typed (sans `&`/redirs), for job-control reporting.
    let cmd_str = toks.join(" ");
    match toks[0].as_str() {
        "exit" => exit(0),
        "help" => {
            print(b"ish builtins: help, exit, cd [dir], pwd, clear, cat <path>..., jobs, fg [id], kill %<job>|<pid>\n");
            print(b"on disk in /bin: ls, echo, rm, cp, touch, wc, head, tail, grep, date, mkdir, rmdir, mv, ps, ...\n");
            print(b"redirection (external cmds): cmd > file, cmd >> file, cmd < file\n");
            print(b"pipes: cmd1 | cmd2 (connect cmd1's stdout to cmd2's stdin)\n");
            print(b"background: cmd &   (then `jobs` to list, `fg [id]` to foreground)\n");
        }
        // jobs / fg are builtins: they read+drive *this shell's* IshJobs FSM,
        // which lives in the shell process (forking would lose it).
        "jobs" => list_jobs(jobs),
        "fg" => fg_job(toks.get(1).map(|s| s.as_str()), jobs),
        "kill" => kill_cmd(toks.get(1).map(|s| s.as_str()), jobs),
        // cd must be a builtin: it changes *this shell's* cwd (per-process in the
        // kernel). No arg → go to root.
        "cd" => {
            let target = if toks.len() > 1 {
                toks[1].as_str()
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
                print(&buf[..n as usize]);
                write_char(b'\n');
            }
        }
        // clear: ANSI clear-screen + cursor-home. A builtin (no point forking).
        "clear" => print(b"\x1b[2J\x1b[H"),
        "cat" => {
            for path in &toks[1..] {
                cat(path);
            }
        }
        _ => run_external(&toks, &redir_in, &redir_out, background, &cmd_str, jobs),
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    mem::init();
    print(b"\nFrame OS interactive shell (ish). Type 'help'.\n");
    // The job-control FSM lives for the whole shell session, in the shell
    // process. It's the ish-resident JobControl (S10): $Idle at the prompt,
    // $Foreground while waiting on a job, with the background-job table in its
    // domain. fork() would lose it, so `jobs`/`fg`/`&` are all builtins.
    let mut jobs = IshJobs::__create();
    let mut buf = [0u8; 256];
    loop {
        // Harvest any background jobs that finished since the last prompt and
        // report them, before drawing the prompt (bash-style).
        harvest(&mut jobs);
        print(b"frameos$ ");
        let n = read_line(&mut buf);
        if let Ok(line) = core::str::from_utf8(&buf[..n]) {
            run_line(line, &mut jobs);
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

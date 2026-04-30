//! Process orchestration: fork/exec, pipes, redirection, process groups,
//! terminal control, signal handling, and background-job reaping.
//!
//! This is the most unsafe-heavy file because POSIX process management is
//! inherently outside Rust's safety model. The unsafe blocks are tiny and
//! commented; the surrounding code is plain Rust.

use crate::builtins::{is_builtin, run_builtin};
use crate::jobs::{add_job, drain_done, mark_pid_reaped};
use crate::parser::{Command, Connector, Pipeline, Script};

use nix::fcntl::{open, OFlag};
use nix::sys::signal::{
    sigaction, sigprocmask, SaFlags, SigAction, SigHandler, SigSet, SigmaskHow, Signal,
};
use nix::sys::stat::Mode;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{
    close, dup, dup2, execvp, fork, getpgrp, getpid, isatty, pipe, setpgid, tcgetpgrp,
    tcsetpgrp, ForkResult, Pid,
};

use std::ffi::CString;
use std::io::Read;
use std::os::fd::{FromRawFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

const STDIN_FD: RawFd = 0;
const STDOUT_FD: RawFd = 1;
const STDERR_FD: RawFd = 2;

static LAST_STATUS: AtomicI32 = AtomicI32::new(0);
static SHELL_PGID: AtomicI32 = AtomicI32::new(0);
static SHELL_INTERACTIVE: AtomicBool = AtomicBool::new(false);
static SIGCHLD_PENDING: AtomicBool = AtomicBool::new(false);

pub fn last_exit_status() -> i32 {
    LAST_STATUS.load(Ordering::Relaxed)
}

fn set_last_exit_status(v: i32) {
    LAST_STATUS.store(v, Ordering::Relaxed);
}

/// The result of running a single pipeline.
pub struct PipelineResult {
    pub status: i32,
    /// (command_summary, stderr_text) captured for Error Autopsy.
    /// Only populated for single-command foreground external pipelines where
    /// the user did not explicitly redirect stderr.
    pub autopsy: Option<(String, String)>,
}

/// Signal handler — must be async-signal-safe.
extern "C" fn sigchld_handler(_: libc::c_int) {
    SIGCHLD_PENDING.store(true, Ordering::Relaxed);
}

pub fn init_shell() {
    let interactive = isatty(STDIN_FD).unwrap_or(false);
    SHELL_INTERACTIVE.store(interactive, Ordering::Relaxed);

    let sa = SigAction::new(
        SigHandler::Handler(sigchld_handler),
        SaFlags::SA_RESTART | SaFlags::SA_NOCLDSTOP,
        SigSet::empty(),
    );
    // SAFETY: our handler only touches an AtomicBool, which is async-signal-safe.
    unsafe {
        sigaction(Signal::SIGCHLD, &sa).expect("sigaction SIGCHLD");
    }

    if interactive {
        loop {
            let my_pgid = getpgrp();
            match tcgetpgrp(STDIN_FD) {
                Ok(p) if p == my_pgid => break,
                _ => {
                    let _ = nix::sys::signal::killpg(my_pgid, Signal::SIGTTIN);
                }
            }
        }

        let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        // SAFETY: SIG_IGN is always safe to install.
        unsafe {
            for sig in [
                Signal::SIGINT,
                Signal::SIGQUIT,
                Signal::SIGTSTP,
                Signal::SIGTTIN,
                Signal::SIGTTOU,
            ] {
                let _ = sigaction(sig, &ignore);
            }
        }

        let pid = getpid();
        SHELL_PGID.store(pid.as_raw(), Ordering::Relaxed);
        let _ = setpgid(pid, pid);
        let _ = tcsetpgrp(STDIN_FD, pid);
    }
}

pub fn reap_jobs() {
    if SIGCHLD_PENDING.swap(false, Ordering::Relaxed) {
        loop {
            match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, _)) | Ok(WaitStatus::Signaled(pid, _, _)) => {
                    mark_pid_reaped(pid);
                }
                Ok(WaitStatus::StillAlive) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }
    for j in drain_done() {
        println!("[{}]+ Done       {}", j.id, j.cmd);
    }
}

/// Apply stdin/stdout/stderr redirections in the current process.
///
/// Stdout is applied before stderr so that `> file 2>&1` correctly routes
/// both streams to `file`. The unusual `2>&1 > file` ordering is not
/// distinguished — documented simplification.
fn apply_redirections(cmd: &Command) -> Result<(), String> {
    if let Some(f) = &cmd.infile {
        let fd = open(f.as_str(), OFlag::O_RDONLY, Mode::empty())
            .map_err(|e| format!("{}: {}", f, e))?;
        dup2(fd, STDIN_FD).map_err(|e| format!("dup2 stdin: {}", e))?;
        let _ = close(fd);
    }

    if let Some(f) = &cmd.outfile {
        let flags = OFlag::O_WRONLY
            | OFlag::O_CREAT
            | if cmd.append { OFlag::O_APPEND } else { OFlag::O_TRUNC };
        let fd = open(
            f.as_str(),
            flags,
            Mode::S_IRUSR | Mode::S_IWUSR | Mode::S_IRGRP | Mode::S_IROTH,
        )
        .map_err(|e| format!("{}: {}", f, e))?;
        dup2(fd, STDOUT_FD).map_err(|e| format!("dup2 stdout: {}", e))?;
        let _ = close(fd);
    }

    // Stderr — applied after stdout so `> file 2>&1` works.
    if cmd.err_to_out {
        dup2(STDOUT_FD, STDERR_FD).map_err(|e| format!("dup2 2>&1: {}", e))?;
    } else if let Some(f) = &cmd.errfile {
        let flags = OFlag::O_WRONLY
            | OFlag::O_CREAT
            | if cmd.err_append { OFlag::O_APPEND } else { OFlag::O_TRUNC };
        let fd = open(
            f.as_str(),
            flags,
            Mode::S_IRUSR | Mode::S_IWUSR | Mode::S_IRGRP | Mode::S_IROTH,
        )
        .map_err(|e| format!("{}: {}", f, e))?;
        dup2(fd, STDERR_FD).map_err(|e| format!("dup2 stderr: {}", e))?;
        let _ = close(fd);
    }

    Ok(())
}

fn make_summary(p: &Pipeline) -> String {
    p.commands
        .iter()
        .map(|c| c.argv.join(" "))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn close_all_pipes(pipes: &[(RawFd, RawFd)]) {
    for &(r, w) in pipes {
        let _ = close(r);
        let _ = close(w);
    }
}

/// Drain a readable fd into a String, closing it when done.
/// Deadlock-safe only when the writer side is already closed — call this
/// after waitpid so the child (the only writer) has exited.
fn drain_fd(fd: RawFd) -> String {
    // SAFETY: we own this fd and are the sole reader.
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s // File drop closes fd
}

/// Execute a full script: pipelines joined by `;` or `&&`.
///
/// Returns `(final_exit_status, autopsy_data)`.
/// `autopsy_data` is `Some((command, stderr))` for the most recent pipeline
/// that failed and had capturable stderr.
pub fn execute_script(script: &Script) -> (i32, Option<(String, String)>) {
    let mut status = 0i32;
    let mut last_autopsy: Option<(String, String)> = None;

    for (idx, pipeline) in script.pipelines.iter().enumerate() {
        if idx > 0 {
            if let Connector::And = &script.connectors[idx - 1] {
                if status != 0 {
                    continue; // short-circuit &&
                }
            }
        }

        let result = execute_pipeline(pipeline);
        status = result.status;
        set_last_exit_status(status);

        if status != 0 {
            // Keep autopsy data from this failure (may overwrite earlier one).
            if result.autopsy.is_some() {
                last_autopsy = result.autopsy;
            }
        } else {
            last_autopsy = None; // success clears any pending autopsy
        }
    }

    (status, last_autopsy)
}

/// Execute a single pipeline (one or more `|`-connected commands).
pub fn execute_pipeline(p: &Pipeline) -> PipelineResult {
    let no_result = |status| PipelineResult { status, autopsy: None };

    if p.commands.is_empty() {
        return no_result(0);
    }

    // Single foreground builtin runs in the shell process so cd/export persist.
    if p.commands.len() == 1 && !p.background && is_builtin(&p.commands[0].argv[0]) {
        let c = &p.commands[0];
        let saved_in  = if c.infile.is_some()                    { dup(STDIN_FD).ok()  } else { None };
        let saved_out = if c.outfile.is_some()                   { dup(STDOUT_FD).ok() } else { None };
        let saved_err = if c.errfile.is_some() || c.err_to_out   { dup(STDERR_FD).ok() } else { None };

        let rc = match apply_redirections(c) {
            Ok(()) => run_builtin(&c.argv),
            Err(e) => { eprintln!("msh: {}", e); 1 }
        };

        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        if let Some(fd) = saved_in  { let _ = dup2(fd, STDIN_FD);  let _ = close(fd); }
        if let Some(fd) = saved_out { let _ = dup2(fd, STDOUT_FD); let _ = close(fd); }
        if let Some(fd) = saved_err { let _ = dup2(fd, STDERR_FD); let _ = close(fd); }

        set_last_exit_status(rc);
        return no_result(rc); // builtins don't produce autopsy data
    }

    let n = p.commands.len();

    // Stderr capture: enabled only for single-command foreground external
    // pipelines where the user did not explicitly redirect stderr.
    // Deadlock risk is negligible for typical error messages (< OS pipe buffer).
    let capture_stderr = n == 1
        && !p.background
        && p.commands[0].errfile.is_none()
        && !p.commands[0].err_to_out;

    let capture_pipe: Option<(RawFd, RawFd)> = if capture_stderr {
        pipe().ok() // silently skip capture if pipe() fails
    } else {
        None
    };

    // Create the N-1 inter-stage pipes.
    let mut pipes: Vec<(RawFd, RawFd)> = Vec::new();
    for _ in 0..n.saturating_sub(1) {
        match pipe() {
            Ok(pp) => pipes.push(pp),
            Err(e) => {
                eprintln!("msh: pipe: {}", e);
                close_all_pipes(&pipes);
                if let Some((r, w)) = capture_pipe { let _ = close(r); let _ = close(w); }
                return no_result(1);
            }
        }
    }

    // Block SIGCHLD for foreground jobs so we can't lose a waitpid status.
    let mut block_set = SigSet::empty();
    block_set.add(Signal::SIGCHLD);
    let mut old_set = SigSet::empty();
    if !p.background {
        let _ = sigprocmask(SigmaskHow::SIG_BLOCK, Some(&block_set), Some(&mut old_set));
    }

    let mut pids: Vec<Pid> = Vec::with_capacity(n);
    let mut pgid: Option<Pid> = None;

    for i in 0..n {
        // Pass the write end of the capture pipe only to the first (and only)
        // child in a single-command pipeline.
        let child_capture_w = if i == 0 { capture_pipe.map(|(_, w)| w) } else { None };

        // SAFETY: single-threaded; fork() is safe here.
        let fork_res = unsafe { fork() };
        match fork_res {
            Ok(ForkResult::Child) => {
                child_after_fork(p, i, &pipes, pgid, &old_set, child_capture_w);
                // never returns
            }
            Ok(ForkResult::Parent { child }) => {
                if pgid.is_none() {
                    pgid = Some(child);
                }
                let _ = setpgid(child, pgid.unwrap()); // race-safe on both sides
                pids.push(child);
            }
            Err(e) => {
                eprintln!("msh: fork: {}", e);
                close_all_pipes(&pipes);
                if let Some((r, w)) = capture_pipe { let _ = close(r); let _ = close(w); }
                if !p.background {
                    let _ = sigprocmask(SigmaskHow::SIG_SETMASK, Some(&old_set), None);
                }
                return no_result(1);
            }
        }
    }

    // Parent: close write end of capture pipe — child is the sole writer.
    // Must happen before we read the read end, otherwise drain_fd never sees EOF.
    if let Some((_, w)) = capture_pipe {
        let _ = close(w);
    }
    close_all_pipes(&pipes);

    let pgid = pgid.unwrap();
    let mut status = 0i32;

    if p.background {
        let summary = make_summary(p);
        let jid = add_job(pgid, pids.clone(), summary);
        println!("[{}] {}", jid, pgid);
        // Discard capture pipe read-end for background jobs.
        if let Some((r, _)) = capture_pipe { let _ = close(r); }
        set_last_exit_status(0);
        return no_result(0);
    }

    // Foreground: give the terminal to the job, wait for every child.
    if SHELL_INTERACTIVE.load(Ordering::Relaxed) {
        let _ = tcsetpgrp(STDIN_FD, pgid);
    }

    for (i, &pid) in pids.iter().enumerate() {
        loop {
            match waitpid(pid, None) {
                Ok(WaitStatus::Exited(_, code)) => {
                    if i == pids.len() - 1 { status = code; }
                    break;
                }
                Ok(WaitStatus::Signaled(_, sig, _)) => {
                    if i == pids.len() - 1 { status = 128 + sig as i32; }
                    break;
                }
                Ok(_) => continue,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(_) => break,
            }
        }
    }

    if SHELL_INTERACTIVE.load(Ordering::Relaxed) {
        let shell_pgid = Pid::from_raw(SHELL_PGID.load(Ordering::Relaxed));
        let _ = tcsetpgrp(STDIN_FD, shell_pgid);
    }
    let _ = sigprocmask(SigmaskHow::SIG_SETMASK, Some(&old_set), None);
    set_last_exit_status(status);

    // Drain captured stderr — safe now that all children have exited.
    let autopsy = capture_pipe.and_then(|(r, _)| {
        let text = drain_fd(r); // closes r
        if status != 0 && !text.trim().is_empty() {
            Some((make_summary(p), text))
        } else {
            None
        }
    });

    PipelineResult { status, autopsy }
}

/// Code path inside the freshly-forked child. Never returns.
fn child_after_fork(
    p: &Pipeline,
    i: usize,
    pipes: &[(RawFd, RawFd)],
    pgid: Option<Pid>,
    old_set: &SigSet,
    capture_stderr_w: Option<RawFd>,
) -> ! {
    // Restore default signal dispositions.
    let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
    // SAFETY: SIG_DFL is always safe.
    unsafe {
        for sig in [
            Signal::SIGINT, Signal::SIGQUIT, Signal::SIGTSTP,
            Signal::SIGTTIN, Signal::SIGTTOU, Signal::SIGCHLD,
        ] {
            let _ = sigaction(sig, &dfl);
        }
    }
    let _ = sigprocmask(SigmaskHow::SIG_SETMASK, Some(old_set), None);

    let mypid = getpid();
    let target_pgid = pgid.unwrap_or(mypid);
    let _ = setpgid(mypid, target_pgid);

    let n = p.commands.len();

    // Wire inter-stage pipe fds.
    if i > 0 { let _ = dup2(pipes[i - 1].0, STDIN_FD); }
    if i < n - 1 { let _ = dup2(pipes[i].1, STDOUT_FD); }
    close_all_pipes(pipes);

    // Wire stderr capture pipe before apply_redirections.
    // apply_redirections won't touch stderr when errfile/err_to_out are unset
    // (which is the precondition for capture_stderr_w being Some).
    if let Some(w) = capture_stderr_w {
        let _ = dup2(w, STDERR_FD);
        let _ = close(w);
    }

    if let Err(e) = apply_redirections(&p.commands[i]) {
        eprintln!("msh: {}", e);
        flush_and_exit(1);
    }

    if is_builtin(&p.commands[i].argv[0]) {
        let rc = run_builtin(&p.commands[i].argv);
        flush_and_exit(rc);
    }

    let argv = &p.commands[i].argv;
    let prog = match CString::new(argv[0].as_str()) {
        Ok(c) => c,
        Err(_) => { eprintln!("msh: invalid program name"); flush_and_exit(127); }
    };
    let cargs: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(a.as_str()).unwrap_or_else(|_| CString::new("").unwrap()))
        .collect();

    match execvp(&prog, &cargs) {
        Ok(_) => unreachable!(),
        Err(e) => { eprintln!("msh: {}: {}", argv[0], e); flush_and_exit(127); }
    }
}

fn flush_and_exit(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: _exit never returns and skips destructors (correct post-fork).
    unsafe { libc::_exit(code) }
}

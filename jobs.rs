//! Background job table.
//!
//! Each job tracks ALL its child pids, not just the pgid. SIGCHLD reaps one
//! pid at a time; we mark the job done only when every pid in it has been
//! reaped. This matters for multi-stage background pipelines.

use nix::unistd::Pid;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Running,
    Done,
}

#[derive(Debug)]
pub struct Job {
    pub id: i32,
    #[allow(dead_code)]
    pub pgid: Pid,
    pub pids: Vec<Pid>, // un-reaped child pids
    pub cmd: String,
    pub state: JobState,
}

static JOBS: Mutex<Vec<Job>> = Mutex::new(Vec::new());
static NEXT_ID: Mutex<i32> = Mutex::new(1);

pub fn add_job(pgid: Pid, pids: Vec<Pid>, cmd: String) -> i32 {
    let mut jobs = JOBS.lock().unwrap();
    let mut id_lock = NEXT_ID.lock().unwrap();
    let id = *id_lock;
    *id_lock += 1;
    jobs.push(Job {
        id,
        pgid,
        pids,
        cmd,
        state: JobState::Running,
    });
    id
}

/// Called after we waitpid-reap a child. Removes that pid from any job's
/// pid list; if the list empties, marks the job Done.
pub fn mark_pid_reaped(pid: Pid) {
    let mut jobs = JOBS.lock().unwrap();
    for j in jobs.iter_mut() {
        if let Some(idx) = j.pids.iter().position(|p| *p == pid) {
            j.pids.swap_remove(idx);
            if j.pids.is_empty() {
                j.state = JobState::Done;
            }
            return;
        }
    }
}

/// Pull out and return all Done jobs (so the caller can print "Done" lines
/// outside any locked region).
pub fn drain_done() -> Vec<Job> {
    let mut jobs = JOBS.lock().unwrap();
    let mut done = Vec::new();
    let mut keep = Vec::new();
    for j in jobs.drain(..) {
        if j.state == JobState::Done {
            done.push(j);
        } else {
            keep.push(j);
        }
    }
    *jobs = keep;
    done
}

pub fn list_jobs() {
    let jobs = JOBS.lock().unwrap();
    for j in jobs.iter() {
        if j.state == JobState::Running {
            println!("[{}]  Running    {} &", j.id, j.cmd);
        }
    }
}

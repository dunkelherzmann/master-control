//! Commands the agent runs: each is a job whose output goes to files, so it can be followed while it runs and read
//! after it ends.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::audit::now_ms;
use crate::platform;

/// What to run.
pub struct Spec {
    /// A program and its arguments, run as given.
    pub argv: Vec<String>,
    /// Or a command line for the system's shell. Used when `argv` is empty.
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ended {
    Exited,
    /// Stopped by the `kill` tool.
    Killed,
    /// Stopped because it ran past its time limit.
    TimedOut,
}

#[derive(Default)]
struct Progress {
    exit_code: Option<i64>,
    ended_ms: Option<u64>,
    stop_reason: Option<Ended>,
}

pub struct Job {
    pub id: u64,
    pub pid: u32,
    pub command: String,
    pub started_ms: u64,
    folder: PathBuf,
    progress: Mutex<Progress>,
    changed: Condvar,
}

/// A job as the tools report it.
pub struct Snapshot {
    pub exit_code: Option<i64>,
    pub ended_ms: Option<u64>,
    pub ended: Option<Ended>,
}

impl Job {
    pub fn snapshot(&self) -> Snapshot {
        let progress = self.progress.lock().unwrap();
        Snapshot {
            exit_code: progress.exit_code,
            ended_ms: progress.ended_ms,
            ended: progress.exit_code.map(|_| progress.stop_reason.unwrap_or(Ended::Exited)),
        }
    }

    /// Waits until the job ends or the time is up. True when it has ended.
    pub fn wait(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut progress = self.progress.lock().unwrap();
        while progress.exit_code.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            progress = self.changed.wait_timeout(progress, deadline - now).unwrap().0;
        }
        true
    }

    /// Stops the job and everything it started. False when it had already ended.
    pub fn stop(&self, reason: Ended) -> bool {
        {
            let mut progress = self.progress.lock().unwrap();
            if progress.exit_code.is_some() {
                return false;
            }
            progress.stop_reason.get_or_insert(reason);
            // Still under the lock, so the pid can't have been given to another process yet: the thread that reaps
            // the child needs this lock to record the exit.
            platform::kill_tree(self.pid);
        }
        true
    }

    fn stream_path(&self, stream: Stream) -> PathBuf {
        self.folder.join(match stream {
            Stream::Stdout => "stdout.log",
            Stream::Stderr => "stderr.log",
        })
    }

    /// Up to `max` bytes of a stream from `from`, and the stream's size now.
    pub fn read(&self, stream: Stream, from: u64, max: usize) -> io::Result<(Vec<u8>, u64)> {
        let mut file = File::open(self.stream_path(stream))?;
        let size = file.metadata()?.len();
        let from = from.min(size);
        file.seek(SeekFrom::Start(from))?;
        let mut bytes = Vec::new();
        file.take(max as u64).read_to_end(&mut bytes)?;
        Ok((bytes, size))
    }

    /// The last `max` bytes of a stream, and whether earlier output was left out.
    pub fn tail(&self, stream: Stream, max: usize) -> io::Result<(Vec<u8>, bool)> {
        let size = fs::metadata(self.stream_path(stream))?.len();
        let from = size.saturating_sub(max as u64);
        let (bytes, _) = self.read(stream, from, max)?;
        Ok((bytes, from > 0))
    }
}

#[derive(Clone, Copy)]
pub enum Stream {
    Stdout,
    Stderr,
}

pub struct Jobs {
    folder: PathBuf,
    next: AtomicU64,
    jobs: Mutex<BTreeMap<u64, Arc<Job>>>,
}

impl Jobs {
    pub fn new(folder: PathBuf) -> io::Result<Jobs> {
        fs::create_dir_all(&folder)?;
        // Ids carry on from the output folders already there, so an agent that restarts never reuses one.
        let highest = fs::read_dir(&folder)?
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        Ok(Jobs { folder, next: AtomicU64::new(highest + 1), jobs: Mutex::new(BTreeMap::new()) })
    }

    pub fn start(&self, spec: Spec) -> io::Result<Arc<Job>> {
        let (mut command, shown) = if spec.argv.is_empty() {
            (platform::shell(&spec.command), spec.command.clone())
        } else {
            let mut command = Command::new(&spec.argv[0]);
            command.args(&spec.argv[1..]);
            (command, spec.argv.join(" "))
        };
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let folder = self.folder.join(id.to_string());
        fs::create_dir_all(&folder)?;
        command
            .current_dir(spec.cwd.unwrap_or_else(crate::state::user_home))
            .envs(spec.env)
            .stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(File::create(folder.join("stdout.log"))?)
            .stderr(File::create(folder.join("stderr.log"))?);
        platform::own_group(&mut command);
        let mut child = command.spawn()?;
        if let (Some(text), Some(mut pipe)) = (spec.stdin, child.stdin.take()) {
            // On its own thread: a child that never reads its input must not hold up the request.
            std::thread::spawn(move || {
                let _ = pipe.write_all(text.as_bytes());
            });
        }
        let job = Arc::new(Job {
            id,
            pid: child.id(),
            command: shown,
            started_ms: now_ms(),
            folder,
            progress: Mutex::new(Progress::default()),
            changed: Condvar::new(),
        });
        self.jobs.lock().unwrap().insert(id, job.clone());
        let reaped = job.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            let mut progress = reaped.progress.lock().unwrap();
            progress.exit_code = Some(status.map_or(-1, platform::exit_code));
            progress.ended_ms = Some(now_ms());
            reaped.changed.notify_all();
        });
        Ok(job)
    }

    pub fn get(&self, id: u64) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().get(&id).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Job>> {
        self.jobs.lock().unwrap().values().cloned().collect()
    }

    pub fn running(&self) -> usize {
        self.all().iter().filter(|job| job.snapshot().exit_code.is_none()).count()
    }
}

/// Writes a file through a temporary one beside it, so a reader never sees half of it.
pub fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path has no file name"))?;
    let mut temporary = name.to_os_string();
    temporary.push(format!(".mc-part-{}", std::process::id()));
    let temporary = path.with_file_name(temporary);
    let written = File::create(&temporary).and_then(|mut file| file.write_all(bytes)).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

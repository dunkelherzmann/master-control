//! The Master Control agent: runs on the machine under test, in the signed-in session, and serves the HTTP API and
//! MCP on the loopback interface (specs.md §2, §5).

#[cfg(target_os = "macos")]
#[macro_use]
extern crate objc;

mod apps;
mod audit;
pub mod cli;
mod control;
pub mod desktop;
mod http;
pub mod identity;
pub mod install;
mod jobs;
mod listen;
pub mod mcp;
pub mod observe;
mod platform;
mod state;
mod takeover;
mod tools;
pub mod worker;

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

pub use state::default_home;
pub use tools::ToolError;

/// Whether this program runs with admin or root rights.
pub fn elevated() -> bool {
    platform::elevated()
}

/// Every tool as MCP describes tools: name, description and the arguments' schema.
pub fn tool_list() -> Vec<Value> {
    tools::TOOLS.iter().map(|tool| serde_json::json!({ "name": tool.name, "description": tool.description, "inputSchema": (tool.schema)() })).collect()
}

pub const VERSION: &str = mc_core::VERSION;

/// The port the agent listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 47900;

pub struct Config {
    /// Where the agent keeps its token, audit log and jobs' output.
    pub home: PathBuf,
    /// 0 picks a free port.
    pub port: u16,
    /// Set to make this machine available to a master across the network.
    pub worker: Option<worker::WorkerConfig>,
}

/// Everything a request needs: the tools and what they share.
pub struct Agent {
    pub(crate) home: PathBuf,
    pub(crate) token: String,
    pub(crate) started: Instant,
    pub(crate) jobs: jobs::Jobs,
    pub(crate) audit: audit::Audit,
    pub(crate) captures: control::Captures,
    pub(crate) trees: control::Trees,
    pub(crate) sessions: observe::Sessions,
    /// The working folders of the sessions calling in: when each was last used, and whether its session ended.
    working: std::sync::Mutex<std::collections::HashMap<String, (Instant, bool)>>,
}

/// How long a session's working folder stays after its last call when the session never says it has ended.
pub const IDLE_WORKING_FOLDER: Duration = Duration::from_secs(24 * 3600);
/// How long a finished job's output is kept.
pub const KEEP_JOB_OUTPUT: Duration = Duration::from_secs(7 * 24 * 3600);
/// The working folder of calls that name no session: the agent's HTTP API used directly.
const SHARED_SESSION: &str = "shared";

impl Agent {
    pub fn new(home: PathBuf) -> io::Result<Arc<Agent>> {
        std::fs::create_dir_all(&home)?;
        state::restrict_to_user(&home)?;
        let token = state::load_or_create_token(&home)?;
        // Windows tells real input from the agent's own only to a program watching as it happens.
        desktop::watch_input();
        Ok(Arc::new(Agent {
            jobs: jobs::Jobs::new(home.join("jobs"))?,
            audit: audit::Audit::open(&home.join("audit.log"))?,
            captures: control::Captures::default(),
            trees: control::Trees::default(),
            sessions: observe::Sessions::new(home.join("sessions"))?,
            working: std::sync::Mutex::new(std::collections::HashMap::new()),
            home,
            token,
            started: Instant::now(),
        }))
    }

    /// Runs one tool and writes it to the audit log. `via` says how the request arrived: `http`, `mcp` or `stdio`.
    pub fn call(&self, via: &str, tool: &str, args: &Value) -> Result<Value, ToolError> {
        self.call_in(via, None, tool, args)
    }

    /// Runs one tool for a session: its commands run in the session's own working folder, where relative paths
    /// start too, and the folder goes when the session ends (`end_session`) or after a day unused.
    pub fn call_in(&self, via: &str, session: Option<&str>, tool: &str, args: &Value) -> Result<Value, ToolError> {
        let folder = self.working_folder_of(session)?;
        let began = Instant::now();
        let result = state::with_working_folder(folder, || tools::call(self, tool, args));
        self.audit.record(via, tool, args, result.as_ref().err(), began.elapsed());
        result
    }

    /// Runs `work` in a session's working folder (for requests that aren't tool calls, such as file uploads).
    pub(crate) fn in_working_folder<T>(&self, session: Option<&str>, work: impl FnOnce() -> T) -> Result<T, ToolError> {
        Ok(state::with_working_folder(self.working_folder_of(session)?, work))
    }

    fn working_folder_of(&self, session: Option<&str>) -> Result<PathBuf, ToolError> {
        let name = session.unwrap_or(SHARED_SESSION);
        if name.is_empty() || name.len() > 64 || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') {
            return Err(ToolError::bad("a session's name is letters, digits, - and _, at most 64"));
        }
        self.working.lock().unwrap().insert(name.to_string(), (Instant::now(), false));
        Ok(self.home.join("work").join(name))
    }

    /// The session is over: its working folder is removed, now or once the jobs running in it end.
    pub fn end_session(&self, session: &str) {
        if let Some(entry) = self.working.lock().unwrap().get_mut(session) {
            entry.1 = true;
        }
        takeover::end_session(&self.home, session);
        self.sweep();
    }

    /// Removes the working folders of sessions that ended or went unused for a day (and those of earlier runs as
    /// old as that), keeping any a job still runs in, and job output older than a week. Returns what it removed:
    /// working folders, then job folders.
    pub fn sweep(&self) -> (usize, usize) {
        self.sweep_older_than(IDLE_WORKING_FOLDER, KEEP_JOB_OUTPUT)
    }

    /// `sweep` with other ages, for tests.
    #[doc(hidden)]
    pub fn sweep_older_than(&self, idle: Duration, keep: Duration) -> (usize, usize) {
        let running: Vec<PathBuf> = self.jobs.all().into_iter().filter(|job| job.snapshot().exit_code.is_none()).map(|job| job.cwd.clone()).collect();
        let in_use = |folder: &std::path::Path| running.iter().any(|cwd| cwd.starts_with(folder));
        let root = self.home.join("work");
        let mut removed = 0;
        let mut working = self.working.lock().unwrap();
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let folder = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let done = match working.get(&name) {
                    Some((used, ended)) => *ended || used.elapsed() > idle,
                    None => state::older_than(&folder, idle),
                };
                if done && !in_use(&folder) && std::fs::remove_dir_all(&folder).is_ok() {
                    working.remove(&name);
                    removed += 1;
                }
            }
        }
        // Ended sessions with nothing left on disk need no more remembering.
        working.retain(|name, (used, ended)| !(*ended || used.elapsed() > idle) || root.join(name.as_str()).exists());
        drop(working);
        (removed, self.jobs.prune(keep))
    }

    /// Hands the machine back from sessions that were let in and have been quiet for a while (the person gets a
    /// pop-up tells them they can use it again). Run every few seconds by whatever keeps the agent going.
    pub fn hand_back_quiet(&self) {
        takeover::hand_back_quiet(&self.home, takeover::DONE_AFTER);
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

/// A listening agent.
pub struct Running {
    pub port: u16,
    /// The port the master reaches this machine on, in worker mode.
    pub worker_port: Option<u16>,
    pub agent: Arc<Agent>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    worker: Option<worker::Worker>,
}

impl Running {
    pub fn token(&self) -> &str {
        self.agent.token()
    }

    /// Blocks until the server stops.
    pub fn wait(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Stops listening, on the loopback address and for the master, and returns once both have stopped.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(worker) = self.worker.take() {
            worker.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Starts listening on the loopback interface and writes `agent.json` (port, pid, version) into the home folder.
pub fn start(config: Config) -> io::Result<Running> {
    let agent = Agent::new(config.home)?;
    let stop = Arc::new(AtomicBool::new(false));
    let (port, thread) = http::serve(agent.clone(), config.port, stop.clone())?;
    start_sweeping(agent.clone(), stop.clone());
    state::write_agent_file(&agent.home, port)?;
    let worker = match config.worker {
        Some(worker) => Some(worker::start(agent.clone(), worker, stop.clone())?),
        None => None,
    };
    Ok(Running { port, worker_port: worker.as_ref().map(|worker| worker.port), agent, stop, thread: Some(thread), worker })
}

/// Sweeps at once (what earlier runs left), then every hour until the agent stops.
fn start_sweeping(agent: Arc<Agent>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut last: Option<Instant> = None;
        while !stop.load(Ordering::SeqCst) {
            if last.is_none_or(|last| last.elapsed() >= Duration::from_secs(3600)) {
                agent.sweep();
                last = Some(Instant::now());
            }
            agent.hand_back_quiet();
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

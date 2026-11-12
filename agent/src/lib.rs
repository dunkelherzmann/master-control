//! The Master Control agent: runs on the machine under test, in the signed-in session, and serves the HTTP API and
//! MCP on the loopback interface (specs.md §2, §5).

#[cfg(target_os = "macos")]
#[macro_use]
extern crate objc;

mod audit;
pub mod cli;
mod control;
pub mod desktop;
mod http;
pub mod identity;
pub mod install;
mod jobs;
pub mod mcp;
pub mod observe;
mod platform;
mod state;
mod tools;
pub mod worker;

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

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
}

impl Agent {
    pub fn new(home: PathBuf) -> io::Result<Arc<Agent>> {
        std::fs::create_dir_all(&home)?;
        state::restrict_to_user(&home)?;
        let token = state::load_or_create_token(&home)?;
        Ok(Arc::new(Agent {
            jobs: jobs::Jobs::new(home.join("jobs"))?,
            audit: audit::Audit::open(&home.join("audit.log"))?,
            captures: control::Captures::default(),
            trees: control::Trees::default(),
            sessions: observe::Sessions::new(home.join("sessions"))?,
            home,
            token,
            started: Instant::now(),
        }))
    }

    /// Runs one tool and writes it to the audit log. `via` says how the request arrived: `http`, `mcp` or `stdio`.
    pub fn call(&self, via: &str, tool: &str, args: &Value) -> Result<Value, ToolError> {
        let began = Instant::now();
        let result = tools::call(self, tool, args);
        self.audit.record(via, tool, args, result.as_ref().err(), began.elapsed());
        result
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
    state::write_agent_file(&agent.home, port)?;
    let worker = match config.worker {
        Some(worker) => Some(worker::start(agent.clone(), worker, stop.clone())?),
        None => None,
    };
    Ok(Running { port, worker_port: worker.as_ref().map(|worker| worker.port), agent, stop, thread: Some(thread), worker })
}

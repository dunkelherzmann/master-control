//! Observation (specs.md §4): a session follows a target and everything it starts, and keeps what they do in an event
//! store the tools search.

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod mac_crash;
pub mod symbols;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod unwind;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod syscalls_x86_64;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod tracer;
#[cfg(windows)]
#[path = "etw.rs"]
pub mod tracer;
#[cfg(target_os = "macos")]
#[path = "mac.rs"]
pub mod tracer;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mc_core::{Class, Filter, Store};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit::now_ms;
use crate::tools::{number, text, ToolError};
use crate::{state, Agent};

/// Whether sessions can be started on this kind of system.
pub const BUILT_IN: bool = cfg!(any(windows, target_os = "macos", all(target_os = "linux", target_arch = "x86_64")));

/// What a session is asked to follow, as the agent hands it to the tracer.
#[derive(Serialize, Deserialize)]
pub struct Request {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    /// `off`: only what the other event classes are made from. `full`: every system call as an event of its own.
    pub syscalls: String,
    /// Folders the target may change. Empty: no limit.
    pub allow: Vec<String>,
    /// Files and folders that must not change.
    pub guard: Vec<String>,
    /// macOS: the target sets up a sandbox of its own, so it can't be run inside the reporting one.
    #[serde(default)]
    pub self_sandboxed: bool,
    /// Every event carries the call stack it was made from.
    #[serde(default)]
    pub stacks: bool,
    /// A program that is already running, to be followed from now on instead of starting `argv`.
    #[serde(default)]
    pub attach: Option<u32>,
    /// The only classes of events to keep. Empty: all.
    #[serde(default)]
    pub classes: Vec<String>,
    /// File and registry events are kept only under these paths. Empty: everywhere.
    #[serde(default)]
    pub include: Vec<String>,
    /// File and registry events under these paths are dropped.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Request {
    /// Whether an event is kept, by the session's filters. Problems always are: the filters thin out what is
    /// recorded, not what is checked.
    pub fn keeps(&self, event: &mc_core::Event) -> bool {
        if event.class == Class::Problem {
            return true;
        }
        if !self.classes.is_empty() && !self.classes.iter().any(|class| class == event.class.as_str()) {
            return false;
        }
        if matches!(event.class, Class::File | Class::Registry) {
            if !self.include.is_empty() && !self.include.iter().any(|path| covers(path, &event.subject)) {
                return false;
            }
            if self.exclude.iter().any(|path| covers(path, &event.subject)) {
                return false;
            }
        }
        true
    }
}

/// Whether a path is the given file, folder or registry key, or inside it. Whole names only: `/a/b` doesn't cover
/// `/a/bc`. On Windows either slash separates names and capitals don't matter, as there.
pub fn covers(folder: &str, path: &str) -> bool {
    let separator = |character: char| character == '/' || (cfg!(windows) && character == '\\');
    let same = |a: &str, b: &str| if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b };
    let mut inside = path.split(separator).filter(|name| !name.is_empty());
    folder.split(separator).filter(|name| !name.is_empty()).all(|name| inside.next().is_some_and(|other| same(name, other)))
}

struct Session {
    id: u64,
    directory: PathBuf,
    command: String,
    started_ms: u64,
    tracer: Mutex<Child>,
}

pub struct Sessions {
    folder: PathBuf,
    next: AtomicU64,
    sessions: Mutex<BTreeMap<u64, Arc<Session>>>,
}

impl Sessions {
    pub fn new(folder: PathBuf) -> io::Result<Sessions> {
        fs::create_dir_all(&folder)?;
        let highest = fs::read_dir(&folder)?.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u64>().ok()).max().unwrap_or(0);
        Ok(Sessions { folder, next: AtomicU64::new(highest + 1), sessions: Mutex::new(BTreeMap::new()) })
    }

    fn get(&self, args: &Value) -> Result<Arc<Session>, ToolError> {
        let id = number(args, "session")?.ok_or_else(|| ToolError::bad("session is required"))?;
        self.sessions.lock().unwrap().get(&id).cloned().ok_or_else(|| ToolError::not_found(format!("there is no session {id}")))
    }
}

impl Session {
    /// What the tracer last wrote about the session: running, exited, stopped or failed, with the target's pid and
    /// how it ended.
    fn state(&self) -> Value {
        let mut state: Value = fs::read(self.directory.join("state.json")).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_else(|| json!({ "state": "starting" }));
        let tracer_gone = self.tracer.lock().unwrap().try_wait().map_or(true, |status| status.is_some());
        if tracer_gone && matches!(state["state"].as_str(), Some("running" | "starting")) {
            // The tracer writes the end state before it exits; if it is gone without one, it died.
            state["state"] = json!("lost");
            state["error"] = json!("the tracer ended without finishing the session; see tracer.log in the session's folder");
        }
        state
    }

    fn store(&self) -> Result<Store, ToolError> {
        Store::open(&self.directory.join("events.sqlite")).map_err(|error| ToolError::failed(error.to_string()))
    }

    fn describe(&self) -> Value {
        let mut answer = self.state();
        answer["session"] = json!(self.id);
        answer["command"] = json!(self.command);
        answer["started_ms"] = json!(self.started_ms);
        answer
    }
}

fn strings(args: &Value, name: &str) -> Result<Vec<String>, ToolError> {
    match &args[name] {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items.iter().map(|item| item.as_str().map(str::to_string).ok_or_else(|| ToolError::bad(format!("{name} must be a list of text")))).collect(),
        _ => Err(ToolError::bad(format!("{name} must be a list of text"))),
    }
}

pub fn observe_start(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let argv = strings(args, "argv")?;
    let command = text(args, "command")?.unwrap_or_default();
    let attach = number(args, "pid")?;
    if attach.is_some_and(|pid| pid == 0 || pid > u64::from(u32::MAX)) {
        return Err(ToolError::bad("pid must be a running program's process id"));
    }
    let attach = attach.map(|pid| pid as u32);
    let argv = match (argv.is_empty(), command.is_empty(), attach) {
        (true, true, None) => return Err(ToolError::bad("give argv or command to start a program, or pid to follow one that is running")),
        (true, true, Some(_)) => Vec::new(),
        (_, _, Some(_)) => return Err(ToolError::bad("give pid, or argv or command, not both")),
        (false, false, None) => return Err(ToolError::bad("give argv or command, not both")),
        (false, true, None) => argv,
        (true, false, None) if cfg!(windows) => ["cmd", "/D", "/S", "/C", command].map(str::to_string).to_vec(),
        (true, false, None) => vec!["sh".to_string(), "-c".to_string(), command.to_string()],
    };
    if argv.first().is_some_and(|program| program.is_empty()) {
        return Err(ToolError::bad("argv's first item is the program and can't be empty"));
    }
    if attach.is_some() && (!args["cwd"].is_null() || !args["env"].is_null()) {
        return Err(ToolError::bad("cwd and env are for a program that is started, not one that is already running"));
    }
    let classes = strings(args, "classes")?;
    if let Some(unknown) = classes.iter().find(|class| Class::parse(class).is_none()) {
        return Err(ToolError::bad(format!("classes: there is no class {unknown}; they are {}", Class::ALL.map(Class::as_str).join(", "))));
    }
    let syscalls = text(args, "syscalls")?.unwrap_or("off");
    if !matches!(syscalls, "off" | "full") {
        return Err(ToolError::bad("syscalls is off or full"));
    }
    let env = match &args["env"] {
        Value::Null => Vec::new(),
        Value::Object(variables) => variables
            .iter()
            .map(|(name, value)| Ok((name.clone(), value.as_str().ok_or_else(|| ToolError::bad(format!("env.{name} must be text")))?.to_string())))
            .collect::<Result<_, ToolError>>()?,
        _ => return Err(ToolError::bad("env must be an object of text values")),
    };
    let cwd = text(args, "cwd")?.map(state::resolve);
    if let Some(folder) = cwd.as_ref().filter(|folder| !folder.is_dir()) {
        return Err(ToolError::not_found(format!("the working folder {} doesn't exist", folder.display())));
    }
    // Registry keys (HKCU\..., HKLM\...) are kept as written; everything else is a path on the machine.
    let is_key = |path: &str| cfg!(windows) && path.len() >= 3 && path[..2].eq_ignore_ascii_case("HK");
    let folders = |name: &str| {
        Ok::<_, ToolError>(strings(args, name)?.iter().map(|path| if is_key(path) { path.clone() } else { state::resolve(path).to_string_lossy().into_owned() }).collect::<Vec<_>>())
    };
    let request = Request {
        argv,
        cwd: Some(cwd.unwrap_or_else(state::user_home).to_string_lossy().into_owned()),
        env,
        syscalls: syscalls.to_string(),
        allow: folders("allow")?,
        guard: folders("guard")?,
        self_sandboxed: match &args["self_sandboxed"] {
            Value::Null => false,
            Value::Bool(given) => *given,
            _ => return Err(ToolError::bad("self_sandboxed is true or false")),
        },
        stacks: crate::tools::flag(args, "stacks")?,
        attach,
        classes,
        include: folders("include")?,
        exclude: folders("exclude")?,
    };
    if request.stacks && !cfg!(any(windows, all(target_os = "linux", target_arch = "x86_64"))) {
        return Err(ToolError { code: "not_supported", message: "a call stack with each event is only built for Linux and Windows so far; leave `stacks` out".into() });
    }
    if !BUILT_IN {
        return Err(ToolError { code: "not_supported", message: "observation isn't built for this system yet (Linux on x86-64, Windows and macOS only)".into() });
    }

    let sessions = &agent.sessions;
    let id = sessions.next.fetch_add(1, Ordering::SeqCst);
    let directory = sessions.folder.join(id.to_string());
    let failed = |what: &str, error: io::Error| ToolError::failed(format!("{what}: {error}"));
    fs::create_dir_all(&directory).map_err(|error| failed("the session's folder couldn't be made", error))?;
    fs::write(directory.join("request.json"), serde_json::to_vec(&request).expect("plain data")).map_err(|error| failed("the session couldn't be written", error))?;
    // The tracer is this same program, started with another command. Tests run inside a test program, so they say
    // where the real one is.
    let program = std::env::var_os("MC_AGENT_PROGRAM").map(PathBuf::from).map_or_else(std::env::current_exe, Ok).map_err(|error| failed("the agent's own program wasn't found", error))?;
    let log = fs::File::create(directory.join("tracer.log")).map_err(|error| failed("the tracer's log couldn't be made", error))?;
    let mut tracer = Command::new(&program);
    // Linux lets a program be traced only by the one that started it, unless told otherwise: following a program
    // that is already running then takes admin rights.
    #[cfg(target_os = "linux")]
    if request.attach.is_some() && tracer::attach_needs_root() {
        let may = Command::new("sudo").args(["-n", "true"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|status| status.success());
        if !may {
            let _ = fs::remove_dir_all(&directory);
            return Err(ToolError {
                code: "not_supported",
                message: "this system lets a program be traced only by the one that started it (kernel.yama.ptrace_scope), so following a running program needs admin rights: sudo without a password".into(),
            });
        }
        tracer = Command::new("sudo");
        tracer.arg("-n").arg(&program);
    }
    let tracer = tracer
        .arg("observe-run")
        .arg(&directory)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| failed("the tracer's log couldn't be shared", error))?)
        .stderr(log)
        .spawn()
        .map_err(|error| failed("the tracer couldn't be started", error))?;
    let command = request.attach.map_or_else(|| request.argv.join(" "), |pid| format!("(running, pid {pid})"));
    let session = Arc::new(Session { id, directory, command, started_ms: now_ms(), tracer: Mutex::new(tracer) });
    sessions.sessions.lock().unwrap().insert(id, session.clone());

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = session.state();
        match state["state"].as_str() {
            Some("starting") if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Some("failed" | "lost" | "starting") => {
                return Err(ToolError::failed(state["error"].as_str().unwrap_or("the tracer didn't start the target in time").to_string()));
            }
            _ => return Ok(session.describe()),
        }
    }
}

pub fn observe_stop(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let session = agent.sessions.get(args)?;
    {
        let mut tracer = session.tracer.lock().unwrap();
        if tracer.try_wait().is_ok_and(|status| status.is_none()) {
            // The tracer stops the target and everything it started, writes out what it holds, and ends.
            #[cfg(unix)]
            // SAFETY: a plain call on our own child's pid, which can't have been reused while we hold the Child.
            unsafe {
                libc::kill(tracer.id() as libc::pid_t, libc::SIGTERM);
            }
            // Windows has no such signal, and a tracer started through sudo can't be signalled by us: both watch
            // for this file.
            let _ = fs::write(session.directory.join("stop"), b"");
            let deadline = Instant::now() + Duration::from_secs(15);
            while tracer.try_wait().is_ok_and(|status| status.is_none()) {
                if Instant::now() > deadline {
                    let _ = tracer.kill();
                    let _ = tracer.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let mut answer = session.describe();
    let store = session.store()?;
    let problems = store.query(&Filter { classes: vec![Class::Problem], ..Filter::default() }).map_err(|error| ToolError::failed(error.to_string()))?;
    answer["verdict"] = json!(if problems.is_empty() && answer["state"] != "lost" { "ok" } else { "problem" });
    answer["problems"] = json!(problems);
    answer["summary"] = summarise(&store)?;
    Ok(answer)
}

fn summarise(store: &Store) -> Result<Value, ToolError> {
    let failed = |error: mc_core::StoreError| ToolError::failed(error.to_string());
    let listed = |class: Class, ops: &[&str]| -> Result<Vec<Value>, ToolError> {
        Ok(store.subjects(class, ops, 200).map_err(failed)?.into_iter().map(|(subject, count)| json!({ "subject": subject, "events": count })).collect())
    };
    let by_class: serde_json::Map<String, Value> = store.count_by_class().map_err(failed)?.into_iter().map(|(class, count)| (class, json!(count))).collect();
    Ok(json!({
        "events": by_class.values().filter_map(Value::as_u64).sum::<u64>(),
        "by_class": by_class,
        "programs_run": listed(Class::Process, &["exec"])?,
        "files_changed": listed(Class::File, &["write", "delete", "rename", "truncate", "create_dir", "delete_dir", "set_mode", "link"])?,
        "files_read": listed(Class::File, &["read"])?,
        "network": listed(Class::Network, &["connect", "bind", "accept"])?,
    }))
}

pub fn summary(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let session = agent.sessions.get(args)?;
    let mut answer = session.describe();
    answer["summary"] = summarise(&session.store()?)?;
    Ok(answer)
}

pub fn events(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let session = agent.sessions.get(args)?;
    let classes = match &args["class"] {
        Value::Null => Vec::new(),
        Value::String(name) => vec![name.clone()],
        _ => strings(args, "class")?,
    };
    let classes = classes
        .iter()
        .map(|name| Class::parse(name).ok_or_else(|| ToolError::bad(format!("{name} isn't an event class; they are {}", Class::ALL.map(Class::as_str).join(", ")))))
        .collect::<Result<Vec<_>, _>>()?;
    let limit = number(args, "limit")?.unwrap_or(200);
    if limit == 0 || limit > 2000 {
        return Err(ToolError::bad("limit must be between 1 and 2000"));
    }
    let offset = number(args, "offset")?.unwrap_or(0);
    let filter = Filter {
        classes,
        pid: number(args, "pid")?.map(|pid| pid as u32),
        op: text(args, "op")?.map(str::to_string),
        subject_contains: text(args, "subject")?.map(str::to_string),
        text: text(args, "text")?.map(str::to_string),
        from_ns: number(args, "from_ns")?,
        until_ns: number(args, "until_ns")?,
        outside: None,
        limit: Some(limit as u32),
        offset: offset.min(u64::from(u32::MAX)) as u32,
    };
    let store = session.store()?;
    let failed = |error: mc_core::StoreError| ToolError::failed(error.to_string());
    let found = store.query(&filter).map_err(failed)?;
    let total = store.count(&filter).map_err(failed)?;
    let next = offset + found.len() as u64;
    Ok(json!({ "session": session.id, "state": session.state()["state"], "total": total, "events": found, "next_offset": (next < total).then_some(next) }))
}

pub fn logs(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let session = agent.sessions.get(args)?;
    let stream = text(args, "stream")?.unwrap_or("stdout");
    if !matches!(stream, "stdout" | "stderr") {
        return Err(ToolError::bad("stream is stdout or stderr"));
    }
    let from = number(args, "from")?.unwrap_or(0);
    let path = session.directory.join(format!("{stream}.log"));
    let read = || -> io::Result<(Vec<u8>, u64)> {
        let mut file = fs::File::open(&path)?;
        let size = file.metadata()?.len();
        file.seek(SeekFrom::Start(from.min(size)))?;
        let mut bytes = Vec::new();
        file.take(256 * 1024).read_to_end(&mut bytes)?;
        Ok((bytes, size))
    };
    let (bytes, size) = read().map_err(|error| ToolError::failed(format!("the target's {stream} couldn't be read: {error}")))?;
    let next = from.min(size) + bytes.len() as u64;
    Ok(json!({ "session": session.id, "stream": stream, "text": String::from_utf8_lossy(&bytes), "next": next, "more": next < size }))
}

pub fn sessions(agent: &Agent, _args: &Value) -> Result<Value, ToolError> {
    let all: Vec<Arc<Session>> = agent.sessions.sessions.lock().unwrap().values().cloned().collect();
    Ok(json!({ "sessions": all.iter().map(|session| session.describe()).collect::<Vec<_>>() }))
}

#[cfg(test)]
mod tests {
    use super::covers;

    #[test]
    fn folders_cover_what_is_inside_them_by_whole_names() {
        assert!(covers("/home/a/app", "/home/a/app"));
        assert!(covers("/home/a/app", "/home/a/app/data/x.db"));
        assert!(covers("/home/a/app/", "/home/a/app/x"));
        assert!(!covers("/home/a/app", "/home/a/app.test/x"));
        assert!(!covers("/home/a/app", "/home/a/ap"));
        assert!(!covers("/home/a/app", "/home/a"));
        assert!(covers("/", "/anything"));
        assert_eq!(covers("/Home/A/app", "/home/a/APP/x"), cfg!(windows), "capitals matter except on Windows");
        assert_eq!(covers("HKCU\\Software\\App", "hkcu\\software\\app\\value"), cfg!(windows));
        assert!(!covers("HKCU\\Software\\App", "HKCU\\Software\\App.Test\\value"));
    }
}

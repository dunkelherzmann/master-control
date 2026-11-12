//! Observation on macOS, without administrator rights.
//!
//! The target runs under a sandbox profile that forbids nothing and asks the system to report what it allows: every
//! program started, every file opened for reading or changed, every network connection and every service looked up,
//! by the target and by everything it starts. The reports come back through the system log (`log stream`), each with
//! the pid that did it. Exits come from the kernel's process events, which also tell how each process ended.
//!
//! What this source doesn't give: how many bytes were read or written, calls that failed, the address behind a
//! connection (only its port), and system calls. A program that sets up a sandbox of its own (a browser, an App
//! Sandbox app) can't start inside this one.
//!
//! With administrator rights (sudo without a password) processes and files come from the system's Endpoint Security
//! events instead, read from `eslogger`: every start with its arguments, every exit with how it ended, files with
//! renames told as renames, in one ordered stream that numbers its events, so a gap shows what was lost. Of the
//! sandbox's reports only network connections and services are then used, and the sandbox can be left out for a
//! target that has its own. The profile stays the full one: a report that is the only one a process ever makes
//! often never arrives (seen on macOS 27.2), while the same report among others does.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mc_core::{Class, Event, Store};
use serde_json::{json, Value};

use super::{covers, Request};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop(_signal: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

const SANDBOX: &str = "/usr/bin/sandbox-exec";

/// Everything is allowed; the rules after the first only add a report. The last rule that matches wins.
const PROFILE: &str = "(version 1)
(allow default)
(allow file-read-data (with report))
(allow file-write* (with report))
(allow process-exec* (with report))
(allow process-fork (with report))
(allow network-outbound (with report))
(allow network-bind (with report))
(allow network-inbound (with report))
(allow mach-lookup (with report))
";

const EVENT_TYPES: [&str; 13] = ["exec", "fork", "exit", "open", "close", "create", "unlink", "rename", "truncate", "link", "setmode", "uipc_connect", "uipc_bind"];

fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos() as u64)
}

// ------------------------------------------------------------------------------------------------ reports

/// One line of the sandbox's reports: who was allowed to do what.
#[derive(Debug, PartialEq)]
struct Report {
    time_ns: u64,
    name: String,
    pid: u32,
    op: String,
    what: String,
}

/// `2026-11-11 15:13:15.373124+0100`, as the log writes it, in nanoseconds since 1970.
fn parse_time(text: &str) -> Option<u64> {
    let (date, rest) = text.split_once(' ')?;
    let zone_at = rest.rfind(['+', '-'])?;
    let (clock, zone) = rest.split_at(zone_at);
    let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let (whole, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut clock = whole.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    let nanoseconds = format!("{fraction:0<9}").get(..9)?.parse::<i64>().ok()?;
    let offset = {
        let digits = zone.get(1..5)?;
        let minutes = digits[..2].parse::<i64>().ok()? * 60 + digits[2..].parse::<i64>().ok()?;
        if zone.starts_with('-') { -minutes } else { minutes }
    };
    // Days since 1970 for a date of the usual calendar.
    let (year, month) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * month + 2) / 5 + day - 1;
    let days = era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset * 60;
    u64::try_from(seconds * 1_000_000_000 + nanoseconds).ok()
}

/// `Sandbox: cat(9451) allow file-read-data /tmp/a.txt` → the name, the pid, the operation and what it was done to.
fn parse_message(message: &str) -> Option<(String, u32, String, String)> {
    let body = message.strip_prefix("Sandbox: ")?;
    let at = body.find(") allow ")?;
    let (who, rest) = (&body[..at], &body[at + ") allow ".len()..]);
    let open = who.rfind('(')?;
    let pid = who[open + 1..].parse().ok()?;
    let (op, what) = rest.split_once(' ').unwrap_or((rest, ""));
    Some((who[..open].to_string(), pid, op.to_string(), what.trim_end().to_string()))
}

/// One Endpoint Security event, as `eslogger` writes it.
struct Kernel {
    time_ns: u64,
    pid: u32,
    tid: u32,
    kind: String,
    body: Value,
    /// The event's number among all this reader was sent: a jump means some were lost.
    number: u64,
}

fn parse_event(line: &str) -> Option<Kernel> {
    let mut entry: Value = serde_json::from_str(line).ok()?;
    let time_ns = parse_time(&entry["time"].as_str()?.replace('T', " ").replace('Z', "+0000"))?;
    let (kind, body) = entry["event"].as_object_mut()?.iter_mut().next().map(|(kind, body)| (kind.clone(), body.take()))?;
    Some(Kernel {
        time_ns,
        pid: entry["process"]["audit_token"]["pid"].as_u64()? as u32,
        tid: entry["thread"]["thread_id"].as_u64().unwrap_or(0) as u32,
        kind,
        body,
        number: entry["global_seq_num"].as_u64().unwrap_or(0),
    })
}

/// A file as an event names it: one that exists, or a name in a folder for one about to.
fn named(place: &Value) -> String {
    match place["existing_file"]["path"].as_str().or_else(|| place["path"].as_str()) {
        Some(path) => path.to_string(),
        None => format!("{}/{}", place["new_path"]["dir"]["path"].as_str().unwrap_or_default().trim_end_matches('/'), place["new_path"]["filename"].as_str().unwrap_or_default()),
    }
}

fn is_folder(file: &Value) -> bool {
    file["stat"]["st_mode"].as_u64().is_some_and(|mode| mode & 0o170000 == 0o040000)
}

fn parse_line(line: &str) -> Option<Report> {
    let entry: Value = serde_json::from_str(line).ok()?;
    let (name, pid, op, what) = parse_message(entry["eventMessage"].as_str()?)?;
    Some(Report { time_ns: parse_time(entry["timestamp"].as_str()?)?, name, pid, op, what })
}

// ------------------------------------------------------------------------------------------------ the system's view

struct Seen {
    parent: u32,
    name: String,
    started_ns: u64,
}

fn text_of(characters: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = characters.iter().take_while(|&&character| character != 0).map(|&character| character as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// What the system knows of a running process. None once it is gone.
fn look(pid: u32) -> Option<Seen> {
    // SAFETY: the structure is plain data the call fills; its size is given.
    let info = unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        (libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast::<c_void>(), size) == size).then_some(info)?
    };
    let name = Some(text_of(&info.pbi_name)).filter(|name| !name.is_empty()).unwrap_or_else(|| text_of(&info.pbi_comm));
    Some(Seen { parent: info.pbi_ppid, name, started_ns: info.pbi_start_tvsec * 1_000_000_000 + info.pbi_start_tvusec * 1_000 })
}

fn children(pid: u32) -> Vec<u32> {
    let mut pids = [0 as libc::c_int; 2048];
    // SAFETY: the buffer is ours and its size in bytes is given. What the call doesn't fill stays zero.
    unsafe { libc::proc_listchildpids(pid as libc::pid_t, pids.as_mut_ptr().cast::<c_void>(), std::mem::size_of_val(&pids) as libc::c_int) };
    pids.iter().filter(|&&pid| pid > 0).map(|&pid| pid as u32).collect()
}

/// The program a process runs and the arguments it was started with.
fn arguments(pid: u32) -> Option<(String, Vec<String>)> {
    let mut buffer = vec![0u8; 1 << 20];
    let mut size = buffer.len();
    let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    // SAFETY: the buffer is ours and `size` holds its length; the call writes at most that and sets what it wrote.
    if unsafe { libc::sysctl(name.as_mut_ptr(), 3, buffer.as_mut_ptr().cast::<c_void>(), &mut size, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    buffer.truncate(size);
    let count = i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?) as usize;
    let rest = &buffer[4..];
    let path_end = rest.iter().position(|&byte| byte == 0)?;
    let program = String::from_utf8_lossy(&rest[..path_end]).into_owned();
    let start = path_end + rest[path_end..].iter().position(|&byte| byte != 0)?;
    let argv: Vec<String> = rest[start..].split(|&byte| byte == 0).take(count).map(|argument| String::from_utf8_lossy(argument).into_owned()).collect();
    (argv.len() == count).then_some((program, argv))
}

fn signal_name(signal: i32) -> String {
    let names = ["", "SIGHUP", "SIGINT", "SIGQUIT", "SIGILL", "SIGTRAP", "SIGABRT", "SIGEMT", "SIGFPE", "SIGKILL", "SIGBUS", "SIGSEGV", "SIGSYS", "SIGPIPE", "SIGALRM", "SIGTERM"];
    names.get(signal as usize).filter(|name| !name.is_empty()).map_or_else(|| format!("signal {signal}"), |name| name.to_string())
}

fn is_crash(signal: i32) -> bool {
    matches!(signal, libc::SIGILL | libc::SIGTRAP | libc::SIGABRT | libc::SIGEMT | libc::SIGFPE | libc::SIGBUS | libc::SIGSEGV | libc::SIGSYS)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether two names are one process's: the system cuts long names short in some places.
fn same_name(a: &str, b: &str) -> bool {
    a == b || (a.len().min(b.len()) >= 15 && (a.starts_with(b) || b.starts_with(a)))
}

/// The real path of a file or folder that may not exist yet: reports carry real paths (`/private/tmp`, not `/tmp`).
fn real_path(path: &str) -> String {
    let path = Path::new(path);
    if let Ok(real) = path.canonicalize() {
        return real.to_string_lossy().into_owned();
    }
    match (path.parent().and_then(|parent| parent.canonicalize().ok()), path.file_name()) {
        (Some(parent), Some(name)) => parent.join(name).to_string_lossy().into_owned(),
        _ => path.to_string_lossy().into_owned(),
    }
}

/// Where the program of a command line is, the way the shell would find it.
fn find_program(request: &Request) -> Option<PathBuf> {
    let program = &request.argv[0];
    let usable = |path: PathBuf| path.is_file().then_some(path);
    if program.contains('/') {
        let folder = request.cwd.as_deref().map_or_else(PathBuf::new, PathBuf::from);
        return usable(folder.join(program));
    }
    let search = request.env.iter().rev().find(|(name, _)| name == "PATH").map(|(_, value)| value.clone()).or_else(|| std::env::var("PATH").ok()).unwrap_or_else(|| "/usr/bin:/bin".into());
    search.split(':').filter(|folder| !folder.is_empty()).find_map(|folder| usable(Path::new(folder).join(program)))
}

// ------------------------------------------------------------------------------------------------ the recorder

/// A child a process was reported making, not yet matched to the child's pid.
struct Fork {
    time_ns: u64,
    /// The program, when the child was started in one step with it.
    program: Option<String>,
}

struct Process {
    name: String,
    parent: Option<u32>,
    /// Still running, as far as the kernel has told.
    live: bool,
    /// Its start has been written.
    introduced: bool,
    /// Its exit has been written.
    ended: bool,
    started_ns: u64,
    last_ns: u64,
    forks: VecDeque<Fork>,
    last_was_fork: bool,
}

/// A process that ended by a fault, waiting for the system's report on it.
struct Crash {
    pid: u32,
    name: String,
    signal: String,
    time_ns: u64,
    until: Instant,
}

fn reports_folder() -> PathBuf {
    crate::state::user_home().join("Library").join("Logs").join("DiagnosticReports")
}

/// The system's report on a crash of this process, if it has been written: where it is, and the stack of the
/// thread that crashed, innermost frame first, as `file!function+0x12`.
fn crash_report(name: &str, pid: u32, since_ns: u64) -> Option<(PathBuf, Vec<String>)> {
    let folder = reports_folder();
    let since = UNIX_EPOCH + Duration::from_nanos(since_ns.saturating_sub(2_000_000_000));
    for entry in fs::read_dir(folder).ok()?.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        if !file.starts_with(&format!("{name}-")) || !file.ends_with(".ips") || !entry.metadata().and_then(|file| file.modified()).is_ok_and(|written| written >= since) {
            continue;
        }
        // One line that says what the report is, then the report itself.
        let Ok(text) = fs::read_to_string(entry.path()) else { continue };
        let Some(report) = text.split_once('\n').and_then(|(_, body)| serde_json::from_str::<Value>(body).ok()) else { continue };
        if report["pid"].as_u64() != Some(u64::from(pid)) {
            continue;
        }
        let images = report["usedImages"].as_array().cloned().unwrap_or_default();
        let crashed = report["threads"].as_array().and_then(|threads| threads.iter().find(|thread| thread["triggered"] == true))?;
        let stack = crashed["frames"]
            .as_array()?
            .iter()
            .map(|frame| {
                let image = frame["imageIndex"].as_u64().and_then(|index| images.get(index as usize)).and_then(|image| image["name"].as_str()).unwrap_or("?");
                match frame["symbol"].as_str() {
                    Some(symbol) => format!("{image}!{:#}+{:#x}", rustc_demangle::demangle(symbol), frame["symbolLocation"].as_u64().unwrap_or(0)),
                    None => format!("{image}+{:#x}", frame["imageOffset"].as_u64().unwrap_or(0)),
                }
            })
            .collect();
        return Some((entry.path(), stack));
    }
    None
}

struct Recorder {
    store: Store,
    pending: Vec<Event>,
    request: Request,
    processes: HashMap<u32, Process>,
    /// Other programs' pids seen in the reports, and the pids of our own markers.
    ignored: HashSet<u32>,
    arguments: HashMap<u32, (String, Vec<String>)>,
    queue: libc::c_int,
    reported: HashSet<(String, String)>,
    written: u64,
    root: u32,
    root_has_run: bool,
    /// Processes and files come from the system's events; the sandbox's reports only add network and services.
    by_events: bool,
    /// The time of the last system event taken in.
    clock: u64,
    last_number: Option<u64>,
    lost: u64,
    /// How the target itself ended.
    outcome: Value,
    /// Crashes whose report from the system is still awaited.
    crashes: Vec<Crash>,
    /// Crashes caught as they happened, with the stack read then: by process.
    caught: Option<Receiver<(u32, Vec<String>)>>,
    stacks: HashMap<u32, Vec<String>>,
    /// The target is started by the sandbox's launcher, whose own start is not the target's.
    through_launcher: bool,
}

impl Recorder {
    // One argument for each field of the event that differs from call to call.
    #[allow(clippy::too_many_arguments)]
    fn event(&mut self, time_ns: u64, pid: u32, class: Class, op: &str, subject: &str, args: Value, result: &str) {
        let mut event = Event::new(time_ns, pid, class, op, subject);
        event.process = self.processes.get(&pid).map(|process| process.name.clone()).unwrap_or_default();
        event.args = args;
        event.result = result.to_string();
        self.pending.push(event);
    }

    fn problem(&mut self, time_ns: u64, pid: u32, kind: &str, subject: &str, args: Value) {
        // Once per kind and subject: a program that writes a stray file a thousand times has one problem.
        if self.reported.insert((kind.to_string(), subject.to_string())) {
            self.event(time_ns, pid, Class::Problem, kind, subject, args, "");
        }
    }

    fn flush(&mut self) {
        let request = &self.request;
        self.pending.retain(|event| request.keeps(event));
        if self.pending.is_empty() {
            return;
        }
        if let Err(error) = self.store.insert(&self.pending) {
            eprintln!("observe-run: events were lost: {error}");
        } else {
            self.written += self.pending.len() as u64;
        }
        self.pending.clear();
    }

    /// A change to a file or folder: checked against what the session guards and what it allows.
    fn changed(&mut self, time_ns: u64, pid: u32, op: &str, path: &str) {
        if !path.starts_with('/') {
            return;
        }
        if self.request.guard.iter().any(|guarded| covers(guarded, path)) {
            self.problem(time_ns, pid, "guard_broken", path, json!({ "op": op }));
        } else if !self.request.allow.is_empty() && !self.request.allow.iter().any(|allowed| covers(allowed, path)) && !covers("/dev", path) {
            self.problem(time_ns, pid, "write_outside_allowed", path, json!({ "op": op }));
        }
    }

    /// Asks the kernel to tell when this process starts another, becomes another program, or ends. False when it is
    /// already gone.
    fn watch(&self, pid: u32) -> bool {
        let change = libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD,
            fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS | libc::NOTE_FORK | libc::NOTE_EXEC,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one change is handed in and no events are asked for.
        unsafe { libc::kevent(self.queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) == 0 }
    }

    fn live(&self) -> Vec<u32> {
        self.processes.iter().filter(|(_, process)| process.live).map(|(&pid, _)| pid).collect()
    }

    /// Takes in a process found running, and whatever it has started in the meantime.
    fn adopt(&mut self, pid: u32, parent: u32) {
        if self.processes.contains_key(&pid) || self.ignored.contains(&pid) {
            return;
        }
        let seen = look(pid);
        let now = now_ns();
        let process = Process {
            name: seen.as_ref().map(|seen| seen.name.clone()).unwrap_or_default(),
            parent: Some(parent),
            live: self.watch(pid),
            introduced: false,
            ended: false,
            started_ns: seen.map_or(now, |seen| seen.started_ns),
            last_ns: now,
            forks: VecDeque::new(),
            last_was_fork: false,
        };
        self.processes.insert(pid, process);
        if let Some(found) = arguments(pid) {
            self.arguments.insert(pid, found);
        }
        self.scan(pid);
    }

    fn scan(&mut self, pid: u32) {
        for child in children(pid) {
            self.adopt(child, pid);
        }
    }

    /// The fork of `parent` that made a child called `name`, taken off its list.
    fn claim(&mut self, parent: u32, name: &str) -> Option<Fork> {
        let process = self.processes.get_mut(&parent)?;
        let at = process
            .forks
            .iter()
            .position(|fork| fork.program.as_deref().is_some_and(|program| same_name(file_name(program), name)))
            .or_else(|| process.forks.iter().position(|fork| fork.program.is_none()))?;
        process.forks.remove(at)
    }

    /// The arguments a process was seen running `program` with.
    fn arguments_for(&self, pid: u32, program: &str) -> Value {
        match self.arguments.get(&pid) {
            Some((path, argv)) if file_name(path) == file_name(program) => json!({ "argv": argv }),
            _ => json!({}),
        }
    }

    /// Writes a process's start, and the program it was started with when its parent's reports told.
    fn introduce(&mut self, pid: u32, no_later_than: u64) {
        let Some(process) = self.processes.get(&pid) else { return };
        if process.introduced {
            return;
        }
        let (parent, name, started) = (process.parent, process.name.clone(), process.started_ns);
        let fork = parent.and_then(|parent| self.claim(parent, &name));
        let time = fork.as_ref().map_or(started, |fork| fork.time_ns).min(no_later_than);
        self.event(time, pid, Class::Process, "start", "", json!({ "parent": parent }), "ok");
        if let Some(program) = fork.and_then(|fork| fork.program) {
            let args = self.arguments_for(pid, &program);
            self.event(time + 1, pid, Class::Process, "exec", &program, args, "ok");
        }
        if let Some(process) = self.processes.get_mut(&pid) {
            process.introduced = true;
            process.started_ns = time;
        }
    }

    /// A pid the reports name that isn't known yet: ours when it descends from the target.
    fn recognise(&mut self, report: &Report) -> bool {
        if let Some(seen) = look(report.pid) {
            if self.processes.contains_key(&seen.parent) {
                self.adopt(report.pid, seen.parent);
                return true;
            }
            // Running, and its parent isn't ours — unless the parent is gone and it was handed to the system.
            if seen.parent != 1 {
                return false;
            }
        }
        // It ended before it was seen running, or lost its parent: the parent's report of making it tells.
        let made_it = |process: &Process| {
            process.forks.iter().filter(|fork| fork.time_ns <= report.time_ns).find_map(|fork| match &fork.program {
                Some(program) => same_name(file_name(program), &report.name).then_some((0, fork.time_ns)),
                None => same_name(&process.name, &report.name).then_some((1, fork.time_ns)),
            })
        };
        let Some((parent, _)) = self.processes.iter().filter_map(|(&pid, process)| made_it(process).map(|order| (pid, order))).min_by_key(|&(_, order)| order) else {
            return false;
        };
        let process = Process {
            name: report.name.clone(),
            parent: Some(parent),
            live: self.watch(report.pid),
            introduced: false,
            ended: false,
            started_ns: report.time_ns,
            last_ns: report.time_ns,
            forks: VecDeque::new(),
            last_was_fork: false,
        };
        self.processes.insert(report.pid, process);
        self.scan(report.pid);
        true
    }

    fn report(&mut self, report: Report) {
        let pid = report.pid;
        if self.ignored.contains(&pid) {
            return;
        }
        if self.by_events {
            // Who is who is known from the system's events; the reports only add what those don't tell.
            let known = self.processes.contains_key(&pid);
            let op = match report.op.as_str() {
                _ if !known || report.what.is_empty() => return,
                "network-outbound" => "connect",
                "network-bind" => "bind",
                "network-inbound" => "accept",
                "mach-lookup" => return self.event(report.time_ns, pid, Class::Ipc, "lookup", &report.what, json!({}), "ok"),
                _ => return,
            };
            let address = report.what.strip_prefix("remote:").or_else(|| report.what.strip_prefix("local:")).unwrap_or(&report.what);
            return self.event(report.time_ns, pid, Class::Network, op, address, json!({}), "ok");
        }
        if !self.processes.contains_key(&pid) && !self.recognise(&report) {
            self.ignored.insert(pid);
            return;
        }
        if let Some(process) = self.processes.get_mut(&pid) {
            // A child started in one step with its program is reported under its parent, with the parent's name.
            if !(report.op == "process-exec*" && process.last_was_fork) {
                process.name = report.name.clone();
            }
            process.last_ns = process.last_ns.max(report.time_ns);
        }
        self.introduce(pid, report.time_ns.saturating_sub(1));
        let time = report.time_ns;
        let was_fork = std::mem::replace(&mut self.processes.get_mut(&pid).expect("known").last_was_fork, report.op == "process-fork");
        match report.op.as_str() {
            "process-fork" => self.processes.get_mut(&pid).expect("known").forks.push_back(Fork { time_ns: time, program: None }),
            "process-exec*" => {
                let process = self.processes.get_mut(&pid).expect("known");
                match process.forks.back_mut().filter(|fork| was_fork && fork.program.is_none()) {
                    Some(fork) => fork.program = Some(report.what),
                    None => {
                        // The target itself is started by the sandbox's own launcher: its first program is ours.
                        let args = if pid == self.root && !std::mem::replace(&mut self.root_has_run, true) {
                            json!({ "argv": self.request.argv })
                        } else {
                            self.arguments_for(pid, &report.what)
                        };
                        self.event(time, pid, Class::Process, "exec", &report.what, args, "ok");
                        // Known at once, not only when its next report comes: it may end before that.
                        self.processes.get_mut(&pid).expect("known").name = file_name(&report.what).to_string();
                    }
                }
            }
            // Every program opens this as it loads; it says nothing about the program.
            _ if report.what == "/dev/dtracehelper" => {}
            "file-read-data" => self.event(time, pid, Class::File, "read", &report.what, json!({}), "ok"),
            "file-write-create" => {
                let op = if Path::new(&report.what).is_dir() { "create_dir" } else { "write" };
                self.event(time, pid, Class::File, op, &report.what, json!({ "created": true }), "ok");
                self.changed(time, pid, op, &report.what);
            }
            "file-write-data" => {
                self.event(time, pid, Class::File, "write", &report.what, json!({}), "ok");
                self.changed(time, pid, "write", &report.what);
            }
            "file-write-unlink" => {
                self.event(time, pid, Class::File, "delete", &report.what, json!({}), "ok");
                self.changed(time, pid, "delete", &report.what);
            }
            op if op.starts_with("file-write-") => {
                let op = format!("set_{}", &op["file-write-".len()..]).replace('-', "_");
                self.event(time, pid, Class::File, &op, &report.what, json!({}), "ok");
                self.changed(time, pid, &op, &report.what);
            }
            // Made a socket, with nowhere to go yet.
            "network-outbound" | "network-bind" | "network-inbound" if report.what.is_empty() => {}
            "network-outbound" | "network-bind" | "network-inbound" => {
                let op = match report.op.as_str() {
                    "network-outbound" => "connect",
                    "network-bind" => "bind",
                    _ => "accept",
                };
                let address = report.what.strip_prefix("remote:").or_else(|| report.what.strip_prefix("local:")).unwrap_or(&report.what);
                self.event(time, pid, Class::Network, op, address, json!({}), "ok");
            }
            "mach-lookup" => self.event(time, pid, Class::Ipc, "lookup", &report.what, json!({}), "ok"),
            other => self.event(time, pid, Class::Api, other, &report.what, json!({}), "ok"),
        }
    }

    /// One of the system's events: taken in when it is about the target or something it started.
    fn kernel(&mut self, event: Kernel) {
        if let Some(last) = self.last_number.replace(event.number) {
            self.lost += event.number.saturating_sub(last + 1);
        }
        self.clock = self.clock.max(event.time_ns);
        let (pid, time, body) = (event.pid, event.time_ns, &event.body);
        if !self.processes.contains_key(&pid) || self.processes[&pid].ended {
            return;
        }
        let path = |value: &Value| value["path"].as_str().unwrap_or_default().to_string();
        let before = self.pending.len();
        match event.kind.as_str() {
            "fork" => {
                let Some(child) = body["child"]["audit_token"]["pid"].as_u64().map(|child| child as u32) else { return };
                let name = self.processes[&pid].name.clone();
                let process = Process { name, parent: Some(pid), live: true, introduced: true, ended: false, started_ns: time, last_ns: time, forks: VecDeque::new(), last_was_fork: false };
                self.processes.insert(child, process);
                self.event(time, child, Class::Process, "start", "", json!({ "parent": pid }), "ok");
            }
            "exec" => {
                let program = path(&body["target"]["executable"]);
                // The target is started through the sandbox's launcher; that step is ours, not the target's.
                if pid == self.root && !self.root_has_run && self.through_launcher && program == SANDBOX {
                    return;
                }
                let argv = if pid == self.root && !std::mem::replace(&mut self.root_has_run, true) { json!(self.request.argv) } else { body["args"].clone() };
                self.event(time, pid, Class::Process, "exec", &program, json!({ "argv": argv }), "ok");
                self.processes.get_mut(&pid).expect("known").name = file_name(&program).to_string();
            }
            "exit" => {
                let status = body["stat"].as_i64().map(|status| status as libc::c_int);
                self.exited(pid, status, time);
            }
            // Every program opens this as it loads; it says nothing about the program.
            "open" | "close" if body["file"]["path"] == "/dev/dtracehelper" || body["target"]["path"] == "/dev/dtracehelper" => {}
            // Opened to read. What is opened to write is told when it is closed, and only if it was written.
            "open" if body["fflag"].as_u64().is_some_and(|flags| flags & 2 == 0) => self.event(time, pid, Class::File, "read", &path(&body["file"]), json!({}), "ok"),
            "close" if body["modified"] == true => {
                let file = path(&body["target"]);
                self.event(time, pid, Class::File, "write", &file, json!({}), "ok");
                self.changed(time, pid, "write", &file);
            }
            "open" | "close" => {}
            "create" => {
                let file = named(&body["destination"]);
                let folder = is_folder(&body["destination"]["existing_file"]) || Path::new(&file).is_dir();
                let op = if folder { "create_dir" } else { "write" };
                self.event(time, pid, Class::File, op, &file, json!({ "created": true }), "ok");
                self.changed(time, pid, op, &file);
            }
            "rename" => {
                let (from, to) = (path(&body["source"]), named(&body["destination"]));
                self.event(time, pid, Class::File, "rename", &from, json!({ "to": to }), "ok");
                self.changed(time, pid, "rename", &from);
                self.changed(time, pid, "rename", &to);
            }
            "unlink" | "truncate" => {
                let file = path(&body["target"]);
                let op = match event.kind.as_str() {
                    "truncate" => "truncate",
                    _ if is_folder(&body["target"]) => "delete_dir",
                    _ => "delete",
                };
                self.event(time, pid, Class::File, op, &file, json!({}), "ok");
                self.changed(time, pid, op, &file);
            }
            "link" => {
                let made = format!("{}/{}", path(&body["target_dir"]).trim_end_matches('/'), body["target_filename"].as_str().unwrap_or_default());
                self.event(time, pid, Class::File, "link", &made, json!({ "from": path(&body["source"]) }), "ok");
                self.changed(time, pid, "link", &made);
            }
            "setmode" => {
                let file = path(&body["target"]);
                self.event(time, pid, Class::File, "set_mode", &file, json!({ "mode": format!("{:o}", body["mode"].as_u64().unwrap_or(0) & 0o7777) }), "ok");
                self.changed(time, pid, "set_mode", &file);
            }
            "uipc_connect" => self.event(time, pid, Class::Network, "connect", &path(&body["file"]), json!({}), "ok"),
            "uipc_bind" => {
                let socket = format!("{}/{}", path(&body["dir"]).trim_end_matches('/'), body["filename"].as_str().unwrap_or_default());
                self.event(time, pid, Class::Network, "bind", &socket, json!({}), "ok");
            }
            _ => {}
        }
        for written in &mut self.pending[before..] {
            if written.pid == pid {
                written.tid = event.tid;
            }
        }
    }

    /// Writes the crashes whose report has come, and those that have waited long enough without one. With `all`,
    /// every one that is left: the session is ending.
    fn tell_crashes(&mut self, all: bool) {
        if let Some(caught) = &self.caught {
            self.stacks.extend(caught.try_iter());
        }
        for crash in std::mem::take(&mut self.crashes) {
            // Caught as it happened: the stack is here already, and nothing is waited for. The system's report is
            // named too when it happens to be there.
            if let Some(stack) = self.stacks.remove(&crash.pid) {
                let report = crash_report(&crash.name, crash.pid, crash.time_ns).map(|(report, _)| report.to_string_lossy().into_owned());
                self.problem(crash.time_ns, crash.pid, "crash", &crash.name, json!({ "signal": crash.signal, "at": stack.first(), "stack": stack, "report": report }));
                continue;
            }
            match crash_report(&crash.name, crash.pid, crash.time_ns) {
                Some((report, stack)) => {
                    let args = json!({ "signal": crash.signal, "at": stack.first(), "stack": stack, "report": report.to_string_lossy() });
                    self.problem(crash.time_ns, crash.pid, "crash", &crash.name, args);
                }
                None if all || Instant::now() >= crash.until => {
                    // Seen on macOS 27.2: a program that crashes again and again gets a report for the first crash
                    // and then none for a good while, whatever is removed from the reports folder; the same program
                    // under another name gets one at once.
                    let why = format!("macOS wrote no crash report for {}: it leaves them out for a program that has crashed repeatedly. A copy of the program under another name gets one", crash.name);
                    self.problem(crash.time_ns, crash.pid, "crash", &crash.name, json!({ "signal": crash.signal, "no_stack_because": why }));
                }
                None => self.crashes.push(crash),
            }
        }
    }

    /// A process ended. `status` is what the kernel said of how, when it said.
    fn exited(&mut self, pid: u32, status: Option<libc::c_int>, time_ns: u64) -> Value {
        let Some(process) = self.processes.get_mut(&pid) else { return Value::Null };
        process.live = false;
        if std::mem::replace(&mut process.ended, true) {
            return Value::Null;
        }
        let name = process.name.clone();
        let how = match status {
            Some(status) if libc::WIFEXITED(status) => json!({ "exit_code": libc::WEXITSTATUS(status) }),
            Some(status) => {
                let signal = libc::WTERMSIG(status);
                if is_crash(signal) {
                    // Told once the system's own report of the crash is there, which has the way to it; or without,
                    // if none comes in a few seconds.
                    // The report comes within a couple of seconds, when it comes at all.
                    self.crashes.push(Crash { pid, name: name.clone(), signal: signal_name(signal), time_ns, until: Instant::now() + Duration::from_secs(4) });
                }
                json!({ "signal": signal_name(signal) })
            }
            None => json!({}),
        };
        self.event(time_ns, pid, Class::Process, "exit", "", how.clone(), if status.is_some() { "ok" } else { "unknown" });
        self.arguments.remove(&pid);
        if pid == self.root && status.is_some() {
            self.outcome = how.clone();
        }
        how
    }

    /// Takes what the kernel has to tell about the processes, waiting a moment when there is nothing.
    fn kernel_events(&mut self, wait: Duration) -> Option<Value> {
        let mut outcome = None;
        // SAFETY: the events are plain data the call fills, at most as many as the array holds.
        let mut events: [libc::kevent; 64] = unsafe { std::mem::zeroed() };
        let timeout = libc::timespec { tv_sec: wait.as_secs() as libc::time_t, tv_nsec: wait.subsec_nanos() as libc::c_long };
        let count = unsafe { libc::kevent(self.queue, std::ptr::null(), 0, events.as_mut_ptr(), events.len() as libc::c_int, &timeout) };
        for event in events.iter().take(count.max(0) as usize) {
            let pid = event.ident as u32;
            if event.fflags & libc::NOTE_FORK != 0 {
                self.scan(pid);
            }
            if event.fflags & libc::NOTE_EXEC != 0 {
                if let Some(found) = arguments(pid) {
                    self.arguments.insert(pid, found);
                }
            }
            if event.fflags & libc::NOTE_EXIT != 0 {
                // What it started in its last moment would otherwise be found only through the reports.
                self.scan(pid);
                let mut status = event.data as libc::c_int;
                if pid == self.root {
                    // Our own child: taking its status also lets the system forget it.
                    // SAFETY: waits for our own child, which has ended; status is a local.
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
                }
                let how = self.exited(pid, Some(status), now_ns());
                if pid == self.root {
                    outcome = Some(how);
                }
            }
        }
        outcome
    }

    /// Writes what is still owed once nothing more can come: starts of processes the reports never named, and the
    /// ends of those only the reports knew.
    fn finish(&mut self) {
        let mut pids: Vec<u32> = self.processes.keys().copied().collect();
        pids.sort_unstable();
        for pid in pids {
            self.introduce(pid, u64::MAX);
            let last = self.processes[&pid].last_ns;
            self.exited(pid, None, last + 1);
        }
    }
}

// ------------------------------------------------------------------------------------------------ the log

struct Log {
    child: Child,
    reports: Receiver<Report>,
}

impl Log {
    fn start() -> io::Result<Log> {
        let mut child = Command::new("/usr/bin/log")
            .args(["stream", "--style", "ndjson", "--predicate", r#"sender == "Sandbox" AND eventMessage CONTAINS " allow ""#])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let output = child.stdout.take().expect("piped");
        let (sender, reports) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Some(report) = parse_line(&line) {
                    if sender.send(report).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Log { child, reports })
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The system's Endpoint Security events, read from `eslogger` running as root.
struct Events {
    child: Child,
    events: Receiver<Kernel>,
}

impl Events {
    fn start() -> io::Result<Events> {
        use std::os::unix::process::CommandExt;
        // A process group of its own: eslogger leaves out what its own group does, and the target must not be in it.
        let mut child = Command::new("sudo").args(["-n", "/usr/bin/eslogger"]).args(EVENT_TYPES).stdin(Stdio::null()).stdout(Stdio::piped()).process_group(0).spawn()?;
        let output = child.stdout.take().expect("piped");
        let (sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Some(event) = parse_event(&line) {
                    if sender.send(event).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Events { child, events })
    }

    /// Starts a program that does nothing and waits for the event of its start: everything before it has arrived
    /// by then. Events that come first are handed to `each`. False when the event didn't come in time.
    fn settle(&self, wait: Duration, mut each: impl FnMut(Kernel)) -> io::Result<bool> {
        let mut child = Command::new("/usr/bin/true").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        let pid = child.id();
        child.wait()?;
        let until = Instant::now() + wait;
        loop {
            match self.events.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(event) if event.kind == "exec" && event.pid == pid => return Ok(true),
                Ok(event) => each(event),
                Err(RecvTimeoutError::Timeout) => return Ok(false),
                Err(RecvTimeoutError::Disconnected) => return Err(io::Error::other("the system's event reader ended")),
            }
        }
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        // It runs as root, so only root can end it: first asked, then made to.
        let group = format!("-{}", self.child.id());
        for signal in ["-TERM", "-KILL"] {
            let quiet = |command: &mut Command| command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
            let _ = quiet(Command::new("sudo").args(["-n", "/bin/kill", signal, "--", &group]));
            let _ = quiet(Command::new("sudo").args(["-n", "/bin/kill", signal, &self.child.id().to_string()]));
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until {
                if !matches!(self.child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Whether this user may become root without being asked: what the system's events need.
fn has_root() -> bool {
    std::env::var_os("MC_OBSERVE_WITHOUT_ROOT").is_none()
        && Command::new("sudo").args(["-n", "true"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|status| status.success())
}

/// Runs a program that does nothing under the same profile, and returns its pid: when its report arrives, the log has
/// delivered everything that happened before it.
fn marker() -> io::Result<u32> {
    let mut child = Command::new(SANDBOX).args(["-p", PROFILE, "/usr/bin/true"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let pid = child.id();
    if !child.wait()?.success() {
        return Err(io::Error::other("a program can't be started under a reporting sandbox here (is Master Control itself running inside a sandbox?)"));
    }
    Ok(pid)
}

/// Starts reading the sandbox's reports and waits until they come through: the log takes a moment to start
/// delivering, and nothing may be started before it does.
fn start_log(ignored: &mut HashSet<u32>) -> Result<Log, String> {
    let log = Log::start().map_err(|error| format!("the system log couldn't be read: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let pid = marker().map_err(|error| error.to_string())?;
        ignored.insert(pid);
        let until = Instant::now() + Duration::from_millis(300);
        loop {
            match log.reports.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(report) if report.pid == pid => return Ok(log),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return Err("the system log stopped; reading it needs an administrator account".into()),
            }
        }
        if Instant::now() > deadline {
            return Err("the system log delivered none of the sandbox's reports in time".into());
        }
    }
}

fn write_state(directory: &Path, state: &Value) {
    let _ = crate::jobs::write_whole(&directory.join("state.json"), state.to_string().as_bytes());
}

/// Runs the session whose request is in `directory`. Returns when the target and everything it started have ended.
pub fn run(directory: &Path) -> io::Result<()> {
    let mut request: Request = serde_json::from_slice(&fs::read(directory.join("request.json"))?).map_err(io::Error::other)?;
    let store = Store::open(&directory.join("events.sqlite")).map_err(io::Error::other)?;
    let failed = |error: String| {
        write_state(directory, &json!({ "state": "failed", "error": error }));
        Ok(())
    };
    if request.syscalls == "full" {
        return failed("every system call can't be recorded on macOS; leave syscalls off".into());
    }
    let attach = request.attach;
    let program = match attach {
        // A program that is already running: nothing to find, nothing to start.
        Some(pid) => {
            if look(pid).is_none() {
                return failed(format!("there is no running program with the process id {pid}"));
            }
            PathBuf::new()
        }
        None => match find_program(&request) {
            Some(program) => program,
            None => return failed(format!("the command couldn't be started: there is no program {}", request.argv[0])),
        },
    };
    for path in request.allow.iter_mut().chain(request.guard.iter_mut()) {
        *path = real_path(path);
    }
    // SAFETY: the handler only sets an atomic flag; the structure is zeroed and then filled as sigaction expects.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_stop as extern "C" fn(libc::c_int) as usize;
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }

    // From here on, a fatal crash of anything this process starts is handed to it first, with the stack.
    #[cfg(target_arch = "aarch64")]
    let caught = if attach.is_none() { super::mac_crash::listen() } else { None };
    #[cfg(not(target_arch = "aarch64"))]
    let caught: Option<Receiver<(u32, Vec<String>)>> = None;
    // With admin rights the system's own events are the source; without, or when they don't come, the sandbox's
    // reports are.
    let events = if has_root() {
        let ready = Events::start().and_then(|events| {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !events.settle(Duration::from_millis(300), |_| {})? {
                if Instant::now() > deadline {
                    return Err(io::Error::other("none of the system's events arrived in time"));
                }
            }
            Ok(events)
        });
        match ready {
            Ok(events) => Some(events),
            Err(error) => {
                eprintln!("observe-run: the system's events can't be read ({error}); using the sandbox's reports");
                None
            }
        }
    } else {
        None
    };
    if events.is_none() && attach.is_some() {
        return failed("following a program that is already running needs admin rights on macOS (sudo without a password)".into());
    }
    if events.is_none() && request.self_sandboxed {
        return failed("a program with a sandbox of its own can only be watched with admin rights on macOS (sudo without a password)".into());
    }
    let mut ignored = HashSet::new();
    // A sandbox can only be put around a program as it starts: one that is already running is watched from
    // outside alone.
    let log = if request.self_sandboxed || attach.is_some() {
        None
    } else {
        match start_log(&mut ignored) {
            Ok(log) => Some(log),
            Err(error) => return failed(error),
        }
    };

    let mut command = match &log {
        Some(_) => {
            let mut command = Command::new(SANDBOX);
            command.args(["-p", PROFILE]).arg(&program);
            command
        }
        None => Command::new(&program),
    };
    command
        .args(request.argv.get(1..).unwrap_or_default())
        .envs(request.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(File::create(directory.join("stdout.log"))?)
        .stderr(File::create(directory.join("stderr.log"))?);
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    let started = now_ns();
    let child = match attach {
        Some(pid) => pid,
        None => match command.spawn() {
            Ok(child) => child.id(),
            Err(error) => return failed(format!("the command couldn't be started: {error}")),
        },
    };
    // SAFETY: a plain call; the descriptor is ours until the process ends.
    let queue = unsafe { libc::kqueue() };
    let mut recorder = Recorder {
        store,
        pending: Vec::new(),
        request,
        processes: HashMap::new(),
        ignored,
        arguments: HashMap::new(),
        queue,
        reported: HashSet::new(),
        written: 0,
        root: child,
        root_has_run: attach.is_some(),
        by_events: events.is_some(),
        clock: 0,
        last_number: None,
        lost: 0,
        outcome: Value::Null,
        crashes: Vec::new(),
        caught,
        stacks: HashMap::new(),
        through_launcher: log.is_some(),
    };
    let root = Process {
        name: file_name(&program.to_string_lossy()).to_string(),
        parent: Some(std::process::id()),
        live: true,
        introduced: true,
        ended: false,
        started_ns: started,
        last_ns: started,
        forks: VecDeque::new(),
        last_was_fork: false,
    };
    recorder.processes.insert(child, root);
    if attach.is_some() {
        // The program and what it has already started, as they are now; what they start from here on comes from
        // the system's events.
        let mut found = vec![(child, None)];
        let mut at = 0;
        while at < found.len() {
            let parent = found[at].0;
            found.extend(children(parent).into_iter().map(|pid| (pid, Some(parent))));
            at += 1;
        }
        for (pid, parent) in found {
            let seen = look(pid);
            let (path, argv) = arguments(pid).unwrap_or_default();
            let name = seen.as_ref().map(|seen| seen.name.clone()).filter(|name| !name.is_empty()).unwrap_or_else(|| file_name(&path).to_string());
            let process = Process { name, parent, live: true, introduced: true, ended: false, started_ns: started, last_ns: started, forks: VecDeque::new(), last_was_fork: false };
            recorder.processes.insert(pid, process);
            recorder.event(started, pid, Class::Process, "attach", &path, json!({ "argv": argv, "parent": parent.or(seen.map(|seen| seen.parent)) }), "ok");
        }
    } else {
        recorder.event(started, child, Class::Process, "start", "", json!({ "parent": std::process::id() }), "ok");
    }
    recorder.flush();
    let reap = || {
        let mut status = 0;
        // SAFETY: asks about our own child without waiting; status is a local.
        (unsafe { libc::waitpid(child as libc::pid_t, &mut status, libc::WNOHANG) } == child as libc::pid_t).then_some(status)
    };
    if events.is_none() && !recorder.watch(child) {
        // Gone already: only a program that can't run at all ends this fast.
        std::thread::sleep(Duration::from_millis(50));
        recorder.exited(child, reap(), now_ns());
    }
    write_state(directory, &json!({ "state": "running", "pid": child, "tracer": std::process::id() }));

    let mut stopped = false;
    let mut events_gone = false;
    let mut held: VecDeque<Report> = VecDeque::new();
    loop {
        if STOP.swap(false, Ordering::SeqCst) {
            stopped = true;
        }
        if stopped && attach.is_some() {
            // A program that was running before the session goes on running after it.
            break;
        }
        if stopped {
            for pid in recorder.live() {
                // SAFETY: a plain call. A pid that is already gone is refused harmlessly.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
        }
        match &events {
            Some(events) if !events_gone => loop {
                match if recorder.pending.is_empty() { events.events.recv_timeout(Duration::from_millis(20)) } else { events.events.try_recv().map_err(|_| RecvTimeoutError::Timeout) } {
                    Ok(event) => recorder.kernel(event),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        eprintln!("observe-run: the system's event reader ended; only the target's own end is known from here on");
                        events_gone = true;
                        break;
                    }
                }
                if recorder.pending.len() >= 512 {
                    break;
                }
            },
            Some(_) => {
                // Nothing tells of the processes any more: the target's own end is all there is to wait for.
                std::thread::sleep(Duration::from_millis(20));
                if let Some(status) = reap() {
                    recorder.exited(child, Some(status), now_ns());
                    for pid in recorder.live() {
                        recorder.exited(pid, None, now_ns());
                    }
                }
            }
            None => {
                recorder.kernel_events(Duration::from_millis(20));
            }
        }
        if let Some(log) = &log {
            while let Ok(report) = log.reports.try_recv() {
                held.push_back(report);
            }
            // With the system's events, a report waits until those have passed its moment, so the process it names
            // is known; never long, though, in case nothing else is happening on the machine.
            let now = now_ns();
            while held.front().is_some_and(|report| !recorder.by_events || report.time_ns <= recorder.clock || now.saturating_sub(report.time_ns) > 500_000_000) {
                recorder.report(held.pop_front().expect("checked"));
            }
        }
        recorder.tell_crashes(false);
        recorder.flush();
        if !recorder.live().is_empty() {
            continue;
        }
        // Everything known has ended. The sources run behind: what they still hold may name a process nobody saw.
        if let Some(events) = events.as_ref().filter(|_| !events_gone) {
            let mut late = Vec::new();
            let _ = events.settle(Duration::from_secs(5), |event| late.push(event));
            for event in late {
                recorder.kernel(event);
            }
        }
        if let Some(log) = &log {
            if let Ok(pid) = marker() {
                recorder.ignored.insert(pid);
                let until = Instant::now() + Duration::from_secs(5);
                while let Ok(report) = log.reports.recv_timeout(until.saturating_duration_since(Instant::now())) {
                    if report.pid == pid {
                        break;
                    }
                    held.push_back(report);
                }
            }
            for report in held.drain(..) {
                recorder.report(report);
            }
        }
        if recorder.live().is_empty() {
            break;
        }
    }
    // The system writes its report on a crash a moment after it; the session waits for it, up to a few seconds.
    while !recorder.crashes.is_empty() && recorder.crashes.iter().any(|crash| Instant::now() < crash.until) {
        recorder.tell_crashes(false);
        std::thread::sleep(Duration::from_millis(100));
    }
    recorder.tell_crashes(true);
    // Our own child: taking its status lets the system forget it. It is also the last word on how the target
    // ended, should the event that told of its end have been lost on the way.
    let reaped = reap();
    if !(stopped && attach.is_some()) {
        recorder.finish();
    }
    if let (true, Some(status)) = (recorder.outcome.is_null(), reaped) {
        recorder.outcome = if libc::WIFEXITED(status) { json!({ "exit_code": libc::WEXITSTATUS(status) }) } else { json!({ "signal": signal_name(libc::WTERMSIG(status)) }) };
    }
    recorder.flush();
    let source = if recorder.by_events { "system events" } else { "sandbox reports" };
    let mut state = json!({ "state": if stopped { "stopped" } else { "exited" }, "pid": child, "events": recorder.written, "source": source });
    if recorder.by_events {
        state["events_lost"] = json!(recorder.lost);
    }
    if attach.is_some() {
        state["attached"] = json!(true);
    }
    drop(events);
    drop(log);
    if let Some(how) = recorder.outcome.as_object() {
        for (name, value) in how {
            state[name] = value.clone();
        }
    }
    write_state(directory, &state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_line_is_read() {
        let line = r#"{"timestamp":"2026-11-11 15:13:15.373124+0100","eventMessage":"Sandbox: cat(9451) allow file-read-data /private/tmp/a file.txt","processID":0}"#;
        let report = parse_line(line).unwrap();
        assert_eq!((report.name.as_str(), report.pid, report.op.as_str(), report.what.as_str()), ("cat", 9451, "file-read-data", "/private/tmp/a file.txt"));
        // 2026-11-11 14:13:15 UTC.
        assert_eq!(report.time_ns, 1_794_406_395_373_124_000);
        assert_eq!(parse_message("Sandbox: My App (beta)(12) allow process-fork"), Some(("My App (beta)".into(), 12, "process-fork".into(), String::new())));
        assert_eq!(parse_message("Sandbox: nc(7) allow network-outbound remote:*:9").unwrap().3, "remote:*:9");
        assert_eq!(parse_message("1 duplicate report for Sandbox: sh(9450) allow file-read-data /bin/sh"), None);
        assert_eq!(parse_message("Sandbox: WindowManager(1159) deny(1) syscall-unix 494"), None);
    }

    #[test]
    fn times_are_read_in_any_zone() {
        assert_eq!(parse_time("1970-01-01 00:00:00.000000+0000"), Some(0));
        assert_eq!(parse_time("1970-01-01 01:00:01.5+0100"), Some(1_500_000_000));
        assert_eq!(parse_time("2024-02-29 23:59:59.000001-0800"), Some(1_709_279_999_000_001_000));
        assert_eq!(parse_time("not a time"), None);
    }

    #[test]
    fn names_cut_short_still_match() {
        assert!(same_name("mc-test-target", "mc-test-target"));
        assert!(same_name("com.apple.WebKit.Networking", "com.apple.WebKi"));
        assert!(!same_name("cat", "ca"));
    }
}

//! The master's side (specs.md §2): the workers it knows, how it finds and pairs with them, and how it calls their
//! tools. Everything said to a worker is sealed with the key the pairing gave the two of them.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mc_agent::identity::mac_of;
use mc_core::link::{self, SharedKey};
use serde_json::{json, Value};

pub mod mcp;
pub mod cli;
pub mod pairing;
pub mod agents;
pub mod role;

pub const WORKER_PORT: u16 = 47901;
pub const ANNOUNCE_PORT: u16 = 47902;
/// The port the master listens on while it shows its pairing code.
pub const PAIR_PORT: u16 = 47903;

#[derive(Debug)]
pub struct Failure {
    /// The worker's own code for a refused tool call, or: `unreachable`, `not_paired`, `unknown_machine`, `failed`.
    pub code: String,
    pub message: String,
}

impl Failure {
    fn new(code: &str, message: impl Into<String>) -> Failure {
        Failure { code: code.to_string(), message: message.into() }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// A worker this master is paired with.
#[derive(Clone, Debug)]
pub struct Machine {
    pub name: String,
    pub id: String,
    pub address: String,
    key: SharedKey,
}

/// A worker heard announcing itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Announced {
    pub id: String,
    pub name: String,
    pub address: String,
    pub paired: bool,
    pub version: String,
}

struct Session {
    id: String,
    number: u64,
}

pub struct Master {
    home: PathBuf,
    name: String,
    sessions: Mutex<HashMap<String, Session>>,
    /// The master's own machine: the same tools, run here.
    own: std::sync::Arc<mc_agent::Agent>,
}

/// What the master's own machine is called in `machine`, unless MC_MASTER_NAME gives it another name.
pub const OWN_NAME: &str = "master";

/// `MC_MASTER_HOME` if set; otherwise a folder in the user's own.
pub fn default_home() -> PathBuf {
    if let Some(home) = std::env::var_os("MC_MASTER_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home);
    }
    let user = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    user.join(".master-control").join("master")
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    io::Write::write_all(&mut options.open(path)?, bytes)
}

fn post(address: &str, path: &str, body: &[u8]) -> Result<(u16, Vec<u8>), Failure> {
    let response = match ureq::post(&format!("http://{address}{path}")).timeout(Duration::from_secs(3700)).send_bytes(body) {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => return Err(Failure::new("unreachable", format!("the worker at {address} didn't answer: {error}"))),
    };
    let status = response.status();
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes).map_err(|error| Failure::new("unreachable", format!("the worker's answer was cut short: {error}")))?;
    Ok((status, bytes))
}

fn load_machines(home: &Path) -> Vec<Machine> {
    let saved: Value = fs::read(home.join("workers.json")).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
    let mut machines: Vec<Machine> = saved["workers"]
        .as_array()
        .map(|workers| {
            workers
                .iter()
                .filter_map(|worker| {
                    Some(Machine {
                        name: worker["name"].as_str()?.to_string(),
                        id: worker["id"].as_str()?.to_string(),
                        address: worker["address"].as_str()?.to_string(),
                        key: link::from_hex(worker["key"].as_str()?)?.try_into().ok()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    machines.sort_by(|a, b| a.name.cmp(&b.name));
    machines
}

fn save_machines(home: &Path, machines: &[Machine]) -> io::Result<()> {
    let workers: Vec<Value> =
        machines.iter().map(|machine| json!({ "name": machine.name, "id": machine.id, "address": machine.address, "key": link::to_hex(&machine.key) })).collect();
    write_private(&home.join("workers.json"), json!({ "workers": workers }).to_string().as_bytes())
}

/// Adds a newly paired worker to the ones remembered, in place of any earlier pairing with the same machine.
fn add_machine(home: &Path, mut machine: Machine) -> io::Result<Machine> {
    let mut machines = load_machines(home);
    machines.retain(|known| known.id != machine.id);
    // Two workers with one name would be one too many to tell apart.
    let taken = |name: &str| machines.iter().any(|known| known.name.eq_ignore_ascii_case(name));
    if taken(&machine.name) {
        machine.name = (2..).map(|number| format!("{}-{number}", machine.name)).find(|name| !taken(name)).expect("a free name");
    }
    machines.push(machine.clone());
    save_machines(home, &machines)?;
    Ok(machine)
}

impl Master {
    pub fn open(home: PathBuf) -> io::Result<Master> {
        fs::create_dir_all(&home)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
        }
        let name = std::env::var("MC_MASTER_NAME").ok().filter(|name| !name.is_empty()).unwrap_or_else(|| OWN_NAME.to_string());
        let own = mc_agent::Agent::new(home.join("own"))?;
        Ok(Master { home, name, sessions: Mutex::new(HashMap::new()), own })
    }

    /// The workers this master is paired with, by name.
    pub fn machines(&self) -> Vec<Machine> {
        load_machines(&self.home)
    }

    fn save(&self, machines: &[Machine]) -> io::Result<()> {
        save_machines(&self.home, machines)
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn machine(&self, name: &str) -> Result<Machine, Failure> {
        let machines = self.machines();
        machines.iter().find(|machine| machine.name.eq_ignore_ascii_case(name) || machine.id == name).cloned().ok_or_else(|| {
            let known: Vec<&str> = machines.iter().map(|machine| machine.name.as_str()).collect();
            Failure::new("unknown_machine", format!("there is no worker named {name}; paired workers: {}", if known.is_empty() { "none".to_string() } else { known.join(", ") }))
        })
    }

    pub fn unpair(&self, name: &str) -> Result<(), Failure> {
        let machine = self.machine(name)?;
        let mut machines = self.machines();
        machines.retain(|known| known.id != machine.id);
        self.save(&machines).map_err(|error| Failure::new("failed", error.to_string()))
    }

    /// Sends one sealed message and opens the sealed answer.
    fn exchange(&self, machine: &Machine, message: &Value) -> Result<Value, Failure> {
        let sealed = link::seal(&machine.key, b"mc1 request", message.to_string().as_bytes());
        let (status, bytes) = post(&machine.address, "/sealed", &sealed)?;
        if status != 200 {
            let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let error = &answer["error"];
            return Err(Failure::new(error["code"].as_str().unwrap_or("failed"), error["message"].as_str().unwrap_or("the worker refused the message")));
        }
        let context = [b"mc1 answer ".as_slice(), &sealed[..link::NONCE]].concat();
        let plain = link::open(&machine.key, &context, &bytes).ok_or_else(|| Failure::new("failed", "the answer wasn't sealed by this worker for this request"))?;
        let answer: Value = serde_json::from_slice(&plain).map_err(|error| Failure::new("failed", format!("the worker's answer isn't JSON: {error}")))?;
        match answer.get("ok") {
            Some(result) => Ok(result.clone()),
            None => Err(Failure::new(answer["error"]["code"].as_str().unwrap_or("failed"), answer["error"]["message"].as_str().unwrap_or("the worker reported an error"))),
        }
    }

    /// Starts a conversation with a worker; returns what the worker says of itself.
    fn hello(&self, machine: &Machine) -> Result<Value, Failure> {
        let greeting = self.exchange(machine, &json!({ "hello": true }))?;
        let session = greeting["session"].as_str().ok_or_else(|| Failure::new("failed", "the worker gave no session"))?.to_string();
        // The worker says who it is each time: its id changes when its system is updated or it is renamed, and the
        // master's record follows.
        if let Some(id) = greeting["id"].as_str().filter(|id| *id != machine.id) {
            let mut machines = self.machines();
            if let Some(known) = machines.iter_mut().find(|known| known.id == machine.id) {
                known.id = id.to_string();
                let _ = self.save(&machines);
            }
        }
        self.sessions.lock().unwrap().insert(machine.address.clone(), Session { id: session, number: 0 });
        Ok(greeting)
    }

    /// Runs one of a worker's tools.
    pub fn call(&self, machine: &Machine, tool: &str, args: &Value) -> Result<Value, Failure> {
        for attempt in 0..2 {
            if !self.sessions.lock().unwrap().contains_key(&machine.address) {
                self.hello(machine)?;
            }
            let (session, number) = {
                let mut sessions = self.sessions.lock().unwrap();
                let session = sessions.get_mut(&machine.address).expect("just made");
                session.number += 1;
                (session.id.clone(), session.number)
            };
            match self.exchange(machine, &json!({ "session": session, "number": number, "tool": tool, "args": args })) {
                // The worker restarted since the conversation began: begin another, once.
                Err(failure) if failure.code == "stale_session" && attempt == 0 => {
                    self.sessions.lock().unwrap().remove(&machine.address);
                }
                other => return other,
            }
        }
        unreachable!("the second attempt returns")
    }

    /// The name the master's own machine goes by.
    pub fn own_name(&self) -> &str {
        &self.name
    }

    fn is_own(&self, machine: &str) -> bool {
        machine.eq_ignore_ascii_case(&self.name) || ["master", "self", "local"].iter().any(|alias| machine.eq_ignore_ascii_case(alias))
    }

    /// Runs a tool on the named machine: the master's own, or a worker. Each call is noted in the master's activity
    /// file (which machine, which tool, how it ended, how long it took — never what was sent), which is what the
    /// app's status shows as it happens.
    pub fn run(&self, machine: &str, tool: &str, args: &Value) -> Result<Value, Failure> {
        let began = std::time::Instant::now();
        let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
        // Noted as it starts too, so a call that takes a while shows as going on while it does.
        note_activity(&self.home, &serde_json::json!({ "time_ms": now(), "machine": machine, "tool": tool, "started": true }).to_string());
        let outcome = self.run_quietly(machine, tool, args);
        let line = serde_json::json!({
            "time_ms": now(),
            "machine": machine,
            "tool": tool,
            "ok": outcome.is_ok(),
            "code": outcome.as_ref().err().map(|failure| failure.code.clone()),
            "took_ms": began.elapsed().as_millis() as u64,
        });
        note_activity(&self.home, &line.to_string());
        outcome
    }

    fn run_quietly(&self, machine: &str, tool: &str, args: &Value) -> Result<Value, Failure> {
        // A worker that was given one of these names is still reachable: workers are looked up first.
        match self.machine(machine) {
            Ok(worker) => self.call(&worker, tool, args),
            Err(_) if self.is_own(machine) => self.own.call("master", tool, args).map_err(|error| Failure::new(error.code, error.message)),
            Err(unknown) => Err(Failure::new("unknown_machine", format!("{}, and the master's own machine is {}", unknown.message.trim_start_matches("unknown_machine: "), self.name))),
        }
    }

    /// The tools a worker offers, as MCP describes tools.
    pub fn tools(&self, machine: &Machine) -> Result<Vec<Value>, Failure> {
        Ok(self.call(machine, "list_tools", &Value::Null)?["tools"].as_array().cloned().unwrap_or_default())
    }

    /// Notes where each paired worker was last heard, so a worker whose address changed is still found.
    pub fn remember_addresses(&self, heard: &[Announced]) -> io::Result<()> {
        let mut machines = self.machines();
        let mut changed = false;
        for machine in &mut machines {
            // The same machine by its whole id, or by its hardware address alone if its name or system has changed since.
            let same = |announced: &&Announced| announced.id == machine.id || (!mac_of(&machine.id).is_empty() && machine.id.contains('|') && mac_of(&announced.id) == mac_of(&machine.id));
            if let Some(now) = heard.iter().find(same).filter(|announced| announced.address != machine.address) {
                machine.address = now.address.clone();
                changed = true;
            }
        }
        if changed {
            self.save(&machines)?;
        }
        Ok(())
    }
}

/// One call the master made, as its activity file keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    pub time_ms: u64,
    pub machine: String,
    pub tool: String,
    pub ok: bool,
    pub code: String,
    pub took_ms: u64,
}

fn activity_file(home: &std::path::Path) -> PathBuf {
    home.join("activity.log")
}

/// Adds a line to the activity file, which is kept short: once it passes half a megabyte, the older half goes.
fn note_activity(home: &std::path::Path, line: &str) {
    use std::io::Write;
    let path = activity_file(home);
    if std::fs::metadata(&path).is_ok_and(|file| file.len() > 512 * 1024) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let keep: Vec<&str> = text.lines().collect();
            let _ = std::fs::write(&path, keep[keep.len() / 2..].join("\n") + "\n");
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

/// The end of the activity file: enough for the latest calls, a line being a couple of hundred bytes.
fn activity_tail(home: &std::path::Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(activity_file(home)) else { return String::new() };
    let length = file.metadata().map_or(0, |file| file.len());
    let _ = file.seek(SeekFrom::Start(length.saturating_sub(16 * 1024)));
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The machines the master is talking to right now: a call to them is under way, or ended within the last
/// `within_ms`. This is what makes the app's status lights flicker.
pub fn active_machines(home: &std::path::Path, within_ms: u64) -> Vec<String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    let mut under_way: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    let mut active = std::collections::BTreeSet::new();
    for line in activity_tail(home).lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let (Some(time), Some(machine)) = (entry["time_ms"].as_u64(), entry["machine"].as_str()) else { continue };
        // Only what is recent counts as under way: a master that was killed mid-call never wrote the call's end.
        if now.saturating_sub(time) > 120_000 {
            continue;
        }
        *under_way.entry(machine.to_string()).or_default() += if entry["started"] == true { 1 } else { -1 };
        if now.saturating_sub(time) <= within_ms {
            active.insert(machine.to_string());
        }
    }
    active.extend(under_way.into_iter().filter(|(_, calls)| *calls > 0).map(|(machine, _)| machine));
    active.into_iter().collect()
}

/// The master's latest calls, newest last, read from its activity file without opening the master itself: the
/// app looks at this every second.
pub fn recent_activity(home: &std::path::Path, count: usize) -> Vec<Activity> {
    let text = activity_tail(home);
    let read = |line: &str| -> Option<Activity> {
        let entry: Value = serde_json::from_str(line).ok()?;
        Some(Activity {
            time_ms: entry["time_ms"].as_u64()?,
            machine: entry["machine"].as_str()?.to_string(),
            tool: entry["tool"].as_str()?.to_string(),
            ok: entry["ok"].as_bool()?,
            code: entry["code"].as_str().unwrap_or_default().to_string(),
            took_ms: entry["took_ms"].as_u64().unwrap_or(0),
        })
    };
    let mut found: Vec<Activity> = text.lines().rev().filter_map(read).take(count).collect();
    found.reverse();
    found
}

/// Listens for workers announcing themselves, for the given time.
pub fn scan(listen_on: SocketAddr, time: Duration) -> io::Result<Vec<Announced>> {
    let socket = UdpSocket::bind(listen_on)?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let deadline = Instant::now() + time;
    let mut heard: Vec<Announced> = Vec::new();
    let mut buffer = [0u8; 2048];
    while Instant::now() < deadline {
        let Ok((length, from)) = socket.recv_from(&mut buffer) else { continue };
        let Ok(message) = serde_json::from_slice::<Value>(&buffer[..length]) else { continue };
        let (Some(1), Some(id), Some(port)) = (message["master_control"].as_u64(), message["id"].as_str(), message["port"].as_u64()) else { continue };
        let announced = Announced {
            id: id.to_string(),
            name: message["name"].as_str().unwrap_or_default().to_string(),
            // Where it came from, not where it says it is: a worker doesn't know which of its addresses we can reach.
            address: format!("{}:{port}", from.ip()),
            paired: message["paired"].as_bool().unwrap_or(false),
            version: message["version"].as_str().unwrap_or_default().to_string(),
        };
        heard.retain(|known| known.id != announced.id);
        heard.push(announced);
    }
    heard.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(heard)
}

//! Worker mode (specs.md §2, §5): the agent makes itself available to one master across the local network.
//!
//! A second listener, apart from the loopback one, takes one thing only: messages sealed with the key a pairing
//! produced. Pairing is asked for from the worker's side, with the code the master shows (`pair_with_master`): the
//! master that proves it had the same code becomes this worker's master, and the worker answers nobody else. The
//! worker also announces itself on the network every few seconds, so its master finds it again if its address changes.

use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use mc_core::link::{self, Pairing, SharedKey};
use serde_json::{json, Value};
use tiny_http::{Method, Request, Response, Server};

use crate::{state, Agent};

/// The port workers listen on for their master, and the port announcements are sent to.
pub const WORKER_PORT: u16 = 47901;
pub const ANNOUNCE_PORT: u16 = 47902;
/// The port a master listens on while it shows its pairing code.
pub const PAIR_PORT: u16 = 47903;

#[derive(Clone)]
pub struct WorkerConfig {
    /// The address to listen on for the master: every address of the machine (0.0.0.0) for real use. Tests listen
    /// on the loopback address only, which also keeps Windows Firewall from asking about each test program.
    pub address: std::net::IpAddr,
    /// 0 picks a free port.
    pub port: u16,
    /// What the master lists this machine as. Default: its host name.
    pub name: Option<String>,
    /// Where to send announcements. Empty: the local network's broadcast address.
    pub announce_to: Vec<SocketAddr>,
}

struct Master {
    name: String,
    key: SharedKey,
}

struct Shared {
    home: PathBuf,
    id: String,
    name: String,
    port: u16,
    master: Option<Master>,
    /// Conversations with the master, each with the number of its last message: a message seen again, or an old
    /// one sent again by someone who recorded it, is refused.
    sessions: HashMap<String, u64>,
}

fn saved(home: &Path) -> Value {
    std::fs::read(home.join("worker.json")).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null)
}

fn master_in(saved: &Value) -> Option<Master> {
    let key = saved["master"]["key"].as_str().and_then(link::from_hex)?.try_into().ok()?;
    Some(Master { name: saved["master"]["name"].as_str().unwrap_or_default().to_string(), key })
}

impl Shared {
    fn load(home: &Path, name: Option<String>, port: u16) -> io::Result<Shared> {
        let saved = saved(home);
        let shared = Shared {
            home: home.to_path_buf(),
            // Who this machine is: its host name, system release and hardware address.
            id: crate::identity::this_machine().id(),
            name: name.or_else(|| saved["name"].as_str().map(str::to_string)).filter(|name| !name.is_empty()).unwrap_or_else(crate::platform::hostname),
            port,
            master: master_in(&saved),
            sessions: HashMap::new(),
        };
        shared.save()?;
        Ok(shared)
    }

    /// Writes who this worker is, where it listens and who its master is, where only this user can read it.
    fn save(&self) -> io::Result<()> {
        let master = self.master.as_ref().map(|master| json!({ "name": master.name, "key": link::to_hex(&master.key) }));
        state::write_private(&self.home.join("worker.json"), json!({ "id": self.id, "name": self.name, "port": self.port, "master": master }).to_string().as_bytes())
    }

    fn announcement(&self) -> Vec<u8> {
        json!({ "master_control": 1, "role": "worker", "identity": crate::identity::this_machine().to_json(), "id": self.id, "name": self.name, "port": self.port, "paired": self.master.is_some(), "version": crate::VERSION }).to_string().into_bytes()
    }
}

fn respond(request: Request, status: u16, body: Vec<u8>) {
    let _ = request.respond(Response::from_data(body).with_status_code(status));
}

fn respond_plain(request: Request, status: u16, code: &str, message: &str) {
    respond(request, status, json!({ "error": { "code": code, "message": message } }).to_string().into_bytes());
}

/// A sealed message from the master, answered sealed.
fn sealed(agent: &Agent, shared: &Mutex<Shared>, request: Request, body: &[u8]) {
    let (key, plain) = {
        let mut state = shared.lock().unwrap();
        if state.master.is_none() {
            // Pairing is done by another run of this program, which writes the master down: look again.
            state.master = master_in(&saved(&state.home));
        }
        let opened = state.master.as_ref().and_then(|master| Some((master.key, link::open(&master.key, b"mc1 request", body)?)));
        match opened {
            Some(found) => found,
            None => {
                drop(state);
                return respond_plain(request, 403, "not_paired", "this message isn't from this worker's master");
            }
        }
    };
    let message: Value = serde_json::from_slice(&plain).unwrap_or(Value::Null);
    let answer = if message["hello"] == true {
        let mut state = shared.lock().unwrap();
        let session = link::to_hex(&link::random::<16>());
        // A master that restarts says hello again; its old conversations are of no more use to anyone.
        state.sessions.clear();
        state.sessions.insert(session.clone(), 0);
        json!({ "ok": { "session": session, "id": state.id, "name": state.name, "identity": crate::identity::this_machine().to_json(), "version": crate::VERSION } })
    } else {
        let fresh = {
            let mut state = shared.lock().unwrap();
            let number = message["number"].as_u64().unwrap_or(0);
            match message["session"].as_str().and_then(|session| state.sessions.get_mut(session)) {
                Some(last) if number > *last => {
                    *last = number;
                    true
                }
                _ => false,
            }
        };
        if !fresh {
            json!({ "error": { "code": "stale_session", "message": "start a new conversation (the worker restarted, or this message was already seen)" } })
        } else if message["tool"] == "forget_master" {
            // The master is cleaning up: this worker is free to be paired again.
            let mut state = shared.lock().unwrap();
            state.master = None;
            state.sessions.clear();
            match state.save() {
                Ok(()) => json!({ "ok": { "forgotten": true } }),
                Err(error) => json!({ "error": { "code": "failed", "message": format!("the worker couldn't save that: {error}") } }),
            }
        } else if message["tool"] == "list_tools" {
            json!({ "ok": { "tools": crate::tool_list() } })
        } else {
            match agent.call("master", message["tool"].as_str().unwrap_or_default(), &message["args"]) {
                Ok(result) => json!({ "ok": result }),
                Err(error) => json!({ "error": { "code": error.code, "message": error.message } }),
            }
        }
    };
    // The answer is tied to the request it answers by that request's nonce, so it can't be passed off as the
    // answer to another.
    let context = [b"mc1 answer ".as_slice(), &body[..link::NONCE]].concat();
    respond(request, 200, link::seal(&key, &context, answer.to_string().as_bytes()));
}

fn handle(agent: &Agent, shared: &Mutex<Shared>, mut request: Request) {
    let mut body = Vec::new();
    if request.as_reader().take(256 * 1024 * 1024).read_to_end(&mut body).is_err() {
        return respond_plain(request, 400, "bad_input", "the request couldn't be read");
    }
    match (request.method(), request.url()) {
        (Method::Post, "/sealed") => sealed(agent, shared, request, &body),
        // Nothing else is served here: the plain API stays on the loopback address.
        _ => respond_plain(request, 404, "not_found", "a worker takes only sealed messages from its master on this port"),
    }
}

pub struct Worker {
    pub port: u16,
    threads: Vec<JoinHandle<()>>,
}

impl Worker {
    pub fn join(self) {
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

/// Starts listening for the master, and announcing.
pub fn start(agent: Arc<Agent>, config: WorkerConfig, stop: Arc<AtomicBool>) -> io::Result<Worker> {
    let server = Server::http((config.address, config.port)).map_err(|error| io::Error::other(format!("can't listen on port {}: {error}", config.port)))?;
    let port = server.server_addr().to_ip().map_or(config.port, |address| address.port());
    let shared = Arc::new(Mutex::new(Shared::load(&agent.home, config.name, port)?));

    let (listening, stopping) = (shared.clone(), stop.clone());
    let listener = std::thread::spawn(move || {
        while !stopping.load(Ordering::SeqCst) {
            if let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(200)) {
                let (agent, shared) = (agent.clone(), listening.clone());
                std::thread::spawn(move || handle(&agent, &shared, request));
            }
        }
        // The server ends by connecting to the address it listens on, to wake its own waiting thread. Listening on
        // every address, that address is 0.0.0.0, which Windows won't connect to: the thread then never wakes and
        // ending never returns. A connection through the loopback address wakes it just the same.
        let waking = std::thread::spawn(move || {
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(50));
                let _ = std::net::TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(200));
            }
        });
        drop(server);
        let _ = waking.join();
    });
    let announcer = std::thread::spawn(move || {
        let Ok(socket) = UdpSocket::bind(("0.0.0.0", 0)) else { return };
        let _ = socket.set_broadcast(true);
        let targets = if config.announce_to.is_empty() { vec![SocketAddr::from(([255, 255, 255, 255], ANNOUNCE_PORT))] } else { config.announce_to };
        let mut waited = Duration::from_secs(2);
        while !stop.load(Ordering::SeqCst) {
            if waited >= Duration::from_secs(2) {
                waited = Duration::ZERO;
                let message = shared.lock().unwrap().announcement();
                for target in &targets {
                    let _ = socket.send_to(&message, target);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
            waited += Duration::from_millis(100);
        }
    });
    Ok(Worker { port, threads: vec![listener, announcer] })
}

fn post(address: &str, path: &str, body: &[u8]) -> io::Result<(u16, Vec<u8>)> {
    let response = match ureq::post(&format!("http://{address}{path}")).timeout(Duration::from_secs(15)).send_bytes(body) {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => return Err(io::Error::other(format!("the master at {address} didn't answer: {error}"))),
    };
    let status = response.status();
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes)?;
    Ok((status, bytes))
}

/// Who this worker's master is, if it has one.
pub fn master_name(home: &Path) -> Option<String> {
    master_in(&saved(home)).map(|master| master.name)
}

/// Pairs the worker whose folder this is with the master at an address, using the code that master shows. Both
/// sides run a key exchange on the code; the worker then proves it had the code by sealing a message with the key,
/// and the master proves the same by answering in kind. Returns the master's name.
pub fn pair_with_master(home: &Path, master: &str, code: &str) -> io::Result<String> {
    let mut saved = saved(home);
    if let Some(name) = master_in(&saved).map(|master| master.name) {
        return Err(io::Error::other(format!("this worker already belongs to {name}; forget that master first (unpair)")));
    }
    if saved["port"].as_u64().is_none() {
        // Not started yet: it will listen on the usual port when it is.
        saved = json!({ "name": crate::platform::hostname(), "port": WORKER_PORT });
    }
    saved["id"] = json!(crate::identity::this_machine().id());
    let address = if master.contains(':') { master.to_string() } else { format!("{master}:{PAIR_PORT}") };
    let (pairing, message) = Pairing::start(code);
    let (status, bytes) = post(&address, "/pair", json!({ "message": BASE64.encode(message), "id": saved["id"] }).to_string().as_bytes())?;
    let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status != 200 {
        return Err(io::Error::other(answer["error"]["message"].as_str().unwrap_or("the master refused to pair").to_string()));
    }
    let key = answer["message"].as_str().and_then(|text| BASE64.decode(text).ok()).and_then(|theirs| pairing.finish(&theirs)).ok_or_else(|| io::Error::other("the master's pairing answer made no sense"))?;
    let proof = json!({ "id": saved["id"], "name": saved["name"], "port": saved["port"] });
    let (status, bytes) = post(&address, "/confirm", &link::seal(&key, b"mc1 pair", proof.to_string().as_bytes()))?;
    let confirmed: Option<Value> = (status == 200).then(|| link::open(&key, b"mc1 paired", &bytes)).flatten().and_then(|plain| serde_json::from_slice(&plain).ok());
    let Some(confirmed) = confirmed else {
        let said: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        return Err(io::Error::other(said["error"]["message"].as_str().unwrap_or("the code is not the one the master shows").to_string()));
    };
    let name = confirmed["name"].as_str().unwrap_or("master").to_string();
    saved["master"] = json!({ "name": name, "key": link::to_hex(&key) });
    state::write_private(&home.join("worker.json"), saved.to_string().as_bytes())?;
    Ok(name)
}

/// Listens for a master that is showing its pairing code, and returns its address and name.
pub fn find_master(listen_on: SocketAddr, time: Duration) -> io::Result<Option<(String, String)>> {
    let socket = UdpSocket::bind(listen_on)?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let deadline = std::time::Instant::now() + time;
    let mut buffer = [0u8; 2048];
    while std::time::Instant::now() < deadline {
        let Ok((length, from)) = socket.recv_from(&mut buffer) else { continue };
        let Ok(message) = serde_json::from_slice::<Value>(&buffer[..length]) else { continue };
        if let (Some("master"), Some(port)) = (message["role"].as_str(), message["pair_port"].as_u64()) {
            return Ok(Some((format!("{}:{port}", from.ip()), message["name"].as_str().unwrap_or("master").to_string())));
        }
    }
    Ok(None)
}

/// Forgets the master. A running worker must be restarted to take a new one.
pub fn unpair(home: &Path) -> io::Result<()> {
    let path = home.join("worker.json");
    let mut saved: Value = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_else(|| json!({}));
    saved["master"] = Value::Null;
    state::write_private(&path, saved.to_string().as_bytes())
}

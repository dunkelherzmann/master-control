//! The master's side of pairing: while its Pair screen is open, the master shows a code, listens for workers that
//! were given it, and announces where it is. A worker that proves it had the code is remembered; one that didn't
//! costs a try, and after five tries the code is replaced.

use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use mc_core::link::{self, Pairing, SharedKey};
use serde_json::{json, Value};
use tiny_http::{Method, Request, Response, Server};

use crate::{add_machine, Machine, ANNOUNCE_PORT};

const TRIES: u32 = 5;

fn new_code() -> String {
    // Eight digits, shown in two groups. Uniform: the random number is far larger than the range.
    let number = u64::from_le_bytes(link::random::<8>()) % 100_000_000;
    format!("{:04} {:04}", number / 10_000, number % 10_000)
}

struct State {
    home: PathBuf,
    name: String,
    code: String,
    tries: u32,
    /// Workers that began the exchange and have yet to prove they had the code, by their id.
    hopeful: HashMap<String, SharedKey>,
    paired: Vec<Machine>,
    until: Instant,
}

/// An open pairing window. Closing it (or dropping it) stops the listening and the announcing.
pub struct Window {
    pub port: u16,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

fn respond(request: Request, status: u16, body: Vec<u8>) {
    let _ = request.respond(Response::from_data(body).with_status_code(status));
}

fn refuse(request: Request, status: u16, code: &str, message: &str) {
    respond(request, status, json!({ "error": { "code": code, "message": message } }).to_string().into_bytes());
}

fn handle(state: &Mutex<State>, mut request: Request) {
    let mut body = Vec::new();
    if request.as_reader().take(64 * 1024).read_to_end(&mut body).is_err() {
        return refuse(request, 400, "bad_input", "the request couldn't be read");
    }
    let from = request.remote_addr().map(SocketAddr::ip);
    let mut state = state.lock().unwrap();
    if Instant::now() > state.until {
        return refuse(request, 410, "expired", "the pairing code has expired; show a new one on the master");
    }
    match (request.method(), request.url()) {
        // The worker's half of the exchange comes in, the master's half goes back. Nothing is settled yet.
        (Method::Post, "/pair") => {
            let asked: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let (Some(message), Some(id)) = (asked["message"].as_str().and_then(|text| BASE64.decode(text).ok()), asked["id"].as_str()) else {
                return refuse(request, 400, "bad_input", "a pairing request carries the worker's id and pairing message");
            };
            let (pairing, answer) = Pairing::start(&state.code);
            let Some(key) = pairing.finish(&message) else {
                return refuse(request, 400, "bad_input", "that isn't a pairing message");
            };
            if state.hopeful.len() > 64 {
                state.hopeful.clear();
            }
            state.hopeful.insert(id.chars().take(64).collect(), key);
            let reply = json!({ "message": BASE64.encode(answer), "name": state.name });
            respond(request, 200, reply.to_string().into_bytes());
        }
        // The proof: a message sealed with the key, which only a worker that had the code can make.
        (Method::Post, "/confirm") => {
            let opened = state.hopeful.iter().find_map(|(id, key)| Some((id.clone(), *key, link::open(key, b"mc1 pair", &body)?)));
            let Some((id, key, plain)) = opened else {
                state.tries += 1;
                if state.tries >= TRIES {
                    // Too many guesses at this code: it is worthless now.
                    state.code = new_code();
                    state.tries = 0;
                    state.hopeful.clear();
                    return refuse(request, 429, "too_many_tries", "too many wrong codes; the master shows a new code now");
                }
                return refuse(request, 403, "wrong_code", "the code is not the one the master shows");
            };
            state.hopeful.remove(&id);
            let said: Value = serde_json::from_slice(&plain).unwrap_or(Value::Null);
            let (Some(port), Some(from)) = (said["port"].as_u64(), from) else {
                return refuse(request, 400, "bad_input", "the worker didn't say where it listens");
            };
            // Where the request came from, not where the worker thinks it is.
            let machine = Machine { name: said["name"].as_str().unwrap_or("worker").chars().take(64).collect(), id, address: SocketAddr::new(from, port as u16).to_string(), key };
            match add_machine(&state.home, machine) {
                Ok(machine) => {
                    state.tries = 0;
                    let reply = link::seal(&key, b"mc1 paired", json!({ "name": state.name, "known_as": machine.name }).to_string().as_bytes());
                    state.paired.push(machine);
                    respond(request, 200, reply);
                }
                Err(error) => refuse(request, 500, "failed", &format!("the pairing couldn't be saved: {error}")),
            }
        }
        _ => refuse(request, 404, "not_found", "only pairing happens on this port"),
    }
}

/// Opens the pairing window: a new code, a listener on `address:port` (port 0 picks a free one), and announcements
/// to `announce_to` (empty: the local network's broadcast address), for `minutes`.
pub fn open(home: PathBuf, name: &str, address: IpAddr, port: u16, announce_to: Vec<SocketAddr>, minutes: u64) -> io::Result<Window> {
    let server = Server::http((address, port)).map_err(|error| io::Error::other(format!("can't listen on port {port}: {error}")))?;
    let port = server.server_addr().to_ip().map_or(port, |address| address.port());
    let state = Arc::new(Mutex::new(State { home, name: name.to_string(), code: new_code(), tries: 0, hopeful: HashMap::new(), paired: Vec::new(), until: Instant::now() + Duration::from_secs(minutes * 60) }));
    let stop = Arc::new(AtomicBool::new(false));

    let (listening, stopping) = (state.clone(), stop.clone());
    let listener = std::thread::spawn(move || {
        while !stopping.load(Ordering::SeqCst) {
            if let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(200)) {
                handle(&listening, request);
            }
        }
    });
    let (stopping, announced) = (stop.clone(), json!({ "master_control": 1, "role": "master", "name": name, "pair_port": port }).to_string());
    let announcer = std::thread::spawn(move || {
        let Ok(socket) = UdpSocket::bind(("0.0.0.0", 0)) else { return };
        let _ = socket.set_broadcast(true);
        let targets = if announce_to.is_empty() { vec![SocketAddr::from(([255, 255, 255, 255], ANNOUNCE_PORT))] } else { announce_to };
        let mut waited = Duration::from_secs(1);
        while !stopping.load(Ordering::SeqCst) {
            if waited >= Duration::from_secs(1) {
                waited = Duration::ZERO;
                for target in &targets {
                    let _ = socket.send_to(announced.as_bytes(), target);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
            waited += Duration::from_millis(100);
        }
    });
    Ok(Window { port, state, stop, threads: vec![listener, announcer] })
}

impl Window {
    /// The code to give the workers. It changes after too many wrong tries.
    pub fn code(&self) -> String {
        self.state.lock().unwrap().code.clone()
    }

    /// The workers paired since the window opened.
    pub fn paired(&self) -> Vec<Machine> {
        self.state.lock().unwrap().paired.clone()
    }

    /// How long the code still works.
    pub fn seconds_left(&self) -> u64 {
        self.state.lock().unwrap().until.saturating_duration_since(Instant::now()).as_secs()
    }

    pub fn expired(&self) -> bool {
        Instant::now() > self.state.lock().unwrap().until
    }

    pub fn close(mut self) {
        self.shut();
    }

    fn shut(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.shut();
    }
}

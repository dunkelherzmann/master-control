//! A master and a worker, each with its own folder, talking over the loopback address as they would over a network.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use mc_agent::worker::{find_master, master_name, pair_with_master, WorkerConfig};
use mc_agent::{Config, Running};
use mc_core::link;
use mctl::pairing::{self, Window};
use mctl::{Machine, Master};
use serde_json::{json, Value};

static NEXT: AtomicU32 = AtomicU32::new(0);

fn folder(what: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("mc-link-test-{}-{}-{what}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&path);
    path
}

struct Worker {
    running: Option<Running>,
    home: PathBuf,
    address: String,
}

impl Worker {
    fn start(home: PathBuf, announce_to: Vec<SocketAddr>) -> Worker {
        // The loopback address only: nothing outside this machine has any business with a test.
        let worker = WorkerConfig { address: std::net::Ipv4Addr::LOCALHOST.into(), port: 0, name: Some("test-worker".into()), announce_to };
        let running = mc_agent::start(Config { home: home.clone(), port: 0, worker: Some(worker) }).expect("the worker starts");
        let address = format!("127.0.0.1:{}", running.worker_port.unwrap());
        Worker { running: Some(running), home, address }
    }

    fn new() -> Worker {
        Worker::start(folder("worker"), Vec::new())
    }

    /// Gives this worker the code of an open pairing window, as someone at the worker would.
    fn pair(&self, window: &Window, code: &str) -> std::io::Result<String> {
        pair_with_master(&self.home, &format!("127.0.0.1:{}", window.port), code)
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn master() -> (Master, PathBuf) {
    let home = folder("master");
    (Master::open(home.clone()).unwrap(), home)
}

/// The master's Pair screen, on the loopback address.
fn window(master: &Master) -> Window {
    pairing::open(master.home().to_path_buf(), master.own_name(), std::net::Ipv4Addr::LOCALHOST.into(), 0, Vec::new(), 10).expect("the pairing window opens")
}

/// Pairs a worker with a master the way it is meant to go, and returns the worker as the master knows it.
fn paired(master: &Master, worker: &Worker) -> Machine {
    let window = window(master);
    worker.pair(&window, &window.code()).expect("pairs");
    let machine = window.paired().pop().expect("the master has it");
    window.close();
    machine
}

fn raw_post(address: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let response = match ureq::post(&format!("http://{address}{path}")).send_bytes(body) {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("{error}"),
    };
    let status = response.status();
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes).unwrap();
    (status, bytes)
}

#[test]
fn a_master_with_the_code_pairs_and_runs_the_workers_tools() {
    let worker = Worker::new();
    let (master, home) = master();
    assert!(master.machines().is_empty());
    assert_eq!(master_name(&worker.home), None);
    let window = window(&master);
    let code = window.code();
    assert_eq!((code.len(), code.chars().filter(char::is_ascii_digit).count()), (9, 8), "eight digits in two groups: {code}");

    assert_eq!(worker.pair(&window, &code).expect("pairs"), "master", "the worker learns its master's name");
    let machine = window.paired().pop().expect("the master has the worker");
    window.close();
    assert_eq!((machine.name.as_str(), machine.address.as_str()), ("test-worker", worker.address.as_str()));
    assert_eq!(master.machines().len(), 1);
    assert_eq!(master_name(&worker.home).as_deref(), Some("master"));

    let status = master.call(&machine, "status", &Value::Null).unwrap();
    assert_eq!((&status["name"], status["os"].as_str()), (&json!("master-control"), Some(std::env::consts::OS)));
    let ran = master.call(&machine, "exec", &json!({"command": "echo over the network"})).unwrap();
    assert_eq!((ran["stdout"].as_str().unwrap().trim(), &ran["exit_code"]), ("over the network", &json!(0)));
    // A tool's own refusal comes through with its code and reason.
    let refused = master.call(&machine, "job", &json!({"id": 4242})).unwrap_err();
    assert_eq!(refused.code, "not_found");
    assert!(refused.message.contains("4242"));
    assert_eq!(master.call(&machine, "no_such_tool", &Value::Null).unwrap_err().code, "unknown_tool");
    assert!(master.tools(&machine).unwrap().iter().any(|tool| tool["name"] == "screenshot"));
    assert_eq!(master.machine("TEST-WORKER").unwrap().id, machine.id, "found by name, whatever the capitals");
    // The worker is known by who it is: host name, system release and hardware address together.
    let identity = mc_agent::identity::this_machine();
    assert_eq!(machine.id, identity.id());
    assert_eq!((machine.id.matches('|').count(), status["identity"]["mac"].as_str()), (2, Some(identity.mac.as_str())));
    assert_eq!(status["identity"]["os_release"], identity.os_release.as_str());
    assert_eq!(master.machine("other").unwrap_err().code, "unknown_machine");

    // A master started again later still has its worker, and its key was never written where others can read it.
    let again = Master::open(home.clone()).unwrap();
    assert_eq!(again.call(&again.machine("test-worker").unwrap(), "status", &Value::Null).unwrap()["os"], std::env::consts::OS);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [home.join("workers.json"), worker.home.join("worker.json")] {
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "{path:?}");
        }
    }
    let audit = std::fs::read_to_string(worker.home.join("audit.log")).unwrap();
    assert!(audit.lines().any(|line| line.contains("\"via\":\"master\"") && line.contains("\"tool\":\"exec\"")), "what the master asked is in the worker's audit log");

    master.unpair("test-worker").unwrap();
    assert!(master.machines().is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_wrong_code_gets_nowhere_and_too_many_change_the_code() {
    let worker = Worker::new();
    let (master, home) = master();
    let window = window(&master);
    let code = window.code();
    let wrong = if code == "0000 0000" { "0000 0001" } else { "0000 0000" };
    let refused = worker.pair(&window, wrong).unwrap_err().to_string();
    assert!(refused.contains("not the one the master shows"), "{refused}");
    assert!(master.machines().is_empty() && window.paired().is_empty());
    assert_eq!((window.code(), master_name(&worker.home)), (code.clone(), None), "one wrong try changes nothing");
    assert!(worker.pair(&window, &code.replace(' ', "")).is_err(), "the code is used as shown, space and all");
    worker.pair(&window, &code).expect("pairs with the right code");
    assert_eq!(window.paired().len(), 1);

    // After five wrong tries the code is replaced, so guessing can't go on against the same one.
    let other = Worker::new();
    let first = window.code();
    let outcomes: Vec<String> = (0..5).map(|_| other.pair(&window, "9999 9999").unwrap_err().to_string()).collect();
    assert!(outcomes[..4].iter().all(|said| said.contains("not the one")) && outcomes[4].contains("new code"), "{outcomes:?}");
    assert_ne!(window.code(), first);
    assert!(other.pair(&window, &first).is_err(), "the old code is worthless");
    other.pair(&window, &window.code()).expect("pairs with the new code");
    // Both test workers are this one machine, so the second pairing takes the first one's place: one machine, one entry.
    assert_eq!((window.paired()[1].name.as_str(), master.machines().len()), ("test-worker", 1));

    // One code admits several workers while the window is open; once it is closed, nobody is listening.
    let port = window.port;
    window.close();
    let late = Worker::new();
    assert!(pair_with_master(&late.home, &format!("127.0.0.1:{port}"), &first).unwrap_err().to_string().contains("didn't answer"));
    // A worker that has a master won't take another until it is told to forget the first.
    let second_window = self::window(&master);
    assert!(worker.pair(&second_window, &second_window.code()).unwrap_err().to_string().contains("already belongs to master"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_paired_worker_answers_its_master_only() {
    let worker = Worker::new();
    let (owner, owner_home) = master();
    let (stranger, stranger_home) = master();
    let machine = paired(&owner, &worker);
    // Another master's window gets a refusal from the worker itself.
    let theirs = window(&stranger);
    assert!(worker.pair(&theirs, &theirs.code()).unwrap_err().to_string().contains("already belongs"));
    assert!(stranger.machines().is_empty());
    assert_eq!(raw_post(&worker.address, "/pair", b"{}").0, 404, "a worker takes no pairing requests: it asks, it isn't asked");
    // A message sealed with some other key, a message in the clear, and the plain API: none get an answer.
    let other_key = link::random::<32>();
    assert_eq!(raw_post(&worker.address, "/sealed", &link::seal(&other_key, b"mc1 request", br#"{"hello":true}"#)).0, 403);
    assert_eq!(raw_post(&worker.address, "/sealed", br#"{"hello":true}"#).0, 403);
    assert_eq!(raw_post(&worker.address, "/sealed", b"").0, 403);
    assert_eq!(raw_post(&worker.address, "/v1/exec", br#"{"command":"echo no"}"#).0, 404);
    assert_eq!(raw_post(&worker.address, "/mcp", b"{}").0, 404);
    let token = worker.running.as_ref().unwrap().token().to_string();
    let with_token = ureq::get(&format!("http://{}/v1/status", worker.address)).set("Authorization", &format!("Bearer {token}")).call();
    assert!(matches!(with_token, Err(ureq::Error::Status(404, _))), "even the worker's own token opens nothing on the network port");

    // A recorded message sent again is refused: the worker has seen that number in that conversation.
    let saved: Value = serde_json::from_slice(&std::fs::read(owner_home.join("workers.json")).unwrap()).unwrap();
    let key: [u8; 32] = link::from_hex(saved["workers"][0]["key"].as_str().unwrap()).unwrap().try_into().unwrap();
    let say = |message: Value| {
        let sealed = link::seal(&key, b"mc1 request", message.to_string().as_bytes());
        (sealed.clone(), raw_post(&worker.address, "/sealed", &sealed))
    };
    let open = |sealed: &[u8], answer: &[u8]| -> Value {
        let context = [b"mc1 answer ".as_slice(), &sealed[..link::NONCE]].concat();
        serde_json::from_slice(&link::open(&key, &context, answer).expect("sealed by the worker for this request")).unwrap()
    };
    let (hello, (_, greeting)) = say(json!({"hello": true}));
    let session = open(&hello, &greeting)["ok"]["session"].clone();
    let (recorded, (status, answer)) = say(json!({"session": session, "number": 1, "tool": "exec", "args": {"command": "echo once"}}));
    assert_eq!((status, open(&recorded, &answer)["ok"]["stdout"].as_str().unwrap().trim()), (200, "once"));
    let (_, replayed) = raw_post(&worker.address, "/sealed", &recorded);
    assert_eq!(open(&recorded, &replayed)["error"]["code"], "stale_session", "the same bytes again run nothing");
    let (sealed, (_, answer)) = say(json!({"session": "made-up", "number": 9, "tool": "status"}));
    assert_eq!(open(&sealed, &answer)["error"]["code"], "stale_session");
    // An answer can't be passed off as the answer to another request.
    let (other, _) = say(json!({"hello": true}));
    assert!(link::open(&key, &[b"mc1 answer ".as_slice(), &other[..link::NONCE]].concat(), &answer).is_none());

    // The owner's own calls go on working; it starts a new conversation by itself.
    assert_eq!(owner.call(&machine, "status", &Value::Null).unwrap()["name"], "master-control");
    for home in [owner_home, stranger_home] {
        let _ = std::fs::remove_dir_all(home);
    }
}

#[test]
fn workers_announce_themselves_and_are_found_again_after_moving() {
    let listen = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let mut worker = Worker::start(folder("worker"), vec![listen]);
    let (master, home) = master();
    let heard = mctl::scan(listen, Duration::from_secs(4)).unwrap();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!((heard[0].name.as_str(), heard[0].address.as_str(), heard[0].paired), ("test-worker", worker.address.as_str(), false));
    // The worker finds a master that is showing its code the same way: by listening.
    let found_on = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let showing = pairing::open(home.clone(), "the-master", std::net::Ipv4Addr::LOCALHOST.into(), 0, vec![found_on], 10).unwrap();
    let (address, name) = find_master(found_on, Duration::from_secs(4)).unwrap().expect("the master is heard");
    assert_eq!((address.as_str(), name.as_str()), (format!("127.0.0.1:{}", showing.port).as_str(), "the-master"));
    assert_eq!(pair_with_master(&worker.home, &address, &showing.code()).unwrap(), "the-master");
    let machine = showing.paired().pop().unwrap();
    showing.close();
    assert_eq!(heard[0].id, machine.id);

    // The worker restarts on another port, as a machine might come back with another address.
    let worker_home = worker.home.clone();
    worker.stop();
    let mut moved = Worker::start(worker_home, vec![listen]);
    worker.home = folder("unused");
    assert_ne!(moved.address, machine.address);
    assert_eq!(master.call(&machine, "status", &Value::Null).unwrap_err().code, "unreachable");
    let heard = mctl::scan(listen, Duration::from_secs(4)).unwrap();
    assert!(heard[0].paired, "it remembers its master across a restart");
    master.remember_addresses(&heard).unwrap();
    let found = master.machine("test-worker").unwrap();
    assert_eq!(found.address, moved.address);
    assert_eq!(master.call(&found, "exec", &json!({"command": "echo found again"})).unwrap()["stdout"].as_str().unwrap().trim(), "found again");

    // Told to forget its master, the worker takes a code again, and the old master is a stranger.
    moved.stop();
    mc_agent::worker::unpair(&moved.home).unwrap();
    let reset_home = moved.home.clone();
    moved.home = folder("unused");
    let reset = Worker::start(reset_home, Vec::new());
    assert_eq!(master_name(&reset.home), None);
    let mut stale = found.clone();
    stale.address = reset.address.clone();
    assert_eq!(master.call(&stale, "status", &Value::Null).unwrap_err().code, "not_paired");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn the_masters_mcp_server_offers_every_workers_tools() {
    let worker = Worker::new();
    let (master, home) = master();
    paired(&master, &worker);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mctl"))
        .arg("mcp")
        .env("MC_MASTER_HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for message in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "machines", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "exec", "arguments": {"machine": "test-worker", "command": "echo through the master"}}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "exec", "arguments": {"command": "echo nowhere"}}}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "exec", "arguments": {"machine": "elsewhere", "command": "echo nowhere"}}}),
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "job", "arguments": {"machine": "test-worker", "id": 4242}}}),
        json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "exec", "arguments": {"machine": "master", "command": "echo on the master itself"}}}),
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "push", "arguments": {"machine": "self", "path": home.join("own-file.txt").to_str().unwrap(), "text": "written here"}}}),
        json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "batch", "arguments": {"machine": "test-worker", "steps": [{"tool": "exec", "args": {"command": "echo in one trip"}}, {"tool": "status"}]}}}),
        json!({"jsonrpc": "2.0", "id": 11, "method": "tools/call", "params": {"name": "guide", "arguments": {}}}),
    ] {
        writeln!(input, "{message}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    let answers: Vec<Value> = String::from_utf8(output.stdout).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(answers.len(), 11);
    // The guide for agents comes from the server itself, and names every tool the server offers.
    let guide = answers[10]["result"]["structuredContent"]["guide"].as_str().unwrap();
    assert!(guide.starts_with("# Master Control: a guide for coding agents") && answers[0]["result"]["instructions"].as_str().unwrap().contains("`guide`"));
    for tool in answers[1]["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert!(guide.contains(&format!("`{name}`")), "the guide doesn't mention {name}");
    }
    // One call to the master, one trip to the worker, two tools run there.
    let batch = &answers[9]["result"]["structuredContent"];
    assert_eq!((&batch["done"], batch["steps"][0]["result"]["stdout"].as_str().map(str::trim), &batch["steps"][1]["result"]["name"]), (&json!(2), Some("in one trip"), &json!("master-control")), "{}", answers[9]);
    assert_eq!(answers[0]["result"]["serverInfo"]["name"], "master-control");
    let tools = answers[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "machines");
    let exec = tools.iter().find(|tool| tool["name"] == "exec").expect("the worker's exec");
    assert_eq!(exec["inputSchema"]["required"], json!(["machine"]));
    assert!(exec["inputSchema"]["properties"]["machine"].is_object() && exec["inputSchema"]["properties"]["command"].is_object());
    assert_eq!(tools[1]["name"], "guide");
    assert!(tools.len() > 20 && tools.iter().skip(2).all(|tool| tool["inputSchema"]["required"][0] == "machine"));
    let machines = &answers[2]["result"]["structuredContent"]["machines"];
    assert_eq!((&machines[0]["name"], &machines[0]["role"], &machines[0]["reachable"], machines[0]["os"].as_str()), (&json!("master"), &json!("master"), &json!(true), Some(std::env::consts::OS)), "the master's own machine comes first");
    assert_eq!((&machines[1]["name"], &machines[1]["role"], &machines[1]["reachable"], machines[1]["os"].as_str()), (&json!("test-worker"), &json!("worker"), &json!(true), Some(std::env::consts::OS)));
    // The same tools work on the master itself, by its name or as "self".
    assert_eq!(answers[7]["result"]["structuredContent"]["stdout"].as_str().unwrap().trim(), "on the master itself");
    assert_eq!(answers[8]["result"]["isError"], false);
    assert_eq!(std::fs::read_to_string(home.join("own-file.txt")).unwrap(), "written here");
    assert_eq!(answers[3]["result"]["structuredContent"]["stdout"].as_str().unwrap().trim(), "through the master");
    for (index, said) in [(4, "machine is required"), (5, "no worker named elsewhere"), (6, "4242")] {
        // (and the refusal for an unknown name says what the master itself is called)
        assert_eq!(answers[index]["result"]["isError"], true);
        assert!(answers[index]["result"]["content"][0]["text"].as_str().unwrap().contains(said), "{}", answers[index]);
    }
    let _ = std::fs::remove_dir_all(home);
}

/// Start finds each AI agent by its own settings folder and adds Master Control to that agent's own list of servers,
/// in that list's own shape; Cleanup takes it out and leaves everything else as it was.
#[test]
fn start_registers_with_every_agent_found_and_cleanup_takes_it_out_again() {
    let home = folder("agents-master");
    let install = folder("agents-install");
    let user = folder("agents-user");
    let app_data = if cfg!(target_os = "macos") { user.join("Library").join("Application Support") } else if cfg!(windows) { user.join("AppData").join("Roaming") } else { user.join(".config") };
    for folder in [user.join(".codex"), user.join(".gemini").join("antigravity"), user.join(".cursor"), app_data.join("Claude"), app_data.join("Code").join("User")] {
        std::fs::create_dir_all(folder).unwrap();
    }
    // Each as its owner left it: other servers, other settings, in an order of their own.
    let claude_code = user.join(".claude.json");
    std::fs::write(&claude_code, r#"{"zeta": 1, "mcpServers": {"other": {"type": "stdio", "command": "x", "args": []}}, "alpha": [1, 2]}"#).unwrap();
    let codex = user.join(".codex").join("config.toml");
    std::fs::write(&codex, "model = \"gpt\"\n\n[mcp_servers.node_repl]\ncommand = \"/x/node_repl\"\nargs = []\n").unwrap();
    let claude_app = app_data.join("Claude").join("claude_desktop_config.json");
    std::fs::write(&claude_app, r#"{"preferences": {"a": true}}"#).unwrap();
    // One with a note in it, which this program can't safely rewrite.
    let vs_code = app_data.join("Code").join("User").join("mcp.json");
    std::fs::write(&vs_code, "{\n  // my servers\n  \"servers\": {}\n}\n").unwrap();
    let mctl = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_mctl")).args(args).env("MC_MASTER_HOME", &home).env("MC_INSTALL_DIR", &install).env("MC_AGENTS_HOME", &user).env_remove("MC_CLAUDE_PROGRAM").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let json = |path: &std::path::Path| -> Value { serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap() };
    let before = mctl(&["agents"]);
    assert_eq!(before.lines().filter(|line| line.contains("doesn't have master-control")).count(), 6, "{before}");

    let said = mctl(&["start"]);
    let program = install.join(if cfg!(windows) { "mctl.exe" } else { "mctl" });
    let program_text = program.to_str().unwrap();
    for agent in ["Claude Code: registered in", "Claude app: registered in", "Codex: registered in", "Antigravity: registered in", "Cursor: registered in"] {
        assert!(said.contains(agent), "{agent}: {said}");
    }
    assert!(said.contains("VS Code: could NOT be registered") && said.contains("Not found on this machine: Gemini CLI, Windsurf."), "{said}");
    assert!(said.contains("Any other agent that speaks MCP"), "{said}");
    let guide = std::fs::read_to_string(install.join("AGENT-GUIDE.md")).unwrap();
    assert!(guide.contains("## Connecting") && said.contains("AGENT-GUIDE.md"), "the guide is left beside the program: {said}");
    assert_eq!(mctl(&["guide"]), guide);

    let now = json(&claude_code);
    assert_eq!(now["mcpServers"]["master-control"], json!({"type": "stdio", "command": program_text, "args": ["mcp"], "env": {}}));
    assert_eq!((&now["mcpServers"]["other"]["command"], &now["zeta"], &now["alpha"]), (&json!("x"), &json!(1), &json!([1, 2])), "the rest is as it was");
    let text = std::fs::read_to_string(&claude_code).unwrap();
    assert!(text.find("\"zeta\"").unwrap() < text.find("\"mcpServers\"").unwrap() && text.find("\"mcpServers\"").unwrap() < text.find("\"alpha\"").unwrap(), "and in the order it was: {text}");
    assert_eq!(json(&claude_app)["mcpServers"]["master-control"], json!({"command": program_text, "args": ["mcp"]}));
    assert_eq!(json(&claude_app)["preferences"]["a"], true);
    assert_eq!(json(&user.join(".gemini").join("antigravity").join("mcp_config.json"))["mcpServers"]["master-control"]["args"], json!(["mcp"]));
    assert_eq!(json(&user.join(".cursor").join("mcp.json"))["mcpServers"]["master-control"]["command"], program_text);
    let codex_now = std::fs::read_to_string(&codex).unwrap();
    assert!(codex_now.starts_with("model = \"gpt\"\n\n[mcp_servers.node_repl]") && codex_now.contains("[mcp_servers.master-control]\ncommand = '") && codex_now.contains(program_text), "{codex_now}");
    assert_eq!(std::fs::read_to_string(&vs_code).unwrap(), "{\n  // my servers\n  \"servers\": {}\n}\n", "a file it can't read is left exactly as it was");
    // What each file was before is kept beside it.
    assert_eq!(std::fs::read_to_string(user.join(".codex").join("config.toml.before-master-control")).unwrap(), "model = \"gpt\"\n\n[mcp_servers.node_repl]\ncommand = \"/x/node_repl\"\nargs = []\n");
    let after = mctl(&["agents"]);
    assert_eq!(after.lines().filter(|line| line.contains("\thas master-control")).count(), 5, "{after}");
    // Again: nothing doubled.
    mctl(&["start"]);
    assert_eq!(std::fs::read_to_string(&codex).unwrap(), codex_now);

    let said = mctl(&["cleanup"]);
    for agent in ["Claude Code", "Claude app", "Codex", "Antigravity", "Cursor"] {
        assert!(said.contains(&format!("{agent}: the master-control MCP server is removed")), "{agent}: {said}");
    }
    assert_eq!(std::fs::read_to_string(&codex).unwrap(), "model = \"gpt\"\n\n[mcp_servers.node_repl]\ncommand = \"/x/node_repl\"\nargs = []\n", "Codex's settings are what they were");
    assert_eq!(json(&claude_code), json!({"zeta": 1, "mcpServers": {"other": {"type": "stdio", "command": "x", "args": []}}, "alpha": [1, 2]}));
    assert!(json(&claude_app)["mcpServers"].get("master-control").is_none());
    assert!(!install.join("AGENT-GUIDE.md").exists());
    for path in [home, install, user] {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// The master's own machine is observed like a worker's: the recorder is the master's program, started again.
#[cfg(any(windows, target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
#[test]
fn the_master_observes_a_command_on_its_own_machine() {
    use std::io::BufRead;
    let (_, home) = master();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mctl"))
        .arg("mcp")
        .env("MC_MASTER_HOME", &home)
        .env_remove("MC_AGENT_PROGRAM")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut number = 0;
    let mut call = |name: &str, arguments: Value| {
        number += 1;
        writeln!(input, "{}", json!({"jsonrpc": "2.0", "id": number, "method": "tools/call", "params": {"name": name, "arguments": arguments}})).unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let answer: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(answer["result"]["isError"], false, "{name}: {answer}");
        answer["result"]["structuredContent"].clone()
    };
    let session = call("observe_start", json!({"machine": "master", "command": "echo watched"}))["session"].as_u64().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while call("summary", json!({"machine": "master", "session": session}))["state"] == "running" {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    let verdict = call("observe_stop", json!({"machine": "master", "session": session}));
    assert_eq!((&verdict["state"], &verdict["exit_code"], &verdict["verdict"]), (&json!("exited"), &json!(0), &json!("ok")), "{verdict}");
    assert_eq!(call("logs", json!({"machine": "master", "session": session}))["text"].as_str().unwrap().trim(), "watched");
    assert!(verdict["summary"]["by_class"]["process"].as_u64().unwrap() >= 2, "{verdict}");
    drop(input);
    child.wait().unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_master_with_no_workers_still_controls_itself() {
    let (master, home) = master();
    assert!(master.machines().is_empty());
    assert_eq!(master.run("master", "status", &Value::Null).unwrap()["os"], std::env::consts::OS);
    assert_eq!(master.run("LOCAL", "exec", &json!({"command": "echo alone"})).unwrap()["stdout"].as_str().unwrap().trim(), "alone");
    assert_eq!(master.run("master", "job", &json!({"id": 4242})).unwrap_err().code, "not_found", "a tool's refusal keeps its code");
    // Each call is noted for the app's status section: which machine, which tool, how it ended — not what was sent.
    let noted = mctl::recent_activity(&home, 10);
    let seen: Vec<(&str, &str, bool, &str)> = noted.iter().map(|call| (call.machine.as_str(), call.tool.as_str(), call.ok, call.code.as_str())).collect();
    assert_eq!(seen, [("master", "status", true, ""), ("LOCAL", "exec", true, ""), ("master", "job", false, "not_found")]);
    assert!(!std::fs::read_to_string(home.join("activity.log")).unwrap().contains("echo alone"));
    // Who the master is talking to right now, for the app's status lights: a call just made counts for a moment.
    assert!(mctl::active_machines(&home, 5_000).contains(&"master".to_string()));
    std::thread::sleep(Duration::from_millis(60));
    assert!(mctl::active_machines(&home, 20).is_empty(), "and no longer once it is over");
    // Several steps in one call run on the machine that is named, like any other tool.
    let batch = master.run("master", "batch", &json!({"steps": [{"tool": "exec", "args": {"command": "echo one"}}, {"tool": "job", "args": {"id": {"from_step": 1, "path": "job"}}}]})).unwrap();
    assert_eq!((&batch["ok"], &batch["done"], batch["steps"][1]["result"]["state"].as_str()), (&json!(true), &json!(2), Some("exited")), "{batch}");
    let unknown = master.run("nowhere", "status", &Value::Null).unwrap_err();
    assert_eq!(unknown.code, "unknown_machine");
    assert!(unknown.message.contains("master"), "{}", unknown.message);
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(unix)]
#[test]
fn the_masters_start_and_cleanup_install_and_remove_everything() {
    use std::os::unix::fs::PermissionsExt;
    let worker = Worker::new();
    let home = folder("master");
    let install = folder("install");
    // A folder to stand for the user's own, so no real agent's settings are touched.
    let agents_home = folder("agents-home");
    std::fs::create_dir_all(&agents_home).unwrap();
    // A stand-in for Claude Code's command that writes down what it was asked.
    let asked = home.with_extension("asked");
    let claude = home.with_extension("claude");
    std::fs::write(&claude, format!("#!/bin/sh\necho \"$@\" >> '{}'\n", asked.display())).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mctl = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_mctl")).args(args).env("MC_MASTER_HOME", &home).env("MC_INSTALL_DIR", &install).env("MC_CLAUDE_PROGRAM", &claude).env("MC_AGENTS_HOME", &agents_home).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    let said = mctl(&["start"]);
    let installed = install.join("mctl");
    assert!(installed.is_file() && said.contains("Claude Code: registered (through "), "{said}");
    assert!(said.contains("Not found on this machine: Claude app, Codex, Antigravity, Gemini CLI, Cursor, Windsurf, VS Code."), "{said}");
    let log = std::fs::read_to_string(&asked).unwrap();
    assert_eq!(log.lines().collect::<Vec<_>>(), ["mcp remove master-control -s user", format!("mcp add -s user master-control -- {} mcp", installed.display()).as_str()]);
    mctl(&["start"]);
    assert_eq!(std::fs::read_to_string(&asked).unwrap().lines().count(), 4, "run again, it repairs: the same steps, nothing doubled");

    paired(&Master::open(home.clone()).unwrap(), &worker);
    assert_eq!(master_name(&worker.home).as_deref(), Some("master"));
    let said = mctl(&["cleanup"]);
    assert!(said.contains("test-worker has forgotten this master") && said.contains("MCP server is removed"), "{said}");
    assert_eq!(master_name(&worker.home), None, "the worker is free to be paired again");
    assert!(!home.exists() && !installed.exists(), "nothing of the master is left");
    assert_eq!(std::fs::read_to_string(&asked).unwrap().lines().last(), Some("mcp remove master-control -s user"));
    // And it can take a new master straight away, without a restart.
    let (next, next_home) = master();
    assert_eq!(next.call(&paired(&next, &worker), "status", &Value::Null).unwrap()["name"], "master-control");
    for path in [asked, claude, next_home, agents_home] {
        let _ = std::fs::remove_file(&path).or_else(|_| std::fs::remove_dir_all(&path));
    }
}

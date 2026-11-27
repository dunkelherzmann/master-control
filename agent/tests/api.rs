//! The agent through its real HTTP port, each test with its own agent and its own home folder.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use mc_agent::{Config, Running};
use serde_json::{json, Value};

static NEXT: AtomicU32 = AtomicU32::new(0);

struct Test {
    running: Option<Running>,
    home: PathBuf,
    port: u16,
    token: String,
}

impl Test {
    fn new() -> Test {
        let home = std::env::temp_dir().join(format!("mc-agent-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&home);
        Test::in_home(home)
    }

    fn in_home(home: PathBuf) -> Test {
        let running = mc_agent::start(Config { home: home.clone(), port: 0, worker: None }).expect("the agent starts");
        Test { port: running.port, token: running.token().to_string(), running: Some(running), home }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn finish(request: Result<ureq::Response, ureq::Error>) -> (u16, Vec<u8>) {
        let response = match request {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("the request didn't reach the agent: {error}"),
        };
        let status = response.status();
        let mut bytes = Vec::new();
        response.into_reader().read_to_end(&mut bytes).unwrap();
        (status, bytes)
    }

    fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let (status, bytes) = Test::finish(
            ureq::post(&self.url(path)).set("Authorization", &format!("Bearer {}", self.token)).send_string(&body.to_string()),
        );
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Calls a tool that is expected to work.
    fn tool(&self, name: &str, args: Value) -> Value {
        let (status, answer) = self.post(&format!("/v1/{name}"), &args);
        assert_eq!(status, 200, "{name} failed: {answer}");
        answer
    }

    /// Calls a tool that is expected to be refused; returns the status and the error code.
    fn refused(&self, name: &str, args: Value) -> (u16, String) {
        let (status, answer) = self.post(&format!("/v1/{name}"), &args);
        (status, answer["error"]["code"].as_str().unwrap_or_default().to_string())
    }

    fn mcp(&self, message: Value) -> (u16, Value) {
        self.post("/mcp", &message)
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

impl Drop for Test {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// A command that prints `started` and then stays for about a minute.
fn long_command() -> &'static str {
    if cfg!(windows) {
        "echo started& ping -n 60 127.0.0.1 >nul"
    } else {
        "echo started; sleep 60"
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "gave up waiting until {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ------------------------------------------------------------------------------------------------ access

#[test]
fn a_request_needs_the_token() {
    let test = Test::new();
    let (status, _) = Test::finish(ureq::get(&test.url("/v1/status")).call());
    assert_eq!(status, 401);
    let (status, _) = Test::finish(ureq::get(&test.url("/v1/status")).set("Authorization", "Bearer 1234").call());
    assert_eq!(status, 401);
    let (status, _) = Test::finish(ureq::get(&test.url("/v1/status")).set("Authorization", &format!("Bearer {}x", test.token)).call());
    assert_eq!(status, 401);
    let (status, bytes) = Test::finish(ureq::get(&test.url("/v1/status")).set("Authorization", &format!("Bearer {}", test.token)).call());
    assert_eq!(status, 200);
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["name"], "master-control");
}

#[test]
fn requests_from_web_pages_and_other_host_names_are_refused() {
    let test = Test::new();
    let (status, _) = Test::finish(
        ureq::get(&test.url("/v1/status"))
            .set("Authorization", &format!("Bearer {}", test.token))
            .set("Origin", "http://example.com")
            .call(),
    );
    assert_eq!(status, 403);
    // A page that points its own name at 127.0.0.1 sends that name as Host.
    let mut stream = TcpStream::connect(("127.0.0.1", test.port)).unwrap();
    write!(stream, "GET /v1/status HTTP/1.1\r\nHost: evil.example:{}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", test.port, test.token)
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
}

#[test]
fn the_token_survives_a_restart_and_only_the_user_can_read_it() {
    let mut first = Test::new();
    let (home, token) = (first.home.clone(), first.token.clone());
    first.stop();
    assert_eq!(token.len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for (name, mode) in [("token", 0o600), ("agent.json", 0o600), ("audit.log", 0o600), ("", 0o700)] {
            assert_eq!(std::fs::metadata(home.join(name)).unwrap().permissions().mode() & 0o777, mode, "{name}");
        }
    }
    let second = Test::in_home(home.clone());
    assert_eq!(second.token, token);
    let file: Value = serde_json::from_str(&std::fs::read_to_string(home.join("agent.json")).unwrap()).unwrap();
    assert_eq!(file["port"], second.port);
    assert_eq!(file["pid"], std::process::id());
    std::mem::forget(first);
}

// ------------------------------------------------------------------------------------------------ status, routes

#[test]
fn status_describes_the_machine() {
    let test = Test::new();
    let status = test.tool("status", json!({}));
    assert_eq!(status["version"], mc_core::VERSION);
    assert_eq!(status["os"], std::env::consts::OS);
    assert_eq!(status["pid"], std::process::id());
    assert!(status["elevated"].is_boolean());
    let identity = &status["identity"];
    assert_eq!(identity["id"].as_str().unwrap(), format!("{}|{}|{}", identity["hostname"].as_str().unwrap().to_lowercase(), identity["os_release"].as_str().unwrap(), identity["mac"].as_str().unwrap()));
    assert_eq!(identity["mac"].as_str().unwrap().len(), 17, "a hardware address: {identity}");
    assert!(["x11", "wayland", "windows", "macos", "none"].contains(&status["session"].as_str().unwrap()));
    assert_eq!(status["capabilities"]["exec"], true);
    assert_eq!(status["capabilities"]["screen"], status["capabilities"]["input"]);
    assert_eq!(status["capabilities"]["observe"], cfg!(any(windows, target_os = "macos", all(target_os = "linux", target_arch = "x86_64"))));
    assert_eq!(status["tools"], json!(["status", "exec", "job", "jobs", "kill", "push", "pull", "screenshot", "windows", "focus", "arrange", "apps", "launch", "desktops", "click", "double_click", "right_click", "move", "drag", "scroll", "type_text", "key", "clipboard_get", "clipboard_set", "tree", "find", "wait_for", "press", "set_value", "toggle", "expand", "select", "observe_start", "observe_stop", "events", "summary", "logs", "sessions", "take_over", "batch"]));
}

#[test]
fn wrong_routes_tools_and_arguments_are_refused_with_a_reason() {
    let test = Test::new();
    assert_eq!(test.refused("nonsense", json!({})), (404, "unknown_tool".into()));
    assert_eq!(test.refused("exec", json!({"comand": "echo hi"})), (400, "bad_input".into()));
    assert_eq!(test.refused("exec", json!({})), (400, "bad_input".into()));
    assert_eq!(test.refused("exec", json!({"command": "echo hi", "argv": ["echo"]})), (400, "bad_input".into()));
    assert_eq!(test.refused("exec", json!({"command": "echo hi", "timeout_s": 0})), (400, "bad_input".into()));
    assert_eq!(test.refused("exec", json!({"command": "echo hi", "cwd": "~/no-such-folder-mc"})), (404, "not_found".into()));
    assert_eq!(test.refused("exec", json!([1, 2])), (400, "bad_input".into()));
    assert_eq!(test.refused("job", json!({"id": 999})), (404, "not_found".into()));
    assert_eq!(test.refused("job", json!({"id": "one"})), (400, "bad_input".into()));
    let (status, bytes) =
        Test::finish(ureq::post(&test.url("/v1/exec")).set("Authorization", &format!("Bearer {}", test.token)).send_string("{not json"));
    assert_eq!((status, serde_json::from_slice::<Value>(&bytes).unwrap()["error"]["code"].as_str()), (400, Some("bad_input")));
    let (status, _) = Test::finish(ureq::get(&test.url("/v1/exec")).set("Authorization", &format!("Bearer {}", test.token)).call());
    assert_eq!(status, 405);
    let (status, _) = Test::finish(ureq::get(&test.url("/elsewhere")).set("Authorization", &format!("Bearer {}", test.token)).call());
    assert_eq!(status, 404);
}

// ------------------------------------------------------------------------------------------------ exec and jobs

#[test]
fn exec_runs_a_shell_command_and_reports_how_it_ended() {
    let test = Test::new();
    let ok = test.tool("exec", json!({"command": "echo hello"}));
    assert_eq!(ok["stdout"].as_str().unwrap().trim(), "hello");
    assert_eq!((&ok["exit_code"], &ok["timed_out"], &ok["stdout_truncated"]), (&json!(0), &json!(false), &json!(false)));
    let failed = test.tool("exec", json!({"command": "echo oops 1>&2 && exit 7"}));
    assert_eq!(failed["exit_code"], 7);
    assert_eq!(failed["stderr"].as_str().unwrap().trim(), "oops");
    assert_eq!(failed["stdout"], "");
}

#[test]
fn exec_runs_a_program_with_its_arguments_as_given() {
    let test = Test::new();
    let answer = test.tool("exec", json!({"argv": ["cargo", "--version"]}));
    assert_eq!(answer["exit_code"], 0);
    assert!(answer["stdout"].as_str().unwrap().starts_with("cargo "));
    assert_eq!(test.refused("exec", json!({"argv": ["no-such-program-mc"]})), (500, "failed".into()));
}

#[test]
fn exec_passes_the_folder_the_environment_and_the_input() {
    let test = Test::new();
    let folder = test.home.join("work folder");
    std::fs::create_dir_all(&folder).unwrap();
    let (print_folder, print_variable) = if cfg!(windows) { ("cd", "echo %MC_TEST_VALUE%") } else { ("pwd", "echo $MC_TEST_VALUE") };
    let answer = test.tool("exec", json!({"command": print_folder, "cwd": folder.to_str().unwrap()}));
    assert!(answer["stdout"].as_str().unwrap().trim().ends_with("work folder"), "{answer}");
    let answer = test.tool("exec", json!({"command": print_variable, "env": {"MC_TEST_VALUE": "a b, c=d"}}));
    assert_eq!(answer["stdout"].as_str().unwrap().trim(), "a b, c=d");
    let answer = test.tool("exec", json!({"command": "sort", "stdin": "pear\napple\n"}));
    assert_eq!(answer["stdout"].as_str().unwrap().split_whitespace().collect::<Vec<_>>(), ["apple", "pear"]);
    assert_eq!(test.refused("exec", json!({"command": "echo", "env": {"A=B": "x"}})), (400, "bad_input".into()));
}

#[test]
fn exec_stops_a_command_that_runs_past_its_time() {
    let test = Test::new();
    let began = Instant::now();
    let answer = test.tool("exec", json!({"command": long_command(), "timeout_s": 1}));
    assert_eq!(answer["timed_out"], true);
    assert!(answer["stdout"].as_str().unwrap().contains("started"));
    assert!(began.elapsed() < Duration::from_secs(15), "took {:?}", began.elapsed());
    let state = test.tool("job", json!({"id": answer["job"]}));
    assert_eq!(state["state"], "timed_out");
}

#[test]
fn a_detached_job_can_be_followed_and_stopped() {
    let test = Test::new();
    let started = test.tool("exec", json!({"command": long_command(), "detach": true}));
    let id = started["job"].clone();
    assert!(started["pid"].as_u64().unwrap() > 0);
    wait_until("the job prints", || test.tool("job", json!({"id": id}))["stdout"].as_str().unwrap().contains("started"));
    let running = test.tool("job", json!({"id": id}));
    assert_eq!((&running["state"], &running["exit_code"]), (&json!("running"), &Value::Null));
    assert_eq!(test.tool("status", json!({}))["jobs_running"], 1);
    // Reading on from where the last answer ended gives nothing new.
    let more = test.tool("job", json!({"id": id, "stdout_from": running["stdout_next"]}));
    assert_eq!((&more["stdout"], &more["stdout_more"]), (&json!(""), &json!(false)));
    let killed = test.tool("kill", json!({"id": id}));
    assert_eq!((&killed["state"], &killed["was_running"]), (&json!("killed"), &json!(true)));
    assert!(killed["exit_code"].is_number());
    assert_eq!(test.tool("kill", json!({"id": id}))["was_running"], false);
    let listed = test.tool("jobs", json!({}));
    assert_eq!(listed["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(listed["jobs"][0]["state"], "killed");
    assert_eq!(test.tool("status", json!({}))["jobs_running"], 0);
}

#[test]
fn a_job_can_be_waited_for() {
    let test = Test::new();
    let started = test.tool("exec", json!({"command": "echo done", "detach": true}));
    let ended = test.tool("job", json!({"id": started["job"], "wait_ms": 20000}));
    assert_eq!((&ended["state"], &ended["exit_code"]), (&json!("exited"), &json!(0)));
    assert_eq!(ended["stdout"].as_str().unwrap().trim(), "done");
}

fn alive(pid: u64) -> bool {
    if cfg!(windows) {
        let output = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"]).output().unwrap();
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    } else {
        std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().unwrap().success()
    }
}

#[test]
fn stopping_a_job_stops_what_it_started() {
    let test = Test::new();
    let command = if cfg!(windows) {
        "powershell -NoProfile -Command \"$p = Start-Process ping -ArgumentList '-n','60','127.0.0.1' -PassThru -NoNewWindow -RedirectStandardOutput NUL; $p.Id; Wait-Process -Id $p.Id\""
    } else {
        "sleep 60 & echo $!; wait"
    };
    let started = test.tool("exec", json!({"command": command, "detach": true}));
    let mut child = 0;
    wait_until("the job prints its child's pid", || {
        child = test.tool("job", json!({"id": started["job"]}))["stdout"].as_str().unwrap().trim().parse().unwrap_or(0);
        child != 0
    });
    assert!(alive(child), "the child {child} should be running");
    test.tool("kill", json!({"id": started["job"]}));
    wait_until("the child is gone", || !alive(child));
}

#[test]
fn long_output_is_cut_to_its_end_and_can_be_read_in_full() {
    let test = Test::new();
    // About 600 KB: more than one answer carries.
    let command = if cfg!(windows) {
        "powershell -NoProfile -Command \"1..60000 | ForEach-Object { 'line ' + $_.ToString('00000') }\""
    } else {
        "i=1; while [ $i -le 60000 ]; do printf 'line %05d\\n' $i; i=$((i+1)); done"
    };
    let answer = test.tool("exec", json!({"command": command, "timeout_s": 300}));
    assert_eq!((&answer["exit_code"], &answer["stdout_truncated"]), (&json!(0), &json!(true)));
    let end = answer["stdout"].as_str().unwrap();
    assert!(end.trim_end().ends_with("line 60000") && end.len() <= 256 * 1024);
    let (mut from, mut whole) = (0, String::new());
    loop {
        let part = test.tool("job", json!({"id": answer["job"], "stdout_from": from}));
        whole.push_str(part["stdout"].as_str().unwrap());
        from = part["stdout_next"].as_u64().unwrap();
        if part["stdout_more"] == false {
            break;
        }
    }
    assert_eq!(whole.lines().count(), 60000);
    assert_eq!(whole.lines().next().unwrap().trim(), "line 00001");
}

// ------------------------------------------------------------------------------------------------ files

#[test]
fn files_go_there_and_back() {
    let test = Test::new();
    let path = test.home.join("files").join("new folder").join("note é.txt");
    let path = path.to_str().unwrap();
    let written = test.tool("push", json!({"path": path, "text": "first"}));
    assert_eq!(written["bytes"], 5);
    test.tool("push", json!({"path": path, "text": "héllo"}));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "héllo");
    let read = test.tool("pull", json!({"path": path}));
    assert_eq!((&read["content_base64"], &read["size"], &read["eof"]), (&json!("aMOpbGxv"), &json!(6), &json!(true)));
    // Bytes that aren't text.
    test.tool("push", json!({"path": path, "content_base64": "AP/+AA=="}));
    assert_eq!(std::fs::read(path).unwrap(), [0x00, 0xff, 0xfe, 0x00]);
    let part = test.tool("pull", json!({"path": path, "offset": 1, "max_bytes": 2}));
    assert_eq!((&part["content_base64"], &part["next"], &part["eof"]), (&json!("//4="), &json!(3), &json!(false)));
    let nothing_left = std::fs::read_dir(test.home.join("files").join("new folder")).unwrap().count();
    assert_eq!(nothing_left, 1, "no temporary file stays behind");
}

#[test]
fn file_mistakes_are_refused_with_a_reason() {
    let test = Test::new();
    let folder = test.home.to_str().unwrap();
    assert_eq!(test.refused("pull", json!({"path": test.home.join("missing.txt").to_str().unwrap()})), (404, "not_found".into()));
    assert_eq!(test.refused("pull", json!({"path": folder})), (400, "bad_input".into()));
    assert_eq!(test.refused("push", json!({"path": folder, "text": "x"})), (400, "bad_input".into()));
    assert_eq!(test.refused("push", json!({"path": test.home.join("a.txt").to_str().unwrap()})), (400, "bad_input".into()));
    assert_eq!(test.refused("push", json!({"path": test.home.join("a.txt").to_str().unwrap(), "content_base64": "!!"})), (400, "bad_input".into()));
    assert_eq!(test.refused("push", json!({"path": "", "text": "x"})), (400, "bad_input".into()));
}

#[test]
fn a_path_starting_with_a_tilde_is_in_the_users_home() {
    let test = Test::new();
    let name = format!("mc-agent-tilde-test-{}.txt", std::process::id());
    let written = test.tool("push", json!({"path": format!("~/{name}"), "text": "x"}));
    let real = PathBuf::from(written["path"].as_str().unwrap());
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap();
    assert_eq!(real, home.join(&name));
    assert_eq!(test.tool("pull", json!({"path": format!("~/{name}")}))["size"], 1);
    assert_eq!(test.refused("pull", json!({"path": name})), (404, "not_found".into()), "a relative path isn't in the home");
    std::fs::remove_file(real).unwrap();
}

/// The folder a session's commands run in, as the command itself sees it.
fn folder_seen_by(agent: &mc_agent::Agent, session: &str) -> PathBuf {
    let command = if cfg!(windows) { "cd" } else { "pwd -P" };
    let ran = agent.call_in("test", Some(session), "exec", &json!({"command": command})).unwrap();
    PathBuf::from(ran["stdout"].as_str().unwrap().trim())
}

#[test]
fn each_session_works_in_its_own_folder_which_goes_when_it_ends() {
    let test = Test::new();
    let agent = &test.running.as_ref().unwrap().agent;
    let written = agent.call_in("test", Some("one"), "push", &json!({"path": "helper.txt", "text": "left behind"})).unwrap();
    let helper = PathBuf::from(written["path"].as_str().unwrap());
    let folder = helper.parent().unwrap().to_path_buf();
    assert_eq!(folder, test.home.join("work").join("one"), "a relative path is in the session's folder");
    let seen = folder_seen_by(agent, "one");
    assert_eq!(seen.canonicalize().unwrap(), folder.canonicalize().unwrap(), "commands run there");
    assert!(seen.canonicalize().unwrap().starts_with(test.home.join("work").canonicalize().unwrap()));
    let status = agent.call_in("test", Some("one"), "status", &json!({})).unwrap();
    assert_eq!(PathBuf::from(status["working_folder"].as_str().unwrap()), folder);
    assert_ne!(folder_seen_by(agent, "two").canonicalize().unwrap(), folder.canonicalize().unwrap(), "another session has another");
    // Calls that name no session share one folder, also in the agent's own.
    let shared = PathBuf::from(test.tool("status", json!({}))["working_folder"].as_str().unwrap());
    assert_eq!(shared, test.home.join("work").join("shared"));
    assert_eq!(agent.call_in("test", Some("../out"), "status", &json!({})).unwrap_err().code, "bad_input");

    // A job still running in the folder keeps it; once it ends, the folder goes.
    let job = agent.call_in("test", Some("one"), "exec", &json!({"command": long_command(), "detach": true})).unwrap();
    agent.end_session("one");
    assert!(helper.is_file(), "kept while a job runs in it");
    test.tool("kill", json!({"id": job["job"]}));
    agent.sweep();
    assert!(!folder.exists(), "removed once nothing runs there");
    assert!(test.home.join("work").join("two").exists(), "the other session's stays");
}

#[test]
fn what_earlier_runs_left_and_old_job_output_are_swept() {
    let test = Test::new();
    let agent = &test.running.as_ref().unwrap().agent;
    let ended = test.tool("exec", json!({"command": "echo done"}));
    let left = test.home.join("work").join("from-an-earlier-run");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::write(left.join("junk.ps1"), "x").unwrap();
    let old_job = test.home.join("jobs").join("0");
    std::fs::create_dir_all(&old_job).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let running = test.tool("exec", json!({"command": long_command(), "detach": true}));
    // Young enough: nothing goes.
    assert_eq!(agent.sweep(), (0, 0));
    let (folders, jobs) = agent.sweep_older_than(Duration::from_millis(10), Duration::from_millis(10));
    assert!(folders >= 1 && !left.exists(), "a folder no session uses any more goes");
    assert!(jobs >= 2 && !old_job.exists(), "old job output goes: {jobs}");
    assert_eq!(test.refused("job", json!({"id": ended["job"]})), (404, "not_found".into()));
    assert_eq!(test.tool("job", json!({"id": running["job"]}))["state"], "running", "a running job is kept");
    test.tool("kill", json!({"id": running["job"]}));
}

#[test]
fn big_files_go_as_raw_bytes() {
    let test = Test::new();
    let bytes: Vec<u8> = (0..3_000_000u32).map(|index| (index % 251) as u8).collect();
    let path = test.home.join("big file.bin");
    let encoded: String = path.to_str().unwrap().bytes().map(|byte| format!("%{byte:02X}")).collect();
    let url = test.url(&format!("/v1/files?path={encoded}"));
    let (status, answer) = Test::finish(ureq::put(&url).set("Authorization", &format!("Bearer {}", test.token)).send_bytes(&bytes));
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&answer));
    assert_eq!(serde_json::from_slice::<Value>(&answer).unwrap()["bytes"], 3_000_000);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let (status, back) = Test::finish(ureq::get(&url).set("Authorization", &format!("Bearer {}", test.token)).call());
    assert_eq!(status, 200);
    assert!(back == bytes);
    let (status, _) = Test::finish(ureq::get(&test.url("/v1/files")).set("Authorization", &format!("Bearer {}", test.token)).call());
    assert_eq!(status, 400);
    let (status, _) = Test::finish(ureq::get(&format!("{url}.missing")).set("Authorization", &format!("Bearer {}", test.token)).call());
    assert_eq!(status, 404);
    let (status, _) = Test::finish(ureq::get(&url).call());
    assert_eq!(status, 401);
}

// ------------------------------------------------------------------------------------------------ MCP

#[test]
fn mcp_over_http_lists_and_calls_the_tools() {
    let test = Test::new();
    let (status, hello) = test.mcp(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}));
    assert_eq!(status, 200);
    assert_eq!(hello["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(hello["result"]["serverInfo"]["name"], "master-control");
    assert!(hello["result"]["capabilities"]["tools"].is_object());
    let (_, newer) = test.mcp(json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocolVersion": "2099-01-01"}}));
    assert_eq!(newer["result"]["protocolVersion"], "2025-06-18");
    let (status, bytes) = Test::finish(
        ureq::post(&test.url("/mcp"))
            .set("Authorization", &format!("Bearer {}", test.token))
            .send_string(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()),
    );
    assert_eq!((status, bytes.len()), (202, 0));

    let (_, listed) = test.mcp(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect::<Vec<_>>(), ["status", "exec", "job", "jobs", "kill", "push", "pull", "screenshot", "windows", "focus", "arrange", "apps", "launch", "desktops", "click", "double_click", "right_click", "move", "drag", "scroll", "type_text", "key", "clipboard_get", "clipboard_set", "tree", "find", "wait_for", "press", "set_value", "toggle", "expand", "select", "observe_start", "observe_stop", "events", "summary", "logs", "sessions", "take_over", "batch"]);
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["description"].as_str().unwrap().len() > 20);
    }

    let (_, called) = test.mcp(json!({"jsonrpc": "2.0", "id": "a", "method": "tools/call",
        "params": {"name": "exec", "arguments": {"command": "echo from mcp"}}}));
    assert_eq!(called["id"], "a");
    assert_eq!(called["result"]["isError"], false);
    assert_eq!(called["result"]["structuredContent"]["stdout"].as_str().unwrap().trim(), "from mcp");
    let text: Value = serde_json::from_str(called["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, called["result"]["structuredContent"]);

    let (_, failed) = test.mcp(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "job", "arguments": {"id": 4242}}}));
    assert_eq!(failed["result"]["isError"], true);
    assert!(failed["result"]["content"][0]["text"].as_str().unwrap().contains("4242"));
    let (_, unknown) = test.mcp(json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "nope"}}));
    assert_eq!(unknown["error"]["code"], -32602);
    let (_, unsupported) = test.mcp(json!({"jsonrpc": "2.0", "id": 7, "method": "resources/list"}));
    assert_eq!(unsupported["error"]["code"], -32601);
    let (_, ping) = test.mcp(json!({"jsonrpc": "2.0", "id": 8, "method": "ping"}));
    assert_eq!(ping["result"], json!({}));
    let (_, batch) = test.mcp(json!([{"jsonrpc": "2.0", "id": 9, "method": "ping"}, {"jsonrpc": "2.0", "method": "notifications/initialized"}]));
    assert_eq!(batch.as_array().unwrap().len(), 1);
    let (status, garbage) = Test::finish(ureq::post(&test.url("/mcp")).set("Authorization", &format!("Bearer {}", test.token)).send_string("{"));
    assert_eq!((status, serde_json::from_slice::<Value>(&garbage).unwrap()["error"]["code"].as_i64()), (200, Some(-32700)));
    let (status, _) = Test::finish(ureq::post(&test.url("/mcp")).send_string("{}"));
    assert_eq!(status, 401);
}

#[test]
fn mcp_over_stdin_and_stdout_through_the_program_itself() {
    let home = std::env::temp_dir().join(format!("mc-agent-test-{}-stdio", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mc-agent"))
        .args(["mcp", "--home", home.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for message in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "exec", "arguments": {"command": "echo over stdio"}}}),
    ] {
        writeln!(input, "{message}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let answers: Vec<Value> = String::from_utf8(output.stdout).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(answers.len(), 2, "the notification gets no answer");
    assert_eq!(answers[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(answers[1]["result"]["structuredContent"]["stdout"].as_str().unwrap().trim(), "over stdio");
    std::fs::remove_dir_all(&home).unwrap();
}

// ------------------------------------------------------------------------------------------------ audit, program

#[test]
fn every_request_is_in_the_audit_log_without_its_contents() {
    let mut test = Test::new();
    test.tool("push", json!({"path": test.home.join("secret.txt").to_str().unwrap(), "text": "the-secret-content"}));
    test.tool("exec", json!({"command": "echo audit-me", "env": {"MC_KEY": "the-secret-value"}, "stdin": "the-secret-input"}));
    test.refused("job", json!({"id": 77}));
    test.mcp(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "jobs", "arguments": {}}}));
    test.stop();
    let log = std::fs::read_to_string(test.home.join("audit.log")).unwrap();
    let lines: Vec<Value> = log.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let seen: Vec<(&str, &str, bool)> =
        lines.iter().map(|line| (line["via"].as_str().unwrap(), line["tool"].as_str().unwrap(), line["ok"].as_bool().unwrap())).collect();
    assert_eq!(seen, [("http", "push", true), ("http", "exec", true), ("http", "job", false), ("mcp", "jobs", true)]);
    assert_eq!(lines[1]["args"]["command"], "echo audit-me");
    assert_eq!(lines[1]["args"]["env"]["names"], json!(["MC_KEY"]));
    assert_eq!(lines[2]["error"], "not_found");
    assert!(lines.iter().all(|line| line["time_ms"].as_u64().unwrap() > 1_700_000_000_000));
    assert!(!log.contains("the-secret"), "{log}");
}

#[test]
fn the_program_prints_its_version_and_token_and_explains_mistakes() {
    let program = env!("CARGO_BIN_EXE_mc-agent");
    let run = |arguments: &[&str]| std::process::Command::new(program).args(arguments).output().unwrap();
    let version = run(&["--version"]);
    assert_eq!(String::from_utf8_lossy(&version.stdout).trim(), format!("mc-agent {}", mc_core::VERSION));
    let home = std::env::temp_dir().join(format!("mc-agent-test-{}-cli", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let token = run(&["token", "--home", home.to_str().unwrap()]);
    let token = String::from_utf8_lossy(&token.stdout).trim().to_string();
    assert_eq!(token.len(), 64);
    assert_eq!(std::fs::read_to_string(home.join("token")).unwrap(), token);
    std::fs::remove_dir_all(&home).unwrap();
    assert!(String::from_utf8_lossy(&run(&[]).stdout).contains("mc-agent serve"));
    for wrong in [&["frobnicate"][..], &["serve", "--port", "abc"], &["serve", "--port"], &["serve", "--nope", "1"]] {
        let output = run(wrong);
        assert_eq!(output.status.code(), Some(2), "{wrong:?}");
        assert!(String::from_utf8_lossy(&output.stderr).starts_with("mc-agent: "));
    }
}

#[test]
fn the_served_program_answers_on_the_port_it_was_given() {
    let home = std::env::temp_dir().join(format!("mc-agent-test-{}-serve", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mc-agent"))
        .args(["serve", "--port", &port.to_string(), "--home", home.to_str().unwrap()])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut token = String::new();
    wait_until("the program is listening", || {
        token = std::fs::read_to_string(home.join("token")).unwrap_or_default();
        !token.is_empty() && home.join("agent.json").exists() && TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let (status, bytes) =
        Test::finish(ureq::get(&format!("http://127.0.0.1:{port}/v1/status")).set("Authorization", &format!("Bearer {token}")).call());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(status, 200);
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["pid"], child.id());
    std::fs::remove_dir_all(&home).unwrap();
}

// ------------------------------------------------------------------------------------------------ desktop

/// On a Mac this test must never click, type or take a picture of the user's screen. So it lists windows, which
/// touches nothing, and calls a picture or input tool only when macOS has NOT granted the permission it needs, to
/// check that the refusal says what to grant.
#[cfg(target_os = "macos")]
#[test]
fn on_a_mac_the_desktop_tools_list_windows_and_say_which_permission_they_wait_for() {
    let test = Test::new();
    let status = test.tool("status", json!({}));
    let needed: Vec<&str> = status["permissions_needed"].as_array().unwrap().iter().map(|name| name.as_str().unwrap()).collect();
    assert_eq!(status["capabilities"]["screen"], !needed.contains(&"Screen Recording"));
    assert_eq!(status["capabilities"]["input"], !needed.contains(&"Accessibility"));

    let windows = test.tool("windows", json!({}));
    assert!(windows["screen"]["width"].as_u64().unwrap() >= 800 && windows["screen"]["height"].as_u64().unwrap() >= 500, "{}", windows["screen"]);
    let listed = windows["windows"].as_array().unwrap();
    assert!(listed.iter().all(|window| window["id"].as_u64().unwrap() > 0 && window["width"].as_u64().unwrap() >= 2 && !window["app"].as_str().unwrap().is_empty()), "{listed:?}");
    assert!(listed.iter().filter(|window| window["focused"] == true).count() <= 1);

    let refusal = |tool: &str, args: Value, permission: &str| {
        let (status, answer) = test.post(&format!("/v1/{tool}"), &args);
        assert_eq!((status, answer["error"]["code"].as_str()), (501, Some("not_supported")), "{tool}: {answer}");
        assert!(answer["error"]["message"].as_str().unwrap().contains(permission), "{tool}: {answer}");
    };
    if needed.contains(&"Screen Recording") {
        refusal("screenshot", json!({}), "Screen Recording");
    }
    if needed.contains(&"Accessibility") {
        refusal("move", json!({"x": 10, "y": 10}), "Accessibility");
        refusal("scroll", json!({"x": 10, "y": 10, "down": 1}), "Accessibility");
    }
    if needed.contains(&"Accessibility") {
        if let Some(window) = listed.first() {
            refusal("tree", json!({"window": window["id"]}), "Accessibility");
        }
    }
    // Mistakes are refused before anything would be sent, permission or not.
    assert_eq!(test.refused("click", json!({"x": 10})), (400, "bad_input".into()));
    assert_eq!(test.refused("type_text", json!({"text": "x"})), (400, "bad_input".into()));
    assert_eq!(test.refused("key", json!({"keys": "ctrl+nonsense", "window": 1})), (400, "bad_input".into()));
}

/// With admin rights where the machine gives them, and a plain refusal where it doesn't.
#[test]
fn exec_runs_with_admin_rights_when_asked_and_able() {
    let test = Test::new();
    let (status, answer) = test.post("/v1/exec", &json!({"command": if cfg!(windows) { "net session >nul && echo admin" } else { "id -u" }, "elevated": true}));
    if status == 200 {
        assert_eq!(answer["stdout"].as_str().unwrap().trim(), if cfg!(windows) { "admin" } else { "0" }, "{answer}");
        let argv = test.tool("exec", json!({"argv": if cfg!(windows) { json!(["whoami", "/groups"]) } else { json!(["id", "-u"]) }, "elevated": true}));
        assert_eq!(argv["exit_code"], 0);
    } else {
        assert_eq!((status, answer["error"]["code"].as_str()), (501, Some("not_supported")), "{answer}");
        assert!(answer["error"]["message"].as_str().unwrap().contains("admin rights"));
    }
    // Without the flag, the command runs as the user the agent runs as.
    if cfg!(unix) && test.tool("status", json!({}))["elevated"] == false {
        assert_ne!(test.tool("exec", json!({"command": "id -u"}))["stdout"].as_str().unwrap().trim(), "0");
    }
}

/// The apps in the Applications folders are listed by name; one the test makes is started by it. The app is a
/// script with no window, which only writes a file, so it runs only when asked for (MC_MAC_GUI_TESTS=1).
#[cfg(target_os = "macos")]
#[test]
fn on_a_mac_apps_are_listed_and_launched_by_name() {
    let test = Test::new();
    let listed = test.tool("apps", json!({"name": "calculator"}));
    assert!(listed["apps"].as_array().unwrap().iter().any(|app| app["id"] == "/System/Applications/Calculator.app"), "{listed}");
    assert_eq!(test.refused("launch", json!({"name": "no such app anywhere at all"})), (404, "not_found".into()));
    if std::env::var_os("MC_MAC_GUI_TESTS").is_none() {
        eprintln!("skipped launching: set MC_MAC_GUI_TESTS=1 to start a test app on this Mac");
        return;
    }
    let folder = std::env::temp_dir().join(format!("mc-mac-apps-{}", std::process::id()));
    let name = format!("MC Test App {}", std::process::id());
    let bundle = folder.join(format!("{name}.app/Contents"));
    let marker = folder.join("launched");
    std::fs::create_dir_all(bundle.join("MacOS")).unwrap();
    std::fs::write(bundle.join("Info.plist"), format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>run</string><key>CFBundleIdentifier</key><string>io.github.dunkelherzmann.mctest{}</string><key>LSUIElement</key><true/></dict></plist>\n", std::process::id())).unwrap();
    let script = bundle.join("MacOS/run");
    std::fs::write(&script, format!("#!/bin/sh\necho launched > '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::env::set_var("MC_APPS_FOLDERS", &folder);
    let launched = test.tool("launch", json!({"name": name, "wait_s": 0}));
    std::env::remove_var("MC_APPS_FOLDERS");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let ran = marker.exists();
    let _ = std::fs::remove_dir_all(&folder);
    assert_eq!(launched["app"]["name"], json!(name), "{launched}");
    assert!(ran, "the app ran: {launched}");
}

/// The Spaces are listed with the one shown; nothing is switched, since that would change the user's screen.
#[cfg(target_os = "macos")]
#[test]
fn on_a_mac_the_spaces_are_listed() {
    let test = Test::new();
    let listed = test.tool("desktops", json!({}));
    let desktops = listed["desktops"].as_array().unwrap();
    assert!(!desktops.is_empty() && desktops.iter().any(|desktop| desktop["current"] == true), "{listed}");
    assert_eq!(listed["current"], desktops.iter().position(|desktop| desktop["current"] == true).map(|index| index + 1).unwrap(), "{listed}");
    assert_eq!(test.refused("desktops", json!({"action": "switch", "number": desktops.len() + 1})), (400, "bad_input".into()));
    assert_eq!(test.refused("desktops", json!({"action": "switch"})), (400, "bad_input".into()));
    assert_eq!(test.refused("desktops", json!({"action": "create"})), (501, "not_supported".into()));
    assert_eq!(test.refused("desktops", json!({"action": "rename"})), (400, "bad_input".into()));
}

/// The Mac window tests take turns: each compares the window list before and after, which the other's window moves.
#[cfg(target_os = "macos")]
fn mac_window_turn() -> std::sync::MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Moves, sizes, minimizes, restores and maximizes a real AppKit window of the test's own, through accessibility. It
/// opens a window on the Mac's desktop without taking the keyboard, so it runs only when asked for:
/// MC_MAC_GUI_TESTS=1, with the Accessibility permission granted to whatever runs the tests.
#[cfg(target_os = "macos")]
#[test]
fn on_a_mac_a_window_is_moved_sized_minimized_and_restored() {
    if std::env::var_os("MC_MAC_GUI_TESTS").is_none() {
        eprintln!("skipped: set MC_MAC_GUI_TESTS=1 to open a test window on this Mac's desktop");
        return;
    }
    let _turn = mac_window_turn();
    let test = Test::new();
    assert_eq!(test.tool("status", json!({}))["capabilities"]["input"], true, "grant Accessibility to the program running the tests");
    let title = format!("mc test: arrange {}", std::process::id());
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mac_window.swift");
    let mut window = std::process::Command::new("swift").args([fixture, &title]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).spawn().expect("swift runs the test window");
    let arrange = || {
        let found = test.post("/v1/wait_for", &json!({"window": title, "role": "button", "name": "Save", "timeout_s": 60}));
        let id = found.1["window"]["id"].clone();
        let mut said = serde_json::Map::new();
        said.insert("placed".into(), test.post("/v1/arrange", &json!({"window": id, "x": 200, "y": 150, "width": 500, "height": 320})).1);
        said.insert("moved only".into(), test.post("/v1/arrange", &json!({"window": id, "x": 260})).1);
        said.insert("minimized".into(), test.post("/v1/arrange", &json!({"window": id, "state": "minimized"})).1);
        said.insert("listed while minimized".into(), json!(test.tool("windows", json!({}))["windows"].as_array().unwrap().iter().any(|window| window["id"] == id)));
        said.insert("restored".into(), test.post("/v1/arrange", &json!({"window": id, "state": "normal"})).1);
        said.insert("maximized".into(), test.post("/v1/arrange", &json!({"window": id, "state": "maximized"})).1);
        said.insert("screen".into(), test.tool("windows", json!({}))["screen"].clone());
        said.insert("back".into(), test.post("/v1/arrange", &json!({"window": id, "x": 200, "y": 150, "width": 500, "height": 320})).1);
        // Typing helpers in the background: emptied, then text put at the start and at the end.
        let field = test.tool("find", json!({"window": id, "role": "text"}))["elements"][0]["id"].clone();
        test.post("/v1/type_text", &json!({"element": field, "text": "middle", "clear": true, "background": true}));
        test.post("/v1/type_text", &json!({"window": id, "text": "<", "caret": "start", "background": true}));
        test.post("/v1/type_text", &json!({"window": id, "text": ">", "caret": "end", "background": true}));
        said.insert("typed".into(), test.tool("find", json!({"window": id, "role": "text"}))["elements"][0]["value"].clone());
        said.insert("compact".into(), test.tool("tree", json!({"window": id, "interactive": true}))["elements"].clone());
        said.insert("in front".into(), json!(test.tool("windows", json!({}))["windows"].as_array().unwrap().iter().any(|window| window["id"] == id && window["focused"] == true)));
        Value::Object(said)
    };
    let outcome = arrange();
    let _ = window.kill();
    let _ = window.wait();
    let frame = |answer: &Value| (answer["window"]["x"].clone(), answer["window"]["y"].clone(), answer["window"]["width"].clone(), answer["window"]["height"].clone());
    assert_eq!(frame(&outcome["placed"]), (json!(200), json!(150), json!(500), json!(320)), "{outcome}");
    assert!(outcome["placed"]["asked"].is_null(), "{outcome}");
    assert_eq!(frame(&outcome["moved only"]), (json!(260), json!(150), json!(500), json!(320)), "what isn't given stays: {outcome}");
    assert_eq!(outcome["minimized"]["window"]["minimized"], true, "{outcome}");
    assert_eq!(outcome["listed while minimized"], false, "macOS lists only what is on screen; the id still reaches it");
    assert_eq!((&outcome["restored"]["window"]["minimized"], &outcome["restored"]["window"]["x"]), (&json!(false), &json!(260)), "{outcome}");
    let (maximized, screen) = (&outcome["maximized"]["window"], &outcome["screen"]);
    assert!(maximized["width"].as_i64().unwrap() >= screen["width"].as_i64().unwrap() - 200 && maximized["height"].as_i64().unwrap() >= screen["height"].as_i64().unwrap() - 200, "fills the work area: {outcome}");
    assert_eq!(frame(&outcome["back"]), (json!(200), json!(150), json!(500), json!(320)), "a maximized window placed is restored: {outcome}");
    assert_eq!(outcome["typed"], "<middle>", "clear and caret, in the background: {outcome}");
    let roles: Vec<&str> = outcome["compact"].as_array().unwrap().iter().map(|element| element["role"].as_str().unwrap()).collect();
    assert!(roles.contains(&"button") && roles.contains(&"text") && roles.contains(&"check_box") && !roles.contains(&"label") && !roles.contains(&"window"), "the compact view: {roles:?}");
    assert_eq!(outcome["in front"], false, "arranging doesn't bring the window forward");
}

/// Reads the contents of a real AppKit window. It opens a small window on the Mac's desktop (without taking the
/// keyboard) and sends no input, so it runs only when asked for: MC_MAC_GUI_TESTS=1, with the Accessibility
/// permission granted to whatever runs the tests.
#[cfg(target_os = "macos")]
#[test]
fn on_a_mac_the_tree_names_what_is_in_a_window() {
    if std::env::var_os("MC_MAC_GUI_TESTS").is_none() {
        eprintln!("skipped: set MC_MAC_GUI_TESTS=1 to open a test window on this Mac's desktop");
        return;
    }
    let _turn = mac_window_turn();
    let test = Test::new();
    assert_eq!(test.tool("status", json!({}))["capabilities"]["input"], true, "grant Accessibility to the program running the tests");
    let title = format!("mc test: tree {}", std::process::id());
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mac_window.swift");
    let mut window = std::process::Command::new("swift").args([fixture, &title]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).spawn().expect("swift runs the test window");
    let found = test.post("/v1/wait_for", &json!({"window": title, "role": "button", "name": "Save", "timeout_s": 60}));
    // A window that is still sliding into place (the user changing desktops) is told in one place and its contents in
    // another: read it once it has settled.
    let mut tree = test.post("/v1/tree", &json!({"window": title}));
    for _ in 0..20 {
        let inside = tree.1["elements"].as_array().is_some_and(|elements| {
            let frame = &tree.1["window"];
            elements.iter().skip(1).all(|element| element["x"].is_null() || (element["x"].as_i64() >= frame["x"].as_i64() && element["x"].as_i64().unwrap() < frame["x"].as_i64().unwrap() + frame["width"].as_i64().unwrap()))
        });
        if inside {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        tree = test.post("/v1/tree", &json!({"window": title}));
    }
    let limited = test.post("/v1/tree", &json!({"window": title, "max_elements": 2}));
    let checks = test.post("/v1/find", &json!({"window": title, "role": "check_box"}));
    let by_value = test.post("/v1/find", &json!({"window": title, "value": "FIRST"}));
    // Acting on the elements themselves: nothing is sent through the pointer or the keyboard.
    let acted = (|| {
        let elements = tree.1["elements"].as_array()?.clone();
        let id = |role: &str, name: &str| elements.iter().find(|element| element["role"] == role && element["name"].as_str().unwrap().contains(name)).map(|element| element["id"].clone());
        let pointer_before = test.tool("windows", json!({}))["windows"].clone();
        let mut said = serde_json::Map::new();
        said.insert("press".into(), test.post("/v1/press", &json!({"element": id("button", "Save it")?})).1);
        said.insert("press again".into(), test.post("/v1/press", &json!({"element": id("button", "Save it")?})).1);
        said.insert("label".into(), test.post("/v1/find", &json!({"window": title, "role": "label"})).1);
        said.insert("set".into(), test.post("/v1/set_value", &json!({"element": id("text", "")?, "value": "second text"})).1);
        said.insert("untick".into(), test.post("/v1/toggle", &json!({"element": id("check_box", "Enable")?, "on": false})).1);
        said.insert("untick again".into(), test.post("/v1/toggle", &json!({"element": id("check_box", "Enable")?, "on": false})).1);
        said.insert("flip".into(), test.post("/v1/toggle", &json!({"element": id("check_box", "Enable")?})).1);
        said.insert("select".into(), test.post("/v1/select", &json!({"element": id("radio_button", "Two")?})).1);
        said.insert("radios".into(), test.post("/v1/find", &json!({"window": title, "role": "radio_button"})).1);
        let label_now = said["label"]["elements"][0]["id"].clone();
        said.insert("label can't".into(), json!(test.refused("press", json!({"element": label_now}))));
        said.insert("button can't".into(), json!(test.refused("expand", json!({"element": id("button", "Save it")?}))));
        said.insert("stale".into(), json!(test.refused("press", json!({"element": id("label", "A label")?}))));
        said.insert("focus unchanged".into(), json!(pointer_before == test.tool("windows", json!({}))["windows"]));
        // Input in the background: handed to the window's app only. The real pointer and keyboard aren't used.
        let now = test.post("/v1/tree", &json!({"window": title})).1;
        let centre = |role: &str, name: &str| {
            let element = now["elements"].as_array()?.iter().find(|element| element["role"] == role && element["name"].as_str().unwrap().contains(name))?;
            Some((element["x"].as_i64()? + element["width"].as_i64()? / 2, element["y"].as_i64()? + element["height"].as_i64()? / 2))
        };
        let (field_x, field_y) = centre("text", "")?;
        let (button_x, button_y) = centre("button", "Save it")?;
        said.insert("quiet field".into(), test.post("/v1/click", &json!({"x": field_x, "y": field_y, "window": title, "background": true})).1);
        said.insert("quiet typing".into(), test.post("/v1/type_text", &json!({"text": "typed quietly é", "window": title, "background": true})).1);
        said.insert("field after typing".into(), test.post("/v1/find", &json!({"window": title, "role": "text"})).1);
        said.insert("quiet select all".into(), test.post("/v1/key", &json!({"keys": "cmd+a", "window": title, "background": true})).1);
        said.insert("quiet key".into(), test.post("/v1/key", &json!({"keys": "x", "window": title, "background": true, "times": 3})).1);
        said.insert("field after keys".into(), test.post("/v1/find", &json!({"window": title, "role": "text"})).1);
        // Typing into an element by its id: it is given the keyboard first, wherever the keyboard was, and the
        // answer says what had it.
        let field_id = said["field after keys"]["elements"][0]["id"].clone();
        said.insert("into element".into(), test.post("/v1/type_text", &json!({"element": field_id, "text": "!", "background": true})).1);
        said.insert("field after element".into(), test.post("/v1/find", &json!({"window": title, "role": "text"})).1);
        let button_id = id("button", "Save it")?;
        said.insert("into a button".into(), test.post("/v1/type_text", &json!({"element": button_id, "text": "x", "background": true})).1);
        said.insert("field after button".into(), test.post("/v1/find", &json!({"window": title, "role": "text"})).1);
        said.insert("quiet press".into(), test.post("/v1/click", &json!({"x": button_x, "y": button_y, "window": title, "background": true})).1);
        said.insert("label after quiet press".into(), test.post("/v1/find", &json!({"window": title, "role": "label"})).1);
        said.insert("quiet right click".into(), json!(test.refused("right_click", json!({"x": button_x, "y": button_y, "window": title, "background": true}))));
        said.insert("quiet scroll".into(), json!(test.refused("scroll", json!({"x": button_x, "y": button_y, "down": 1, "window": title, "background": true}))));
        said.insert("quiet without window".into(), json!(test.refused("click", json!({"x": button_x, "y": button_y, "background": true}))));
        said.insert("quiet modifier alone".into(), json!(test.refused("key", json!({"keys": "shift", "window": title, "background": true}))));
        // Find, press and look again in one call: the press takes the id the find in the same call answered.
        said.insert(
            "batch".into(),
            test.post(
                "/v1/batch",
                &json!({"steps": [
                    {"tool": "find", "args": {"window": title, "role": "button", "name": "Save it"}},
                    {"tool": "press", "args": {"element": {"from_step": 1, "path": "elements.0.id"}}},
                    {"tool": "wait_for", "args": {"window": title, "role": "label", "name": "pressed 4", "timeout_s": 5}},
                ]}),
            )
            .1,
        );
        let ours_in_front = test.tool("windows", json!({}))["windows"].as_array().unwrap().iter().any(|window| window["title"] == title.as_str() && window["focused"] == true);
        said.insert("stayed in the background".into(), json!(!ours_in_front));
        Some(Value::Object(said))
    })();
    let _ = window.kill();
    let _ = window.wait();

    assert_eq!(found.0, 200, "{}", found.1);
    assert_eq!(found.1["elements"][0]["name"], "Save it");
    let (status, tree) = tree;
    assert_eq!(status, 200, "{tree}");
    let elements = tree["elements"].as_array().unwrap();
    let frame = &tree["window"];
    let one = |role: &str, name: &str| elements.iter().find(|element| element["role"] == role && element["name"].as_str().unwrap().contains(name)).unwrap_or_else(|| panic!("no {role} named {name} in {tree}"));
    assert_eq!((&elements[0]["role"], &elements[0]["depth"], &elements[0]["name"]), (&json!("window"), &json!(0), &json!(title)));
    let button = one("button", "Save it");
    // AppKit counts from the window's bottom-left; the tree gives desktop positions from the top-left. The button
    // was put 20 points in from the left and 28 below the title bar; AppKit reports it a point wider all round,
    // for its edge.
    let (left, top) = (button["x"].as_i64().unwrap() - frame["x"].as_i64().unwrap(), button["y"].as_i64().unwrap() - frame["y"].as_i64().unwrap());
    assert!((18..=22).contains(&left) && (50..=70).contains(&top), "{left},{top}: {button} in {frame}");
    assert!((118..=124).contains(&button["width"].as_i64().unwrap()) && (30..=36).contains(&button["height"].as_i64().unwrap()), "{button}");
    assert!(button["enabled"].is_null(), "only said when unusual: {button}");
    assert_eq!(one("text", "")["value"], "first text");
    one("label", "A label");
    assert_eq!(one("check_box", "Enable thing")["value"], "1", "ticked");
    assert_eq!((limited.1["elements"].as_array().unwrap().len(), &limited.1["truncated"]), (2, &json!(true)));
    assert_eq!(checks.1["elements"].as_array().unwrap().len(), 1);
    assert_eq!(by_value.1["elements"][0]["role"], "text");
    assert_eq!(test.refused("wait_for", json!({"window": title, "timeout_s": 1})), (408, "timed_out".into()), "the window is gone");

    let acted = acted.expect("the tree had every element");
    assert_eq!((&acted["press"]["role"], &acted["press"]["name"]), (&json!("button"), &json!("Save it")), "{acted}");
    assert_eq!(acted["label"]["elements"][0]["name"], "pressed 2", "the button's own action ran, twice: {acted}");
    assert_eq!(acted["set"]["value"], "second text", "{acted}");
    assert_eq!((&acted["untick"]["on"], &acted["untick again"]["on"], &acted["flip"]["on"]), (&json!(false), &json!(false), &json!(true)), "{acted}");
    let radios: Vec<(&str, &str)> = acted["radios"]["elements"].as_array().unwrap().iter().map(|radio| (radio["name"].as_str().unwrap(), radio["value"].as_str().unwrap())).collect();
    assert_eq!(radios, [("One", "0"), ("Two", "1")], "{acted}");
    assert_eq!(acted["button can't"], json!([501, "not_supported"]), "{acted}");
    assert_eq!(acted["label can't"], json!([501, "not_supported"]), "{acted}");
    // The label the first tree named "A label" now says something else: its old id is refused, not acted on.
    assert_eq!(acted["stale"], json!([409, "element_gone"]), "{acted}");
    assert_eq!(acted["focus unchanged"], true, "no window came forward or lost its place: {acted}");

    assert_eq!((&acted["quiet field"]["delivered"], &acted["quiet field"]["background"]), (&json!(true), &json!(true)), "{acted}");
    assert!(acted["quiet field"]["did"].as_str().unwrap().starts_with("gave the keyboard to the text"), "{acted}");
    assert_eq!(acted["field after typing"]["elements"][0]["value"], "second texttyped quietly é", "typed where the field's caret was, after what it held: {acted}");
    assert_eq!(acted["field after keys"]["elements"][0]["value"], "xxx", "select all, then a key three times: {acted}");
    assert_eq!(acted["quiet typing"]["into"]["role"], "text", "the answer says what had the keyboard: {acted}");
    assert_eq!((&acted["into element"]["delivered"], &acted["into element"]["into"]["role"]), (&json!(true), &json!("text")), "{acted}");
    assert_eq!(acted["field after element"]["elements"][0]["value"], "xxx!", "{acted}");
    // Sent is not landed: keys aimed at a button go to the button, and the answer says so rather than leaving the
    // client to think they went into the text field.
    assert_eq!(acted["into a button"]["into"], json!({"role": "button", "name": "Save it"}), "{acted}");
    assert_eq!(acted["field after button"]["elements"][0]["value"], "xxx!", "the field didn't get them: {acted}");
    assert_eq!(acted["quiet press"]["did"], "pressed the button \"Save it\"", "{acted}");
    assert_eq!(acted["label after quiet press"]["elements"][0]["name"], "pressed 3", "{acted}");
    // Tools that don't offer `background` refuse it as an argument they don't know, rather than act for real.
    assert_eq!(acted["quiet right click"], json!([400, "bad_input"]), "{acted}");
    assert_eq!(acted["quiet scroll"], json!([400, "bad_input"]), "{acted}");
    assert_eq!(acted["quiet without window"], json!([400, "bad_input"]), "{acted}");
    assert_eq!(acted["quiet modifier alone"], json!([400, "bad_input"]), "{acted}");
    assert_eq!((&acted["batch"]["ok"], &acted["batch"]["done"], &acted["batch"]["steps"][2]["result"]["elements"][0]["name"]), (&json!(true), &json!(3), &json!("pressed 4")), "{acted}");
    assert_eq!(acted["stayed in the background"], true, "the test window never came to the front: {acted}");
}

/// Several tools in one call, a later step using what an earlier one answered.
#[test]
fn a_batch_runs_its_steps_in_order_and_hands_answers_on() {
    let test = Test::new();
    let file = test.home.join("batch.txt");
    let path = file.to_str().unwrap();
    let answer = test.tool(
        "batch",
        json!({"steps": [
            {"tool": "push", "args": {"path": path, "text": "written by step one"}},
            {"tool": "exec", "args": {"argv": if cfg!(windows) { json!(["cmd", "/c", "echo", "second"]) } else { json!(["echo", "second"]) }}},
            {"tool": "pull", "args": {"path": {"from_step": 1, "path": "path"}}},
            {"tool": "status"},
        ]}),
    );
    assert_eq!((&answer["ok"], &answer["done"], &answer["refused"], &answer["not_run"]), (&json!(true), &json!(4), &json!(0), &json!(0)), "{answer}");
    let steps = answer["steps"].as_array().unwrap();
    assert_eq!(steps.iter().map(|step| (step["step"].as_u64().unwrap(), step["tool"].as_str().unwrap(), step["ok"].as_bool().unwrap())).collect::<Vec<_>>(), [(1, "push", true), (2, "exec", true), (3, "pull", true), (4, "status", true)]);
    assert_eq!(steps[1]["result"]["stdout"].as_str().unwrap().trim(), "second");
    // "written by step one", as pull hands contents over.
    assert_eq!(steps[2]["result"]["content_base64"], "d3JpdHRlbiBieSBzdGVwIG9uZQ==", "step 3 read the file step 1 said it wrote: {answer}");
    assert_eq!(steps[3]["result"]["name"], "master-control");

    // A refused step stops the rest, and says which; with keep_going the rest still run.
    let stopped = test.tool("batch", json!({"steps": [{"tool": "status"}, {"tool": "job", "args": {"id": 4242}}, {"tool": "status"}]}));
    assert_eq!((&stopped["ok"], &stopped["done"], &stopped["refused"], &stopped["not_run"]), (&json!(false), &json!(1), &json!(1), &json!(1)), "{stopped}");
    assert_eq!((&stopped["steps"][1]["ok"], &stopped["steps"][1]["error"]["code"]), (&json!(false), &json!("not_found")));
    let went_on = test.tool("batch", json!({"keep_going": true, "steps": [{"tool": "job", "args": {"id": 4242}}, {"tool": "status"}, {"tool": "pull", "args": {"path": {"from_step": 1, "path": "x"}}}]}));
    assert_eq!((&went_on["done"], &went_on["refused"], &went_on["not_run"]), (&json!(1), &json!(2), &json!(0)), "{went_on}");
    assert!(went_on["steps"][2]["error"]["message"].as_str().unwrap().contains("step 1 was refused"), "{went_on}");
    let missing = test.tool("batch", json!({"steps": [{"tool": "status"}, {"tool": "pull", "args": {"path": {"from_step": 1, "path": "no.such.thing"}}}]}));
    assert!(missing["steps"][1]["error"]["message"].as_str().unwrap().contains("nothing at no.such.thing"), "{missing}");

    // A list with a slip in it is refused whole: nothing of it runs.
    let untouched = test.home.join("never.txt");
    let slip = json!({"steps": [{"tool": "push", "args": {"path": untouched.to_str().unwrap(), "text": "x"}}, {"tool": "no_such_tool"}]});
    assert_eq!(test.refused("batch", slip), (400, "bad_input".into()));
    assert!(!untouched.exists());
    assert_eq!(test.refused("batch", json!({"steps": []})), (400, "bad_input".into()));
    assert_eq!(test.refused("batch", json!({})), (400, "bad_input".into()));
    assert_eq!(test.refused("batch", json!({"steps": [{"tool": "batch", "args": {"steps": [{"tool": "status"}]}}]})), (400, "bad_input".into()));
    assert_eq!(test.refused("batch", json!({"steps": [{"tool": "status", "when": "now"}]})), (400, "bad_input".into()));
    assert_eq!(test.refused("batch", json!({"steps": [{"tool": "status"}, {"tool": "pull", "args": {"path": {"from_step": 2, "path": "x"}}}]})), (200, String::new()), "a step that points at itself is that step's refusal, not the batch's");

    // Each step is in the agent's own record, without what was typed or written.
    let log = std::fs::read_to_string(test.home.join("audit.log")).unwrap();
    assert!(log.lines().any(|line| line.contains("\"via\":\"batch\"") && line.contains("\"tool\":\"push\"")), "{log}");
    assert!(!log.contains("written by step one"), "{log}");
}

/// The take-over setting is kept on the machine and the session is let in without a question when asking is off; a
/// status changes nothing. (No test shows the pop-up: it would ask whoever is at this machine.)
#[test]
fn take_over_is_set_and_reported_without_asking() {
    let test = Test::new();
    let first = test.tool("take_over", json!({"action": "status"}));
    assert_eq!(first["active_within_s"], 60, "{first}");
    assert_eq!((first["granted"].clone(), first["asked"].clone(), first["declined"].clone()), (json!(false), json!(false), json!(false)), "{first}");
    let set = test.tool("take_over", json!({"action": "status", "active_within_s": 0}));
    assert_eq!(set["active_within_s"], 0, "{set}");
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(test.home.join("take-over.json")).unwrap()).unwrap();
    assert_eq!(kept["active_within_s"], 0, "{kept}");
    let asked = test.tool("take_over", json!({"reason": "checking a test window"}));
    assert_eq!((asked["granted"].clone(), asked["asked"].clone(), asked["someone_active"].clone()), (json!(true), json!(false), json!(false)), "{asked}");
    assert_eq!(asked["reason"], "checking a test window");
    let released = test.tool("take_over", json!({"action": "release"}));
    assert_eq!(released["granted"], false, "{released}");
    assert_eq!(test.refused("take_over", json!({"action": "grab"})), (400, "bad_input".into()));
    assert_eq!(test.refused("take_over", json!({"active_within_s": 100000})), (400, "bad_input".into()));
}

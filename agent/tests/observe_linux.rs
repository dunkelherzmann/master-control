//! Observation on Linux: sessions that follow a small target program which does exactly what the test tells it to,
//! so every expected event is known.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

mod common;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::Test;
use serde_json::{json, Value};

const TARGET: &str = env!("CARGO_BIN_EXE_mc-test-target");

struct Session {
    test: Test,
    folder: PathBuf,
}

impl Session {
    fn new() -> Session {
        // The agent starts its tracer by running its own program; inside a test, that program is elsewhere.
        std::env::set_var("MC_AGENT_PROGRAM", env!("CARGO_BIN_EXE_mc-agent"));
        let test = Test::new();
        let folder = test.home.join("work");
        std::fs::create_dir_all(&folder).unwrap();
        // The real path: the temporary folder may be reached through a link, and events carry real paths.
        let folder = folder.canonicalize().unwrap();
        Session { test, folder }
    }

    fn path(&self, name: &str) -> String {
        self.folder.join(name).to_str().unwrap().to_string()
    }

    fn start(&self, actions: &[String], extra: Value) -> u64 {
        let mut args = json!({ "argv": std::iter::once(TARGET.to_string()).chain(actions.iter().cloned()).collect::<Vec<_>>() });
        for (name, value) in extra.as_object().unwrap() {
            args[name] = value.clone();
        }
        self.test.tool("observe_start", args)["session"].as_u64().unwrap()
    }

    /// Waits for the target to end by itself.
    fn finished(&self, session: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let now = self.test.tool("summary", json!({ "session": session }));
            if now["state"] != "running" {
                return now;
            }
            assert!(Instant::now() < deadline, "the target was still running: {now}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Runs the target to its end and returns the verdict.
    fn run(&self, actions: &[String], extra: Value) -> (u64, Value) {
        let session = self.start(actions, extra);
        self.finished(session);
        (session, self.test.tool("observe_stop", json!({ "session": session })))
    }

    fn events(&self, session: u64, filter: Value) -> Vec<Value> {
        let mut args = filter;
        args["session"] = json!(session);
        args["limit"] = json!(2000);
        self.test.tool("events", args)["events"].as_array().unwrap().clone()
    }
}

fn ops(events: &[Value]) -> Vec<&str> {
    events.iter().map(|event| event["op"].as_str().unwrap()).collect()
}

#[test]
fn file_activity_is_recorded_in_order_with_its_details() {
    let s = Session::new();
    let (a, b) = (s.path("a.txt"), s.path("b.txt"));
    let actions = [format!("write={a}|hello"), format!("read={a}"), format!("rename={a}|{b}"), format!("delete={b}"), format!("mkdir={}", s.path("sub")), "print=done".into()];
    let (session, verdict) = s.run(&actions, json!({}));
    assert_eq!((&verdict["state"], &verdict["exit_code"], &verdict["verdict"], &verdict["problems"]), (&json!("exited"), &json!(0), &json!("ok"), &json!([])));

    let events = s.events(session, json!({"class": "file", "subject": s.folder.to_str().unwrap()}));
    let seen: Vec<(&str, &str)> = events.iter().map(|event| (event["op"].as_str().unwrap(), event["subject"].as_str().unwrap())).collect();
    let wanted = [("open", &a), ("write", &a), ("open", &a), ("read", &a), ("rename", &a), ("delete", &b), ("create_dir", &s.path("sub"))];
    let mut at = 0;
    for (op, subject) in wanted {
        at += seen[at..].iter().position(|event| *event == (op, subject.as_str())).unwrap_or_else(|| panic!("no {op} of {subject} after event {at} in {seen:?}")) + 1;
    }
    let created = &events[0];
    assert_eq!(created["args"]["mode"], json!(["write", "create", "truncate"]));
    assert_eq!((&created["result"], created["process"].as_str()), (&json!("ok"), Some("mc-test-target")));
    assert!(created["time_ns"].as_u64().unwrap() > 1_700_000_000_000_000_000 && created["pid"] == verdict["pid"]);
    let write = events.iter().find(|event| event["op"] == "write").unwrap();
    assert_eq!(write["args"]["bytes"], 5);
    let read_bytes: u64 = events.iter().filter(|event| event["op"] == "read").map(|event| event["args"]["bytes"].as_u64().unwrap()).sum();
    assert_eq!(read_bytes, 5);
    assert_eq!(events.iter().find(|event| event["op"] == "rename").unwrap()["args"]["to"], b);
    assert!(events.iter().all(|event| event["result"] == "ok"), "{events:?}");

    // A file that isn't there: the failure and its reason are events too.
    let missing = s.path("missing.txt");
    let (session, verdict) = s.run(&[format!("read={missing}")], json!({}));
    assert_eq!(verdict["exit_code"], 101, "the target panics on a failed read");
    let failed = s.events(session, json!({"class": "file", "subject": missing}));
    assert_eq!((ops(&failed), &failed[0]["result"]), (vec!["open"], &json!("ENOENT")));

    assert_eq!(s.test.tool("logs", json!({"session": session - 1}))["text"], "done\n");
    assert!(s.test.tool("logs", json!({"session": session, "stream": "stderr"}))["text"].as_str().unwrap().contains("panicked"));
}

#[test]
fn the_summary_and_paging_cover_the_whole_session() {
    let s = Session::new();
    let a = s.path("a.txt");
    let (session, verdict) = s.run(&[format!("write={a}|hello"), format!("read={a}")], json!({}));
    let summary = &verdict["summary"];
    assert_eq!(summary["programs_run"][0]["subject"], TARGET);
    assert_eq!(summary["files_changed"], json!([{"subject": a, "events": 1}]));
    assert!(summary["files_read"].as_array().unwrap().iter().any(|file| file["subject"] == a.as_str()));
    assert!(summary["files_read"].as_array().unwrap().iter().any(|file| file["subject"].as_str().unwrap().contains("libc.so")), "the libraries it loaded: {summary}");
    let by_class = summary["by_class"].as_object().unwrap();
    assert!(by_class["file"].as_u64().unwrap() >= 4 && by_class["process"].as_u64().unwrap() == 3 && by_class["memory"].as_u64().unwrap() > 5, "{by_class:?}");
    assert_eq!(summary["events"].as_u64().unwrap(), by_class.values().map(|count| count.as_u64().unwrap()).sum::<u64>());

    let total = summary["events"].as_u64().unwrap();
    let (mut offset, mut collected) = (json!(0), Vec::new());
    while !offset.is_null() {
        let page = s.test.tool("events", json!({"session": session, "limit": 25, "offset": offset}));
        assert_eq!(page["total"], total);
        collected.extend(page["events"].as_array().unwrap().iter().map(|event| event["time_ns"].as_u64().unwrap()));
        offset = page["next_offset"].clone();
    }
    assert_eq!(collected.len() as u64, total);
    assert!(collected.windows(2).all(|pair| pair[0] <= pair[1]), "oldest first");
    assert_eq!(s.test.tool("events", json!({"session": session, "text": "hello-nothing-has-this"}))["total"], 0);
    assert_eq!(s.test.tool("events", json!({"session": session, "class": ["process", "file"], "op": "exit"}))["total"], 1);
    assert_eq!(s.test.tool("observe_stop", json!({"session": session}))["summary"], *summary, "asking again gives the same answer");
    assert_eq!(s.test.tool("sessions", json!({}))["sessions"][0]["state"], "exited");
}

#[test]
fn programs_it_starts_are_followed_too() {
    let s = Session::new();
    let child_file = s.path("from-child.txt");
    let (session, verdict) = s.run(&["print=parent".into(), "spawn".into(), format!("write={child_file}|x"), "exit=3".into()], json!({}));
    assert_eq!((&verdict["exit_code"], &verdict["verdict"]), (&json!(3), &json!("ok")));
    let root = verdict["pid"].as_u64().unwrap();
    let events = s.events(session, json!({"class": "process"}));
    assert_eq!(ops(&events), ["start", "exec", "start", "exec", "exit", "exit"]);
    let child = events[2]["pid"].as_u64().unwrap();
    assert_ne!(child, root);
    assert_eq!((&events[2]["args"]["parent"], &events[3]["pid"], &events[3]["subject"]), (&json!(root), &json!(child), &json!(TARGET)));
    assert_eq!(events[3]["args"]["argv"], json!([TARGET, format!("write={child_file}|x"), "exit=3"]));
    assert_eq!((&events[4]["pid"], &events[4]["args"]["exit_code"], &events[5]["pid"], &events[5]["args"]["exit_code"]), (&json!(child), &json!(3), &json!(root), &json!(3)));
    // What the child did is filed under the child.
    let written = s.events(session, json!({"class": "file", "op": "write", "subject": child_file}));
    assert_eq!((written.len(), &written[0]["pid"]), (1, &json!(child)));
    assert_eq!(s.events(session, json!({"pid": child, "class": "file", "op": "write"})).len(), 1);
}

#[test]
fn a_shell_command_line_and_a_thread_are_followed() {
    let s = Session::new();
    let file = s.path("from-shell.txt");
    let session = s.test.tool("observe_start", json!({"command": format!("echo hi > '{file}'; cat '{file}'"), "cwd": s.folder.to_str().unwrap()}))["session"].as_u64().unwrap();
    s.finished(session);
    let programs = s.events(session, json!({"class": "process", "op": "exec"}));
    assert!(programs.iter().any(|event| event["subject"].as_str().unwrap().ends_with("/cat")), "{programs:?}");
    assert_eq!(s.events(session, json!({"class": "file", "op": "write", "subject": file})).len(), 1);
    assert_eq!(s.test.tool("logs", json!({"session": session}))["text"], "hi\n");

    let threaded = s.path("from-thread.txt");
    let (session, verdict) = s.run(&[format!("thread-write={threaded}|abc")], json!({}));
    let write = &s.events(session, json!({"class": "file", "op": "write", "subject": threaded}))[0];
    assert_eq!(write["pid"], verdict["pid"], "a thread's work belongs to its process");
    assert_ne!(write["tid"], write["pid"], "and says which thread did it");
    assert_eq!(s.events(session, json!({"class": "process", "op": "start"})).len(), 1, "a thread is not a new process");
}

#[test]
fn network_connections_are_recorded_with_their_addresses() {
    let s = Session::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut received = [0u8; 64];
        let count = stream.read(&mut received).unwrap();
        stream.write_all(b"ok").unwrap();
        String::from_utf8_lossy(&received[..count]).into_owned()
    });
    let (session, verdict) = s.run(&[format!("connect={address}")], json!({}));
    assert_eq!(server.join().unwrap(), "hello from the target");
    assert_eq!(verdict["summary"]["network"], json!([{"subject": address, "events": 1}]));
    let events = s.events(session, json!({"class": "network", "subject": address}));
    assert_eq!(ops(&events), ["connect", "send", "receive"]);
    assert_eq!((&events[0]["result"], &events[1]["args"]["bytes"], &events[2]["args"]["bytes"]), (&json!("ok"), &json!(21), &json!(2)));

    // Nobody listens there any more: the refusal is recorded with its reason.
    let (session, verdict) = s.run(&[format!("connect={address}")], json!({}));
    assert_eq!(verdict["exit_code"], 101);
    let refused = s.events(session, json!({"class": "network", "op": "connect"}));
    assert_eq!((&refused[0]["subject"], &refused[0]["result"]), (&json!(address), &json!("ECONNREFUSED")));
}

#[test]
fn a_crash_is_a_problem_with_the_place_it_happened() {
    let s = Session::new();
    let (session, verdict) = s.run(&["print=before".into(), "segv".into()], json!({}));
    assert_eq!((&verdict["state"], &verdict["signal"], &verdict["verdict"]), (&json!("exited"), &json!("SIGSEGV"), &json!("problem")));
    let problems = verdict["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!((&problems[0]["class"], &problems[0]["op"], &problems[0]["subject"], &problems[0]["args"]["signal"]), (&json!("problem"), &json!("crash"), &json!("mc-test-target"), &json!("SIGSEGV")));
    let at = problems[0]["args"]["at"].as_str().unwrap();
    assert!(at.starts_with("mc-test-target+0x"), "in the program itself: {at}");
    assert_eq!(s.events(session, json!({"class": "process", "op": "exit"}))[0]["args"]["signal"], "SIGSEGV");

    let (_, verdict) = s.run(&["abort".into()], json!({}));
    assert_eq!((&verdict["problems"][0]["args"]["signal"], &verdict["verdict"]), (&json!("SIGABRT"), &json!("problem")));
    assert!(verdict["problems"][0]["args"]["at"].as_str().unwrap().starts_with("libc.so"), "{}", verdict["problems"][0]);

    // A crash in a program it started counts just the same.
    let (_, verdict) = s.run(&["spawn".into(), "segv".into()], json!({}));
    assert_eq!((&verdict["verdict"], verdict["problems"].as_array().unwrap().len()), (&json!("problem"), 1));
    // Ending with an error code is not a crash.
    assert_eq!(s.run(&["exit=1".into()], json!({})).1["verdict"], "ok");
}

#[test]
fn writes_outside_the_allowed_folders_and_to_guarded_files_are_problems() {
    let s = Session::new();
    for folder in ["allowed", "allowed-not"] {
        std::fs::create_dir(s.folder.join(folder)).unwrap();
    }
    std::fs::write(s.folder.join("allowed/keep.txt"), "original").unwrap();
    let (fine, stray, sibling, kept) = (s.path("allowed/ok.txt"), s.path("stray.txt"), s.path("allowed-not/x.txt"), s.path("allowed/keep.txt"));
    let rules = json!({"allow": [s.path("allowed")], "guard": [kept]});
    let (_, verdict) = s.run(&[format!("write={fine}|1"), format!("read={kept}")], rules.clone());
    assert_eq!((&verdict["verdict"], &verdict["problems"]), (&json!("ok"), &json!([])), "writing inside the folder and reading a guarded file are fine");

    let actions = [format!("write={fine}|1"), format!("write={stray}|2"), format!("write={stray}|3"), format!("write={sibling}|4"), format!("write={kept}|5")];
    let (_, verdict) = s.run(&actions, rules.clone());
    assert_eq!(verdict["verdict"], "problem");
    let mut problems: Vec<(&str, &str)> = verdict["problems"].as_array().unwrap().iter().map(|problem| (problem["op"].as_str().unwrap(), problem["subject"].as_str().unwrap())).collect();
    problems.sort();
    assert_eq!(problems, [("guard_broken", kept.as_str()), ("write_outside_allowed", sibling.as_str()), ("write_outside_allowed", stray.as_str())], "each once, however often it happened");

    // Deleting and renaming a guarded file count, with no allowed folders set at all.
    let (_, verdict) = s.run(&[format!("delete={kept}")], json!({"guard": [s.path("allowed")]}));
    assert_eq!((&verdict["problems"][0]["op"], &verdict["problems"][0]["args"]["op"]), (&json!("guard_broken"), &json!("delete")));
    let (_, verdict) = s.run(&[format!("rename={fine}|{stray}")], json!({"guard": [fine]}));
    assert_eq!(verdict["problems"].as_array().unwrap().len(), 1);
    assert_eq!(s.run(&[format!("write={stray}|9")], json!({})).1["verdict"], "ok", "no rules, no problems");
}

#[test]
fn stopping_a_session_stops_the_target_and_what_it_started() {
    let s = Session::new();
    let session = s.start(&["spawn".into(), "print=started".into(), "sleep=60000".into()], json!({}));
    let deadline = Instant::now() + Duration::from_secs(20);
    while s.test.tool("logs", json!({"session": session}))["text"] != "started\n" {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(s.test.tool("summary", json!({"session": session}))["state"], "running");
    let pids: Vec<u64> = s.events(session, json!({"class": "process", "op": "start"})).iter().map(|event| event["pid"].as_u64().unwrap()).collect();
    assert_eq!(pids.len(), 2);
    // The agent's other work goes on while a session runs: the tracer's waiting doesn't take its children's exits.
    assert_eq!(s.test.tool("exec", json!({"command": "echo still works"}))["stdout"], "still works\n");

    let began = Instant::now();
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert!(began.elapsed() < Duration::from_secs(10));
    assert_eq!((&verdict["state"], &verdict["signal"], &verdict["verdict"]), (&json!("stopped"), &json!("SIGKILL"), &json!("ok")), "stopped on request is not a crash");
    for pid in pids {
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(), "{pid} is gone");
    }
    assert_eq!(s.events(session, json!({"class": "process", "op": "exit"})).len(), 2);
}

#[test]
fn every_system_call_is_recorded_only_when_asked() {
    let s = Session::new();
    let file = s.path("a.txt");
    let (session, _) = s.run(&[format!("write={file}|hello")], json!({}));
    assert_eq!(s.events(session, json!({"class": "syscall"})).len(), 0);
    let (session, _) = s.run(&[format!("write={file}|hello")], json!({"syscalls": "full"}));
    let calls = s.events(session, json!({"class": "syscall"}));
    assert!(calls.len() > 20, "{}", calls.len());
    let write = calls.iter().find(|call| call["op"] == "write").expect("the write call");
    assert_eq!((&write["args"]["returned"], &write["result"], write["args"]["args"].as_array().unwrap().len()), (&json!(5), &json!("ok"), 6));
    let open = calls.iter().find(|call| call["op"] == "openat" && call["subject"] == file.as_str()).expect("the openat call with its path");
    assert!(open["args"]["returned"].as_i64().unwrap() >= 3);
    assert!(calls.iter().any(|call| call["op"] == "exit_group" || call["op"] == "mmap"));
}

#[test]
fn mistakes_are_refused_with_a_reason() {
    let s = Session::new();
    assert_eq!(s.test.refused("observe_start", json!({})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "argv": ["true"]})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "syscalls": "some"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "allow": "/tmp"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "cwd": "/no/such/folder"})), (404, "not_found".into()));
    assert_eq!(s.test.refused("observe_start", json!({"argv": ["/no/such/program"]})), (500, "failed".into()));
    for tool in ["observe_stop", "events", "summary", "logs"] {
        assert_eq!(s.test.refused(tool, json!({"session": 99})), (404, "not_found".into()), "{tool}");
        assert_eq!(s.test.refused(tool, json!({})), (400, "bad_input".into()), "{tool}");
    }
    let (session, _) = s.run(&["print=x".into()], json!({}));
    assert_eq!(s.test.refused("events", json!({"session": session, "class": "nonsense"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("events", json!({"session": session, "limit": 0})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("logs", json!({"session": session, "stream": "both"})), (400, "bad_input".into()));
    assert_eq!(s.test.tool("status", json!({}))["capabilities"]["observe"], true);
}

#[test]
fn only_what_the_filters_keep_is_recorded_but_problems_always_are() {
    let s = Session::new();
    std::fs::create_dir(s.folder.join("kept")).unwrap();
    let (kept, dropped, guarded) = (s.path("kept/a.txt"), s.path("b.txt"), s.path("guarded.txt"));
    std::fs::write(&guarded, "original").unwrap();
    let actions = [format!("write={kept}|1"), format!("write={dropped}|2"), format!("write={guarded}|3")];

    // Only file events, and only under one folder: the rest of the machine's files and every other class are left out.
    let (session, verdict) = s.run(&actions, json!({"classes": ["file"], "include": [s.path("kept")], "guard": [guarded]}));
    let events = s.events(session, json!({}));
    let classes: std::collections::BTreeSet<&str> = events.iter().map(|event| event["class"].as_str().unwrap()).collect();
    assert_eq!(classes.into_iter().collect::<Vec<_>>(), ["file", "problem"], "{events:?}");
    assert!(events.iter().filter(|event| event["class"] == "file").all(|event| event["subject"] == kept.as_str()), "{events:?}");
    assert_eq!((&verdict["verdict"], &verdict["problems"][0]["op"], &verdict["problems"][0]["subject"]), (&json!("problem"), &json!("guard_broken"), &json!(guarded)), "the guard is checked on what really happened, kept or not");

    // Everything but some folders.
    let (session, _) = s.run(&actions, json!({"exclude": [s.path("kept"), "/usr", "/lib", "/etc", "/proc"]}));
    let files = s.events(session, json!({"class": "file"}));
    assert!(files.iter().any(|event| event["subject"] == dropped.as_str()) && !files.iter().any(|event| event["subject"] == kept.as_str()), "{files:?}");
    assert!(!files.iter().any(|event| event["subject"].as_str().unwrap().starts_with("/usr/")), "{files:?}");
    assert!(!s.events(session, json!({"class": "process"})).is_empty(), "other classes are untouched");
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "classes": ["files"]})), (400, "bad_input".into()));
}

/// A program that is already running is followed from the moment the session starts, and left running when it stops.
/// Where the kernel lets a program be traced only by the one that started it, this goes through sudo.
#[test]
fn a_running_program_is_followed_and_left_running() {
    let s = Session::new();
    let file = s.path("after-attach.txt");
    let spawn = |actions: &[String]| std::process::Command::new(TARGET).args(actions).stdout(std::process::Stdio::null()).spawn().unwrap();
    let scope = std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope").unwrap_or_default();
    let may = scope.trim() == "0" || std::process::Command::new("sudo").args(["-n", "true"]).status().is_ok_and(|status| status.success());
    if !may {
        let mut target = spawn(&["sleep=3000".into()]);
        assert_eq!(s.test.refused("observe_start", json!({"pid": target.id()})), (501, "not_supported".into()), "the kernel forbids it and there are no admin rights");
        let _ = target.kill();
        let _ = target.wait();
        return;
    }
    // It idles first, so the session is in place before it does anything worth seeing.
    let mut target = spawn(&["sleep=2500".into(), format!("write={file}|x"), "spawn".into(), format!("read={file}"), "exit=4".into()]);
    let pid = target.id();
    let started = s.test.tool("observe_start", json!({"pid": pid}));
    assert_eq!((&started["pid"], &started["command"], &started["attached"]), (&json!(pid), &json!(format!("(running, pid {pid})")), &json!(true)), "{started}");
    let session = started["session"].as_u64().unwrap();
    assert_eq!(target.wait().unwrap().code(), Some(4), "it ran to its own end, unharmed");
    s.finished(session);
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert_eq!((&verdict["state"], &verdict["attached"], &verdict["exit_code"], &verdict["verdict"]), (&json!("exited"), &json!(true), &json!(4), &json!("ok")), "{verdict}");
    let processes = s.events(session, json!({"class": "process"}));
    assert_eq!(ops(&processes), ["attach", "start", "exec", "exit", "exit"], "{processes:?}");
    assert_eq!((&processes[0]["pid"], &processes[0]["subject"], &processes[0]["args"]["argv"][0]), (&json!(pid), &json!(TARGET), &json!(TARGET)), "{processes:?}");
    assert_eq!(processes[1]["args"]["parent"], pid, "what it started afterwards is followed too");
    assert_eq!(s.events(session, json!({"class": "file", "op": "write", "subject": file}))[0]["pid"], pid);
    assert_eq!(s.events(session, json!({"class": "file", "op": "read", "subject": file}))[0]["pid"], processes[1]["pid"]);
    // What the session wrote belongs to the user, whoever the tracer ran as.
    use std::os::unix::fs::MetadataExt;
    let folder = s.test.home.join("sessions").join(session.to_string());
    let own = std::fs::metadata(&s.test.home).unwrap().uid();
    for entry in std::fs::read_dir(&folder).unwrap_or_else(|error| panic!("{}: {error}", folder.display())) {
        let entry = entry.unwrap();
        assert_eq!(entry.metadata().unwrap().uid(), own, "{}", entry.path().display());
    }

    // Stopping the session leaves the program alone.
    let mut target = spawn(&["sleep=60000".into()]);
    let session = s.test.tool("observe_start", json!({"pid": target.id()}))["session"].as_u64().unwrap();
    let began = Instant::now();
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert!(began.elapsed() < Duration::from_secs(10));
    assert_eq!((&verdict["state"], &verdict["verdict"], &verdict["signal"]), (&json!("stopped"), &json!("ok"), &Value::Null), "{verdict}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(target.try_wait().unwrap().is_none(), "still running after the session let go of it");
    assert_eq!(s.events(session, json!({"class": "process", "op": "exit"})).len(), 0);
    let _ = target.kill();
    let _ = target.wait();

    assert_eq!(s.test.refused("observe_start", json!({"pid": 4_000_000})), (500, "failed".into()), "no such program");
    assert_eq!(s.test.refused("observe_start", json!({"pid": pid, "command": "true"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"pid": pid, "cwd": "/tmp"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"pid": 0})), (400, "bad_input".into()));
}

/// With stacks asked for, an event says where in the program it was made from; a crash always does.
#[test]
fn events_and_crashes_carry_the_call_stack_that_led_to_them() {
    let s = Session::new();
    let file = s.path("stacked.txt");
    let (session, _) = s.run(&[format!("write={file}|x")], json!({"stacks": true}));
    let write = &s.events(session, json!({"class": "file", "op": "write", "subject": file}))[0];
    let stack: Vec<&str> = write["stack"].as_array().unwrap().iter().map(|frame| frame.as_str().unwrap()).collect();
    assert!(stack.len() >= 4, "{stack:?}");
    assert!(stack[0].starts_with("libc.so.6!"), "it was made in the C library's own write: {stack:?}");
    let main = stack.iter().position(|frame| frame.starts_with("mc-test-target!mc_test_target::main+0x")).unwrap_or_else(|| panic!("the program's own main is among the callers: {stack:?}"));
    assert!(main > 1 && stack[..main].iter().any(|frame| frame.contains("std::fs::")), "with the library's routines between: {stack:?}");
    assert!(stack.iter().all(|frame| !frame.is_empty() && frame.contains("+0x")), "{stack:?}");

    // Not asked for, not paid for.
    let (session, _) = s.run(&[format!("write={file}|x")], json!({}));
    assert_eq!(s.events(session, json!({"class": "file", "op": "write", "subject": file}))[0]["stack"], json!([]));

    // A crash names the whole way to it, asked or not.
    let (_, verdict) = s.run(&["segv".into()], json!({}));
    let crash = &verdict["problems"][0];
    let stack: Vec<&str> = crash["args"]["stack"].as_array().unwrap().iter().map(|frame| frame.as_str().unwrap()).collect();
    assert!(stack[0].starts_with("mc-test-target!") && stack.iter().any(|frame| frame.starts_with("mc-test-target!mc_test_target::main+0x")), "{stack:?}");
    assert!(crash["args"]["at"].as_str().unwrap().starts_with("mc-test-target+0x"), "{crash}");
}


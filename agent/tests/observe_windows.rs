//! Observation on Windows: sessions that follow a small target program which does exactly what the test tells it to.
//! Needs admin rights (a kernel trace session), which the desktop-session runner has.
#![cfg(windows)]

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
        std::env::set_var("MC_AGENT_PROGRAM", env!("CARGO_BIN_EXE_mc-agent"));
        let test = Test::new();
        let folder = test.home.join("work");
        std::fs::create_dir_all(&folder).unwrap();
        // The long, real path, without the \\?\ that canonicalize puts in front: events carry paths as people write them.
        let folder = PathBuf::from(folder.canonicalize().unwrap().to_str().unwrap().trim_start_matches(r"\\?\"));
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

    fn finished(&self, session: u64) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.test.tool("summary", json!({ "session": session }))["state"] == "running" {
            assert!(Instant::now() < deadline, "the session was still running");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

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

fn same_path(event: &Value, path: &str) -> bool {
    event["subject"].as_str().unwrap().eq_ignore_ascii_case(path)
}

#[test]
fn file_activity_is_recorded_in_order_with_its_details() {
    let s = Session::new();
    let (a, b) = (s.path("a.txt"), s.path("b.txt"));
    let (session, verdict) = s.run(&[format!("write={a}|hello"), format!("read={a}"), format!("rename={a}|{b}"), format!("delete={b}"), "print=done".into()], json!({}));
    assert_eq!((&verdict["state"], &verdict["exit_code"], &verdict["verdict"], &verdict["problems"]), (&json!("exited"), &json!(0), &json!("ok"), &json!([])));

    let events = s.events(session, json!({"class": "file", "subject": s.folder.to_str().unwrap()}));
    let seen: Vec<(&str, &str)> = events.iter().map(|event| (event["op"].as_str().unwrap(), event["subject"].as_str().unwrap())).collect();
    let mut at = 0;
    for (op, subject) in [("open", &a), ("write", &a), ("read", &a), ("rename", &a), ("delete", &b)] {
        at += seen[at..].iter().position(|(seen_op, seen_subject)| *seen_op == op && seen_subject.eq_ignore_ascii_case(subject)).unwrap_or_else(|| panic!("no {op} of {subject} after event {at} in {seen:?}")) + 1;
    }
    let write = events.iter().find(|event| event["op"] == "write").unwrap();
    assert_eq!((&write["args"]["bytes"], &write["args"]["offset"], write["process"].as_str()), (&json!(5), &json!(0), Some("mc-test-target.exe")));
    assert!(write["time_ns"].as_u64().unwrap() > 1_700_000_000_000_000_000 && write["pid"] == verdict["pid"]);
    assert!(events.iter().find(|event| event["op"] == "rename").unwrap()["args"]["to"].as_str().unwrap().eq_ignore_ascii_case(&b));
    assert!(events.iter().any(|event| event["op"] == "open" && same_path(event, &a) && event["args"]["mode"] != "open"), "the file was made, not just opened: {seen:?}");

    assert_eq!(s.test.tool("logs", json!({"session": session}))["text"], "done\n");
    let summary = &verdict["summary"];
    assert!(summary["programs_run"].as_array().unwrap().iter().any(|program| program["subject"].as_str().unwrap().to_lowercase().ends_with("mc-test-target.exe")), "{summary}");
    assert!(summary["files_changed"].as_array().unwrap().iter().any(|file| file["subject"].as_str().unwrap().eq_ignore_ascii_case(&a)), "{summary}");
    assert!(summary["by_class"]["file"].as_u64().unwrap() >= 5 && summary["by_class"]["process"].as_u64().unwrap() >= 3, "{summary}");
}

#[test]
fn programs_it_starts_are_followed_and_a_crash_is_a_problem() {
    let s = Session::new();
    let child_file = s.path("from-child.txt");
    let (session, verdict) = s.run(&["print=parent".into(), "spawn".into(), format!("write={child_file}|x"), "segv".into()], json!({}));
    let root = verdict["pid"].as_u64().unwrap();
    let ours = |events: Vec<Value>| events.into_iter().filter(|event| event["process"] == "mc-test-target.exe").collect::<Vec<_>>();
    let starts = ours(s.events(session, json!({"class": "process", "op": "start"})));
    assert_eq!(starts.len(), 2, "{starts:?}");
    let child = starts.iter().find(|event| event["pid"] != root).expect("the child")["pid"].as_u64().unwrap();
    assert_eq!(starts.iter().find(|event| event["pid"] == child).unwrap()["args"]["parent"], root);
    let written = s.events(session, json!({"class": "file", "op": "write", "subject": child_file}));
    assert_eq!((written.len(), &written[0]["pid"]), (1, &json!(child)), "what the child did is filed under the child");
    let exits = ours(s.events(session, json!({"class": "process", "op": "exit"})));
    assert_eq!(exits.iter().find(|event| event["pid"] == child).unwrap()["args"]["exit_code"], 0xC000_0005u32);

    assert_eq!(verdict["verdict"], "problem");
    let problems = verdict["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "once for the program, though parent and child both end with the code: {problems:?}");
    assert_eq!((&problems[0]["op"], &problems[0]["subject"], &problems[0]["args"]["code"], &problems[0]["args"]["what"]), (&json!("crash"), &json!("mc-test-target.exe"), &json!("0xC0000005"), &json!("access violation")));
    // Ending with an error code of its own is not a crash.
    let (_, verdict) = s.run(&["exit=3".into()], json!({}));
    assert_eq!((&verdict["exit_code"], &verdict["verdict"]), (&json!(3), &json!("ok")));
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
    let events = s.events(session, json!({"class": "network", "subject": address}));
    let ops: Vec<&str> = events.iter().map(|event| event["op"].as_str().unwrap()).collect();
    assert!(ops.contains(&"connect"), "{ops:?} in {:?}", s.events(session, json!({"class": "network"})));
    let sent = events.iter().find(|event| event["op"] == "send").expect("the send");
    assert_eq!((&sent["args"]["bytes"], &sent["pid"]), (&json!(21), &verdict["pid"]));
    assert!(verdict["summary"]["network"].as_array().unwrap().iter().any(|entry| entry["subject"] == address.as_str()));
}

#[test]
fn stray_writes_and_changes_to_guarded_files_and_keys_are_problems() {
    let s = Session::new();
    std::fs::create_dir(s.folder.join("allowed")).unwrap();
    std::fs::create_dir(s.folder.join("allowed-not")).unwrap();
    std::fs::write(s.folder.join("allowed").join("keep.txt"), "original").unwrap();
    let (fine, stray, sibling, kept) = (s.path("allowed\\ok.txt"), s.path("stray.txt"), s.path("allowed-not\\x.txt"), s.path("allowed\\keep.txt"));
    let rules = json!({"allow": [s.path("allowed")], "guard": [kept]});
    let (_, verdict) = s.run(&[format!("write={fine}|1"), format!("read={kept}")], rules.clone());
    assert_eq!((&verdict["verdict"], &verdict["problems"]), (&json!("ok"), &json!([])), "writing inside the folder and reading a guarded file are fine");

    let (_, verdict) = s.run(&[format!("write={fine}|1"), format!("write={stray}|2"), format!("write={stray}|3"), format!("write={sibling}|4"), format!("write={kept}|5")], rules);
    let mut problems: Vec<(String, String)> = verdict["problems"].as_array().unwrap().iter().map(|problem| (problem["op"].as_str().unwrap().to_string(), problem["subject"].as_str().unwrap().to_lowercase())).collect();
    problems.sort();
    assert_eq!(problems, [("guard_broken".to_string(), kept.to_lowercase()), ("write_outside_allowed".to_string(), sibling.to_lowercase()), ("write_outside_allowed".to_string(), stray.to_lowercase())], "each once, however often it happened");

    // A guarded registry key: making it and setting a value in it are both changes. The key is the test's own.
    let key = format!(r"HKCU\Software\MasterControlTest-{}\Guarded", std::process::id());
    let add = |guard: Value| {
        let session = s.test.tool("observe_start", json!({"argv": ["reg", "add", key, "/v", "probe", "/d", "1", "/f"], "guard": guard}))["session"].as_u64().unwrap();
        s.finished(session);
        (session, s.test.tool("observe_stop", json!({"session": session})))
    };
    let (session, verdict) = add(json!([key]));
    let set = s.events(session, json!({"class": "registry", "op": "set_value"}));
    assert!(set.iter().any(|event| event["subject"].as_str().unwrap().eq_ignore_ascii_case(&format!(r"{key}\probe")) && event["result"] == "ok"), "{set:?}");
    assert_eq!(verdict["verdict"], "problem");
    let problems = verdict["problems"].as_array().unwrap();
    assert!(!problems.is_empty() && problems.iter().all(|problem| problem["op"] == "guard_broken" && problem["subject"].as_str().unwrap().to_lowercase().starts_with(&key.to_lowercase())), "{problems:?}");
    // The key beside it is not guarded by this one's name.
    let (_, verdict) = add(json!([format!("{key}-other")]));
    assert_eq!(verdict["verdict"], "ok", "{}", verdict["problems"]);
    let parent = key.rsplit_once('\\').unwrap().0;
    assert_eq!(s.test.tool("exec", json!({"argv": ["reg", "delete", parent, "/f"]}))["exit_code"], 0);
}

#[test]
fn stopping_a_session_stops_the_target_and_mistakes_are_refused() {
    let s = Session::new();
    let session = s.start(&["print=started".into(), "sleep=60000".into()], json!({}));
    let deadline = Instant::now() + Duration::from_secs(30);
    while s.test.tool("logs", json!({"session": session}))["text"] != "started\n" {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(s.test.tool("summary", json!({"session": session}))["state"], "running");
    assert_eq!(s.test.tool("exec", json!({"command": "echo still works"}))["stdout"].as_str().unwrap().trim(), "still works");
    let began = Instant::now();
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert!(began.elapsed() < Duration::from_secs(20), "{:?}", began.elapsed());
    assert_eq!((&verdict["state"], &verdict["verdict"]), (&json!("stopped"), &json!("ok")), "stopped on request is not a crash: {verdict}");
    let pid = verdict["pid"].as_u64().unwrap();
    let listed = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&listed.stdout).contains(&format!("\"{pid}\"")), "the target is gone");

    assert_eq!(s.test.refused("observe_start", json!({})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"argv": ["C:\\no\\such\\program.exe"]})), (500, "failed".into()));
    assert_eq!(s.test.refused("events", json!({"session": 99})), (404, "not_found".into()));
    assert_eq!(s.test.tool("status", json!({}))["capabilities"]["observe"], true);
}

#[test]
fn it_records_what_the_linux_tracer_records_where_windows_can_tell() {
    let s = Session::new();
    let (a, missing) = (s.path("a.txt"), s.path("missing.txt"));
    let (session, verdict) = s.run(&[format!("write={a}|hello"), format!("read={a}"), "print=said so".into()], json!({}));
    assert_eq!(verdict["verdict"], "ok");

    // The command line of the program, as Windows has it: one line.
    let programs = s.events(session, json!({"class": "process", "op": "exec"}));
    let ours = programs.iter().find(|event| event["pid"] == verdict["pid"]).expect("the target's own start");
    let line = ours["args"]["command_line"].as_str().unwrap_or_else(|| panic!("no command line in {ours}"));
    assert!(line.contains("mc-test-target") && line.contains("print=said so") && line.contains("a.txt|hello"), "{line}");

    // How each file operation ended, and how much was really read and written.
    let files = s.events(session, json!({"class": "file", "subject": a}));
    let made = files.iter().find(|event| event["op"] == "open").unwrap();
    assert_eq!((&made["args"]["mode"], &made["result"]), (&json!("create"), &json!("ok")), "{made}");
    let write = files.iter().find(|event| event["op"] == "write").unwrap();
    assert_eq!((&write["args"]["bytes"], &write["args"]["asked"], &write["result"]), (&json!(5), &json!(5), &json!("ok")));
    let reads: Vec<&Value> = files.iter().filter(|event| event["op"] == "read").collect();
    assert_eq!(reads.iter().map(|event| event["args"]["bytes"].as_u64().unwrap()).sum::<u64>(), 5, "five bytes came back, whatever was asked for: {reads:?}");
    assert!(reads.iter().any(|event| event["args"]["asked"].as_u64().unwrap() >= 5));
    let reopened = files.iter().filter(|event| event["op"] == "open").nth(1).expect("opened again to read");
    assert_eq!(reopened["args"]["mode"], "open", "an existing file opened is not a file made");

    // Memory the program asked the system for.
    let memory = s.events(session, json!({"class": "memory"}));
    assert!(!memory.is_empty(), "no memory events");
    let map = memory.iter().find(|event| event["op"] == "map").expect("an allocation");
    assert!(map["subject"].as_str().unwrap().starts_with("0x") && map["args"]["length"].as_u64().unwrap() > 0 && map["pid"] == verdict["pid"], "{map}");
    assert!(verdict["summary"]["by_class"]["memory"].as_u64().unwrap() >= 1);

    // A file that isn't there: the failure and its reason.
    let (session, verdict) = s.run(&[format!("read={missing}")], json!({}));
    assert_eq!(verdict["exit_code"], 101, "the target panics on a failed read");
    let failed = s.events(session, json!({"class": "file", "subject": missing}));
    assert!(failed.iter().any(|event| event["op"] == "open" && event["result"] == "OBJECT_NAME_NOT_FOUND"), "{failed:?}");

    // A write that is refused changes nothing, so it breaks no guard: the folder is in the way of the file's name.
    let folder_in_the_way = s.path("taken");
    std::fs::create_dir(&folder_in_the_way).unwrap();
    let (_, verdict) = s.run(&[format!("write={folder_in_the_way}|x")], json!({"guard": [folder_in_the_way]}));
    assert_eq!((&verdict["exit_code"], &verdict["verdict"]), (&json!(101), &json!("ok")), "{}", verdict["problems"]);
}

#[test]
fn only_what_the_filters_keep_is_recorded_but_problems_always_are() {
    let s = Session::new();
    std::fs::create_dir(s.folder.join("kept")).unwrap();
    let (kept, dropped, guarded) = (s.path("kept\\a.txt"), s.path("b.txt"), s.path("guarded.txt"));
    std::fs::write(&guarded, "original").unwrap();
    let actions = [format!("write={kept}|1"), format!("write={dropped}|2"), format!("write={guarded}|3")];

    // Only file events, and only under one folder: the rest of the machine's files and every other class are left out.
    let (session, verdict) = s.run(&actions, json!({"classes": ["file"], "include": [s.path("kept")], "guard": [guarded]}));
    let events = s.events(session, json!({}));
    let classes: std::collections::BTreeSet<&str> = events.iter().map(|event| event["class"].as_str().unwrap()).collect();
    assert_eq!(classes.into_iter().collect::<Vec<_>>(), ["file", "problem"], "{events:?}");
    assert!(events.iter().filter(|event| event["class"] == "file").all(|event| event["subject"].as_str().unwrap().eq_ignore_ascii_case(&kept)), "{events:?}");
    assert_eq!((&verdict["verdict"], &verdict["problems"][0]["op"]), (&json!("problem"), &json!("guard_broken")), "the guard is checked on what really happened, kept or not: {verdict}");

    // Everything but some folders.
    let (session, _) = s.run(&actions, json!({"exclude": [s.path("kept"), "C:\\Windows"]}));
    let files = s.events(session, json!({"class": "file"}));
    let named = |path: &str| files.iter().any(|event| event["subject"].as_str().unwrap().eq_ignore_ascii_case(path));
    assert!(named(&dropped) && !named(&kept), "{files:?}");
    assert!(!files.iter().any(|event| event["subject"].as_str().unwrap().to_lowercase().starts_with("c:\\windows\\")), "{files:?}");
    assert!(!s.events(session, json!({"class": "process"})).is_empty(), "other classes are untouched");
    assert_eq!(s.test.refused("observe_start", json!({"command": "ver", "classes": ["files"]})), (400, "bad_input".into()));
}

/// A program that is already running is followed from the moment the session starts, and left running when it stops.
#[test]
fn a_running_program_is_followed_and_left_running() {
    let s = Session::new();
    let file = s.path("after-attach.txt");
    let spawn = |actions: &[String]| std::process::Command::new(TARGET).args(actions).stdout(std::process::Stdio::null()).spawn().unwrap();
    // It idles first, so the session is in place before it does anything worth seeing.
    let mut target = spawn(&["sleep=4000".into(), format!("write={file}|x"), "spawn".into(), format!("read={file}"), "exit=4".into()]);
    let pid = target.id();
    let started = s.test.tool("observe_start", json!({"pid": pid}));
    assert_eq!((&started["pid"], &started["command"], &started["attached"]), (&json!(pid), &json!(format!("(running, pid {pid})")), &json!(true)), "{started}");
    let session = started["session"].as_u64().unwrap();
    assert_eq!(target.wait().unwrap().code(), Some(4), "it ran to its own end, unharmed");
    s.finished(session);
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert_eq!((&verdict["state"], &verdict["attached"], &verdict["exit_code"], &verdict["verdict"]), (&json!("exited"), &json!(true), &json!(4), &json!("ok")), "{verdict}");
    let processes = s.events(session, json!({"class": "process"}));
    assert_eq!(processes[0]["op"], "attach", "{processes:?}");
    assert_eq!(processes[0]["pid"], pid);
    assert!(processes[0]["subject"].as_str().unwrap().to_lowercase().ends_with("mc-test-target.exe"), "{processes:?}");
    let child = processes.iter().find(|event| event["op"] == "start" && event["pid"] != pid).unwrap_or_else(|| panic!("what it started afterwards is followed too: {processes:?}"));
    let wrote = s.events(session, json!({"class": "file", "pid": pid}));
    assert!(wrote.iter().any(|event| event["subject"].as_str().unwrap().eq_ignore_ascii_case(&file)), "{wrote:?}");
    let read = s.events(session, json!({"class": "file", "pid": child["pid"]}));
    assert!(read.iter().any(|event| event["subject"].as_str().unwrap().eq_ignore_ascii_case(&file)), "{read:?}");

    // Stopping the session leaves the program alone.
    let mut target = spawn(&["sleep=60000".into()]);
    let session = s.test.tool("observe_start", json!({"pid": target.id()}))["session"].as_u64().unwrap();
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert_eq!((&verdict["state"], &verdict["verdict"], &verdict["exit_code"]), (&json!("stopped"), &json!("ok"), &Value::Null), "{verdict}");
    assert!(target.try_wait().unwrap().is_none(), "still running after the session let go of it");
    let _ = target.kill();
    let _ = target.wait();

    assert_eq!(s.test.refused("observe_start", json!({"pid": 4_000_000})), (500, "failed".into()), "no such program");
    assert_eq!(s.test.refused("observe_start", json!({"pid": pid, "command": "ver"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"pid": 0})), (400, "bad_input".into()));
}

/// With stacks asked for, an event says where in the program it was made from.
#[test]
fn events_carry_the_call_stack_that_led_to_them() {
    let s = Session::new();
    let file = s.path("stacked.txt");
    let (session, _) = s.run(&[format!("write={file}|x")], json!({"stacks": true}));
    let events = s.events(session, json!({"class": "file"}));
    let ours: Vec<&Value> = events.iter().filter(|event| event["subject"].as_str().unwrap().eq_ignore_ascii_case(&file)).collect();
    let stacked = ours.iter().find(|event| event["stack"].as_array().is_some_and(|stack| stack.len() >= 3)).unwrap_or_else(|| panic!("no event on the file has a stack: {ours:?}"));
    let stack: Vec<&str> = stacked["stack"].as_array().unwrap().iter().map(|frame| frame.as_str().unwrap()).collect();
    assert!(stack.iter().any(|frame| frame.starts_with("ntdll.dll!")), "it went through Windows' own library: {stack:?}");
    let log = std::fs::read_to_string(s.test.home.join("sessions").join(session.to_string()).join("tracer.log")).unwrap_or_default();
    assert!(stack.iter().any(|frame| frame.starts_with("mc-test-target.exe!") && frame.contains("main")), "the program's own main is among the callers: {stack:?}\n{log}");
    assert!(stack.iter().all(|frame| !frame.is_empty()), "{stack:?}");

    // Not asked for, not paid for.
    let (session, _) = s.run(&[format!("write={file}|x")], json!({}));
    assert!(s.events(session, json!({"class": "file"})).iter().all(|event| event["stack"] == json!([])));
}


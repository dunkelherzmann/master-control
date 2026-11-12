// The observation tests for macOS, shared by the two programs that run them: once with the system's own events
// (admin rights) and once with the sandbox's reports alone. Each sets WITHOUT_ROOT before including this.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::Test;
use serde_json::{json, Value};

const TARGET: &str = env!("CARGO_BIN_EXE_mc-test-target");

/// Whether sessions get the system's own events: this user may become root unasked, and the test wants it.
fn by_events() -> bool {
    !WITHOUT_ROOT && std::process::Command::new("sudo").args(["-n", "true"]).stderr(std::process::Stdio::null()).status().is_ok_and(|status| status.success())
}

struct Session {
    test: Test,
    folder: PathBuf,
}

impl Session {
    fn new() -> Session {
        // The agent starts its recorder by running its own program; inside a test, that program is elsewhere.
        std::env::set_var("MC_AGENT_PROGRAM", env!("CARGO_BIN_EXE_mc-agent"));
        // Every test of this program sets the same, so they don't disturb each other.
        if WITHOUT_ROOT {
            std::env::set_var("MC_OBSERVE_WITHOUT_ROOT", "1");
        }
        let test = Test::new();
        let folder = test.home.join("work");
        std::fs::create_dir_all(&folder).unwrap();
        // The real path: the temporary folder is reached through a link, and events carry real paths.
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

/// Removes what a session's crashes left with the system: exactly the report files the verdict names, when they are
/// reports of a program whose name starts as given, and the crash reporter's own small note about that program.
/// The tests crash programs on purpose, and what that leaves is no use to the person whose machine this is.
fn tidy(verdict: &Value, program: &str) -> usize {
    let library = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("Library");
    let folder = library.join("Logs").join("DiagnosticReports");
    let mut removed = 0;
    for problem in verdict["problems"].as_array().into_iter().flatten() {
        let Some(report) = problem["args"]["report"].as_str().map(std::path::PathBuf::from) else { continue };
        let ours = report.parent() == Some(folder.as_path()) && report.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with(&format!("{program}-")) && name.ends_with(".ips"));
        if ours && std::fs::remove_file(&report).is_ok() {
            removed += 1;
        }
    }
    // "<program>_<a number for this machine>.plist": when the program last crashed.
    for note in std::fs::read_dir(library.join("Application Support").join("CrashReporter")).into_iter().flatten().flatten() {
        if note.file_name().to_string_lossy().starts_with(&format!("{program}_")) {
            let _ = std::fs::remove_file(note.path());
        }
    }
    removed
}

fn ops(events: &[Value]) -> Vec<&str> {
    events.iter().map(|event| event["op"].as_str().unwrap()).collect()
}

#[test]
fn file_activity_is_recorded_in_order() {
    let s = Session::new();
    let (a, b, folder) = (s.path("a.txt"), s.path("b.txt"), s.path("made"));
    let actions = [format!("write={a}|hello"), format!("read={a}"), format!("write={a}|again"), format!("rename={a}|{b}"), format!("mkdir={folder}"), format!("delete={b}")];
    let (session, verdict) = s.run(&actions, json!({}));
    assert_eq!((&verdict["state"], &verdict["exit_code"], &verdict["verdict"]), (&json!("exited"), &json!(0), &json!("ok")));
    let events = s.events(session, json!({"class": "file", "subject": s.folder.to_str().unwrap()}));
    let seen: Vec<(&str, &str)> = events.iter().map(|event| (event["op"].as_str().unwrap(), event["subject"].as_str().unwrap())).collect();
    assert_eq!(verdict["source"], if by_events() { "system events" } else { "sandbox reports" });
    let wanted = if by_events() {
        assert_eq!(events.iter().find(|event| event["op"] == "rename").unwrap()["args"]["to"], b.as_str());
        vec![("write", &a), ("read", &a), ("write", &a), ("rename", &a), ("create_dir", &folder), ("delete", &b)]
    } else {
        // A rename is told as the old name going and the new one coming.
        vec![("write", &a), ("read", &a), ("write", &a), ("delete", &a), ("write", &b), ("create_dir", &folder), ("delete", &b)]
    };
    let mut at = 0;
    for (op, subject) in wanted {
        at += seen[at..].iter().position(|event| *event == (op, subject.as_str())).unwrap_or_else(|| panic!("no {op} of {subject} after event {at} in {seen:?}")) + 1;
    }
    assert_eq!(events.iter().find(|event| event["op"] == "write").unwrap()["args"]["created"], true);
    assert!(events.iter().all(|event| event["pid"] == verdict["pid"] && event["process"] == "mc-test-target"), "{events:?}");

    let summary = &verdict["summary"];
    assert_eq!(summary["programs_run"][0]["subject"], TARGET);
    let changed: Vec<&str> = summary["files_changed"].as_array().unwrap().iter().map(|file| file["subject"].as_str().unwrap()).collect();
    assert!([&a, &b, &folder].iter().all(|path| changed.contains(&path.as_str())), "{changed:?}");
    assert!(summary["files_read"].as_array().unwrap().iter().any(|file| file["subject"] == a.as_str()));
    assert!(summary["files_read"].as_array().unwrap().iter().any(|file| file["subject"] == TARGET), "the program itself is read as it loads: {summary}");
}

#[test]
fn programs_it_starts_are_followed_too() {
    let s = Session::new();
    let child_file = s.path("from-child.txt");
    // The child stays a moment, long enough to be seen running: that is when its arguments can be read.
    let child_does = [format!("write={child_file}|x"), "sleep=300".to_string(), "exit=3".to_string()];
    let actions: Vec<String> = ["print=parent".to_string(), "spawn".to_string()].into_iter().chain(child_does.iter().cloned()).collect();
    let (session, verdict) = s.run(&actions, json!({}));
    assert_eq!((&verdict["exit_code"], &verdict["verdict"]), (&json!(3), &json!("ok")));
    let root = verdict["pid"].as_u64().unwrap();
    let events = s.events(session, json!({"class": "process"}));
    assert_eq!(ops(&events), ["start", "exec", "start", "exec", "exit", "exit"], "{events:?}");
    assert_eq!((&events[1]["pid"], &events[1]["subject"]), (&json!(root), &json!(TARGET)));
    assert_eq!(events[1]["args"]["argv"], json!(std::iter::once(TARGET.to_string()).chain(actions.iter().cloned()).collect::<Vec<_>>()));
    let child = events[2]["pid"].as_u64().unwrap();
    assert_ne!(child, root);
    assert_eq!((&events[2]["args"]["parent"], &events[3]["pid"], &events[3]["subject"]), (&json!(root), &json!(child), &json!(TARGET)));
    assert_eq!(events[3]["args"]["argv"], json!(std::iter::once(TARGET.to_string()).chain(child_does.iter().cloned()).collect::<Vec<_>>()));
    assert_eq!((&events[4]["pid"], &events[4]["args"]["exit_code"], &events[5]["pid"], &events[5]["args"]["exit_code"]), (&json!(child), &json!(3), &json!(root), &json!(3)));
    // What the child did is filed under the child.
    let written = s.events(session, json!({"class": "file", "op": "write", "subject": child_file}));
    assert!(!written.is_empty() && written.iter().all(|event| event["pid"] == child), "{written:?}");
}

#[test]
fn a_shell_command_line_is_followed_through_the_programs_it_runs() {
    let s = Session::new();
    let file = s.path("said.txt");
    let session = s.test.tool("observe_start", json!({"command": format!("echo hi > '{file}'; cat '{file}'; /bin/ls / > /dev/null"), "cwd": s.folder.to_str().unwrap()}))["session"].as_u64().unwrap();
    assert_eq!(s.finished(session)["exit_code"], 0);
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert_eq!(s.test.tool("logs", json!({"session": session}))["text"], "hi\n");
    let programs = s.events(session, json!({"class": "process", "op": "exec"}));
    // Programs that live a few thousandths of a second: found through the reports alone.
    for program in ["/bin/cat", "/bin/ls"] {
        let ran = programs.iter().find(|event| event["subject"] == program).unwrap_or_else(|| panic!("no {program} in {programs:?}"));
        assert_ne!(ran["pid"], verdict["pid"], "{program} ran as a process of its own");
        let read_by = s.events(session, json!({"class": "file", "op": "read", "pid": ran["pid"]}));
        assert!(!read_by.is_empty(), "what {program} read is filed under it");
    }
    assert!(!s.events(session, json!({"class": "file", "op": "write", "subject": file})).is_empty());
    let cat = programs.iter().find(|event| event["subject"] == "/bin/cat").unwrap();
    assert_eq!(s.events(session, json!({"class": "file", "op": "read", "subject": file}))[0]["pid"], cat["pid"]);
    let starts = s.events(session, json!({"class": "process", "op": "start"}));
    let exits = s.events(session, json!({"class": "process", "op": "exit"}));
    assert_eq!((starts.len(), exits.len()), (3, 3), "the shell and the two programs: {starts:?} {exits:?}");
}

#[test]
fn network_connections_are_recorded_with_their_ports() {
    let s = Session::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        let mut received = [0u8; 64];
        let count = stream.read(&mut received).unwrap();
        stream.write_all(b"ok").unwrap();
        String::from_utf8_lossy(&received[..count]).into_owned()
    });
    let (session, verdict) = s.run(&[format!("connect=127.0.0.1:{port}")], json!({}));
    assert_eq!(server.join().unwrap(), "hello from the target");
    // The system tells the port, not the address.
    let subject = format!("*:{port}");
    assert_eq!(verdict["summary"]["network"], json!([{"subject": subject, "events": 1}]));
    let events = s.events(session, json!({"class": "network"}));
    assert_eq!((ops(&events), &events[0]["pid"]), (vec!["connect"], &verdict["pid"]));
}

#[test]
fn a_crash_is_a_problem() {
    let s = Session::new();
    let (session, verdict) = s.run(&["print=before".into(), "segv".into()], json!({}));
    let signal = verdict["signal"].as_str().unwrap().to_string();
    assert!(signal == "SIGSEGV" || signal == "SIGBUS", "{verdict}");
    assert_eq!((&verdict["state"], &verdict["verdict"]), (&json!("exited"), &json!("problem")));
    let problems = verdict["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!((&problems[0]["class"], &problems[0]["op"], &problems[0]["subject"], &problems[0]["args"]["signal"]), (&json!("problem"), &json!("crash"), &json!("mc-test-target"), &json!(signal)));
    assert_eq!(s.events(session, json!({"class": "process", "op": "exit"}))[0]["args"]["signal"], signal.as_str());
    // The way to the crash is read as it happens, so it is there whether or not macOS writes a report (it writes
    // none for a program that has crashed again and again, as this one has).
    let stack_of = |problem: &Value| -> Vec<String> { problem["args"]["stack"].as_array().unwrap_or_else(|| panic!("the crash has its stack: {problem}")).iter().map(|frame| frame.as_str().unwrap().to_string()).collect() };
    let stack = stack_of(&problems[0]);
    assert!(stack[0].starts_with("mc-test-target!") && stack.iter().any(|frame| frame.starts_with("mc-test-target!mc_test_target::main+0x")), "{stack:?}");
    assert!(stack.last().unwrap().contains("start"), "all the way out to where the program began: {stack:?}");
    assert_eq!(problems[0]["args"]["at"], stack[0].as_str());
    tidy(&verdict, "mc-test-target");

    // Under a name the system has never seen, the program's crash is reported at once, and the way to it comes from
    // that report: every run uses a new name, and removes what it leaves.
    let fresh = format!("mct{:x}{}", std::process::id(), if by_events() { "e" } else { "r" });
    let copy = s.folder.join(&fresh);
    std::fs::copy(TARGET, &copy).unwrap();
    let started = s.test.tool("observe_start", json!({"argv": [copy.to_str().unwrap(), "segv"]}));
    let session = started["session"].as_u64().unwrap();
    s.finished(session);
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    let crash = &verdict["problems"][0];
    let stack = stack_of(crash);
    assert!(stack[0].starts_with(&format!("{fresh}!")) && stack.iter().any(|frame| frame.starts_with(&format!("{fresh}!mc_test_target::main+0x"))), "{stack:?}");
    tidy(&verdict, &fresh);

    let (_, verdict) = s.run(&["abort".into()], json!({}));
    assert_eq!((&verdict["problems"][0]["args"]["signal"], &verdict["verdict"]), (&json!("SIGABRT"), &json!("problem")));
    let stack = stack_of(&verdict["problems"][0]);
    assert!(stack.iter().any(|frame| frame.contains("!abort+")) && stack.iter().any(|frame| frame.contains("mc_test_target::main+0x")), "through the system's abort, from main: {stack:?}");
    tidy(&verdict, "mc-test-target");
    // A crash in a program it started counts just the same.
    let (_, verdict) = s.run(&["spawn".into(), "segv".into()], json!({}));
    assert_eq!((&verdict["verdict"], verdict["problems"].as_array().unwrap().len()), (&json!("problem"), 1));
    assert!(stack_of(&verdict["problems"][0]).iter().any(|frame| frame.contains("mc_test_target::main+0x")), "a child's crash is caught just the same");
    tidy(&verdict, "mc-test-target");
    // Ending with an error code is not a crash.
    assert_eq!(s.run(&["exit=1".into()], json!({})).1["verdict"], "ok");
    // The system writes its report and its note of a crash a moment after it, when the session has long since told
    // of the crash: what these crashes left is removed once that moment has passed.
    std::thread::sleep(Duration::from_secs(3));
    let library = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("Library");
    for (folder, mark) in [(library.join("Logs").join("DiagnosticReports"), '-'), (library.join("Application Support").join("CrashReporter"), '_')] {
        let ours = |name: &str| [fresh.as_str(), "mc-test-target"].iter().any(|program| name.starts_with(&format!("{program}{mark}")));
        for left in std::fs::read_dir(folder).into_iter().flatten().flatten().filter(|entry| ours(&entry.file_name().to_string_lossy())) {
            let _ = std::fs::remove_file(left.path());
        }
    }
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

    // Deleting and renaming a guarded file count, with no allowed folders set at all. The rules may name a path
    // through a link (the temporary folder is one): they are matched against the real path.
    let through_link = s.test.home.join("work/allowed").to_str().unwrap().to_string();
    let (_, verdict) = s.run(&[format!("delete={kept}")], json!({"guard": [through_link]}));
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
    let pids = loop {
        let printed = s.test.tool("logs", json!({"session": session}))["text"] == "started\n";
        let pids: Vec<u64> = s.events(session, json!({"class": "process", "op": "start"})).iter().map(|event| event["pid"].as_u64().unwrap()).collect();
        if printed && pids.len() == 2 {
            break pids;
        }
        assert!(Instant::now() < deadline, "{pids:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(s.test.tool("summary", json!({"session": session}))["state"], "running");
    // The agent's other work goes on while a session runs.
    assert_eq!(s.test.tool("exec", json!({"command": "echo still works"}))["stdout"], "still works\n");

    let began = Instant::now();
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert!(began.elapsed() < Duration::from_secs(10));
    assert_eq!((&verdict["state"], &verdict["signal"], &verdict["verdict"]), (&json!("stopped"), &json!("SIGKILL"), &json!("ok")), "stopped on request is not a crash");
    for pid in pids {
        // SAFETY: signal 0 sends nothing; it only asks whether the process is there.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1, "{pid} is gone");
    }
    assert_eq!(s.events(session, json!({"class": "process", "op": "exit"})).len(), 2);
}

#[test]
fn a_program_with_a_sandbox_of_its_own_is_watched_from_outside() {
    let s = Session::new();
    let file = s.path("inside.txt");
    std::fs::write(&file, "kept in").unwrap();
    // The system's own launcher puts `cat` in a sandbox, which is refused inside the reporting sandbox.
    let argv = json!(["/usr/bin/sandbox-exec", "-p", "(version 1)(allow default)(deny network*)", "/bin/cat", file]);
    let session = s.test.tool("observe_start", json!({"argv": argv}))["session"].as_u64().unwrap();
    s.finished(session);
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert_eq!((&verdict["exit_code"], &verdict["verdict"]), (&json!(71), &json!("ok")), "the launcher gives up: {verdict}");
    assert!(s.test.tool("logs", json!({"session": session, "stream": "stderr"}))["text"].as_str().unwrap().contains("Operation not permitted"));

    if !by_events() {
        assert_eq!(s.test.refused("observe_start", json!({"argv": argv, "self_sandboxed": true})), (500, "failed".into()), "without admin rights there is no other way to watch it");
        return;
    }
    let session = s.test.tool("observe_start", json!({"argv": argv, "self_sandboxed": true}))["session"].as_u64().unwrap();
    s.finished(session);
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert_eq!((&verdict["exit_code"], &verdict["verdict"], &verdict["events_lost"]), (&json!(0), &json!("ok"), &json!(0)), "{verdict}");
    assert_eq!(s.test.tool("logs", json!({"session": session}))["text"], "kept in");
    let programs: Vec<Value> = s.events(session, json!({"class": "process", "op": "exec"}));
    let subjects: Vec<&str> = programs.iter().map(|event| event["subject"].as_str().unwrap()).collect();
    assert_eq!(subjects, ["/usr/bin/sandbox-exec", "/bin/cat"]);
    assert_eq!((&programs[0]["args"]["argv"], &programs[1]["args"]["argv"]), (&argv, &json!(["/bin/cat", file])));
    assert_eq!(s.events(session, json!({"class": "file", "op": "read", "subject": file})).len(), 1);
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

    // Everything but one folder.
    let (session, _) = s.run(&actions, json!({"exclude": [s.path("kept"), "/System", "/usr/lib"]}));
    let files = s.events(session, json!({"class": "file"}));
    assert!(files.iter().any(|event| event["subject"] == dropped.as_str()) && !files.iter().any(|event| event["subject"] == kept.as_str()), "{files:?}");
    assert!(!files.iter().any(|event| event["subject"].as_str().unwrap().starts_with("/System/")), "{files:?}");
    assert!(!s.events(session, json!({"class": "process"})).is_empty(), "other classes are untouched");
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "classes": ["files"]})), (400, "bad_input".into()));
}

/// A program that is already running is followed from the moment the session starts, and left running when it stops.
#[test]
fn a_running_program_is_followed_and_left_running() {
    let s = Session::new();
    let file = s.path("after-attach.txt");
    let spawn = |actions: &[String]| std::process::Command::new(TARGET).args(actions).stdout(std::process::Stdio::null()).spawn().unwrap();
    if !by_events() {
        let mut target = spawn(&["sleep=3000".into()]);
        assert_eq!(s.test.refused("observe_start", json!({"pid": target.id()})), (500, "failed".into()), "without admin rights a running program can't be followed");
        let _ = target.kill();
        let _ = target.wait();
        return;
    }
    // It idles first, so the session is in place before it does anything worth seeing.
    let mut target = spawn(&["sleep=2500".into(), format!("write={file}|x"), "spawn".into(), format!("read={file}"), "exit=4".into()]);
    let pid = target.id();
    let started = s.test.tool("observe_start", json!({"pid": pid}));
    assert_eq!((&started["pid"], &started["command"]), (&json!(pid), &json!(format!("(running, pid {pid})"))), "{started}");
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

    // Stopping the session leaves the program alone.
    let mut target = spawn(&["sleep=60000".into()]);
    let session = s.test.tool("observe_start", json!({"pid": target.id()}))["session"].as_u64().unwrap();
    let verdict = s.test.tool("observe_stop", json!({"session": session}));
    assert_eq!((&verdict["state"], &verdict["verdict"], &verdict["signal"]), (&json!("stopped"), &json!("ok"), &Value::Null), "{verdict}");
    assert!(target.try_wait().unwrap().is_none(), "still running after the session let go of it");
    assert_eq!(s.events(session, json!({"class": "process", "op": "exit"})).len(), 0);
    let _ = target.kill();
    let _ = target.wait();

    assert_eq!(s.test.refused("observe_start", json!({"pid": 4_000_000})), (500, "failed".into()), "no such program");
    assert_eq!(s.test.refused("observe_start", json!({"pid": pid, "command": "true"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"pid": pid, "cwd": "/tmp"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"pid": 0})), (400, "bad_input".into()));
}

#[test]
fn mistakes_are_refused_with_a_reason() {
    let s = Session::new();
    assert_eq!(s.test.refused("observe_start", json!({})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "argv": ["true"]})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "syscalls": "some"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "self_sandboxed": "yes"})), (400, "bad_input".into()));
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "cwd": "/no/such/folder"})), (404, "not_found".into()));
    assert_eq!(s.test.refused("observe_start", json!({"argv": ["/no/such/program"]})), (500, "failed".into()));
    assert_eq!(s.test.refused("observe_start", json!({"argv": ["no-such-program-anywhere"]})), (500, "failed".into()));
    // Every system call is more than this source tells.
    assert_eq!(s.test.refused("observe_start", json!({"command": "true", "syscalls": "full"})), (500, "failed".into()));
    assert_eq!(s.test.refused("events", json!({"session": 99})), (404, "not_found".into()));
}

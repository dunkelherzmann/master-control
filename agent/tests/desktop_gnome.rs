//! Screen and input on GNOME under Wayland, through the agent's GNOME Shell extension, in the signed-in session
//! itself: there is no private Wayland desktop to test in. So the test runs only when asked
//! (`MC_GNOME_GUI_TESTS=1`, on a machine whose session may be taken over), acts only on a window of its own, and
//! puts back the text the clipboard held (an empty one can't be set back).
#![cfg(target_os = "linux")]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{decode_base64, desktop_turn, png_pixel, Test};
use serde_json::{json, Value};

fn wanted() -> bool {
    if std::env::var_os("MC_GNOME_GUI_TESTS").is_none() {
        eprintln!("skipped: set MC_GNOME_GUI_TESTS=1 to drive the signed-in GNOME session");
        return false;
    }
    let runtime = format!("/run/user/{}", unsafe { libc::getuid() });
    std::env::set_var("WAYLAND_DISPLAY", std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into()));
    std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={runtime}/bus"));
    }
    std::env::remove_var("GDK_BACKEND");
    true
}

#[test]
fn a_wayland_window_is_listed_pictured_clicked_and_typed_into() {
    if !wanted() {
        return;
    }
    let _turn = desktop_turn();
    let test = Test::new();
    let status = test.tool("status", json!({}));
    assert_eq!(status["capabilities"]["screen"], true, "the GNOME extension must run (linux/scripts/load-gnome-extension.sh): {status}");
    let saved_clipboard = test.tool("clipboard_get", json!({}))["text"].clone();

    let title = format!("mc gnome test {}", std::process::id());
    let state_file = test.home.join("gtk-state.json");
    let mut app = Command::new("python3")
        .args([concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gtk_window.py"), &title, state_file.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("python3 with GTK 4 is needed for this test");
    let state = || -> Value { serde_json::from_slice(&std::fs::read(&state_file).unwrap_or_default()).unwrap_or(Value::Null) };

    // Listed with its place, once it has drawn and joined the accessibility bus.
    test.tool("wait_for", json!({"window": title, "role": "text", "timeout_s": 30}));
    // GNOME places a new window after its first frame: read its place, and the field's, once it has settled.
    std::thread::sleep(Duration::from_millis(500));
    let field = test.tool("find", json!({"window": title, "role": "text"}))["elements"][0].clone();
    let windows = test.tool("windows", json!({}));
    let listed = windows["windows"].as_array().unwrap().iter().find(|window| window["title"] == title.as_str()).cloned().expect("listed");
    assert!(listed["width"].as_u64().unwrap() >= 300 && listed["pid"].as_u64().unwrap() > 0, "{listed}");

    // The whole screen and the window's own area come back as pictures of the right size.
    let screen = test.tool("screenshot", json!({"max_width": 640}));
    assert_eq!(screen["image_width"], 640);
    let picture = test.tool("screenshot", json!({"window": title}));
    assert_eq!((&picture["image_width"], &picture["image_height"]), (&listed["width"], &listed["height"]));
    let (_, size) = png_pixel(&decode_base64(picture["png_base64"].as_str().unwrap()), 0, 0);
    assert_eq!(size, (listed["width"].as_u64().unwrap() as u32, listed["height"].as_u64().unwrap() as u32));

    // The field's place is where the window draws it: a real click there gives it the keyboard, and the keys go in.
    let inside = |element: &Value| {
        let (x, y) = (element["x"].as_i64().unwrap(), element["y"].as_i64().unwrap());
        let (left, top) = (listed["x"].as_i64().unwrap(), listed["y"].as_i64().unwrap());
        x >= left && y >= top && x < left + listed["width"].as_i64().unwrap() && y < top + listed["height"].as_i64().unwrap()
    };
    assert!(inside(&field), "{field} lies in {listed}");
    test.tool("click", json!({"element": field["id"]}));
    test.tool("key", json!({"keys": "ctrl+a", "window": title}));
    test.tool("type_text", json!({"text": "Wayland, typed: café ✓", "window": title}));
    let deadline = Instant::now() + Duration::from_secs(10);
    while state()["text"] != "Wayland, typed: café ✓" {
        assert!(Instant::now() < deadline, "the field holds {}", state()["text"]);
        std::thread::sleep(Duration::from_millis(100));
    }

    // A pointer click on the button by its id presses it.
    let button = test.tool("find", json!({"window": title, "role": "button", "name": "Save it"}))["elements"][0].clone();
    test.tool("click", json!({"element": button["id"]}));
    let deadline = Instant::now() + Duration::from_secs(10);
    while state()["presses"] != 1 {
        assert!(Instant::now() < deadline, "the button was pressed {} times", state()["presses"]);
        std::thread::sleep(Duration::from_millis(100));
    }

    // The clipboard takes text and gives it back; then it holds what it held before.
    test.tool("clipboard_set", json!({"text": "mc gnome clipboard ✓"}));
    assert_eq!(test.tool("clipboard_get", json!({}))["text"], "mc gnome clipboard ✓");
    if let Some(saved) = saved_clipboard.as_str() {
        test.tool("clipboard_set", json!({"text": saved}));
    }

    drop(app.stdin.take());
    let _ = app.wait();
}

#[test]
fn a_wayland_window_is_arranged_and_the_workspaces_are_switched() {
    if !wanted() {
        return;
    }
    let _turn = desktop_turn();
    let test = Test::new();
    let title = format!("mc gnome arrange {}", std::process::id());
    let state_file = test.home.join("gtk-state.json");
    let mut app = Command::new("python3")
        .args([concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gtk_window.py"), &title, state_file.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("python3 with GTK 4 is needed for this test");
    test.tool("wait_for", json!({"window": title, "role": "text", "timeout_s": 30}));
    std::thread::sleep(Duration::from_millis(500));
    let before = test.tool("desktops", json!({}));
    let home = before["current"].clone();
    let act = || {
        let mut said = serde_json::Map::new();
        said.insert("placed".into(), test.tool("arrange", json!({"window": title, "x": 120, "y": 140, "width": 520, "height": 380})));
        said.insert("minimized".into(), test.tool("arrange", json!({"window": title, "state": "minimized"})));
        said.insert("restored".into(), test.tool("arrange", json!({"window": title, "state": "normal"})));
        said.insert("maximized".into(), test.tool("arrange", json!({"window": title, "state": "maximized"})));
        said.insert("back".into(), test.tool("arrange", json!({"window": title, "x": 120, "y": 140, "width": 520, "height": 380})));
        let count = before["desktops"].as_array().unwrap().len();
        if count > 1 {
            let other = if home == 1 { 2 } else { 1 };
            said.insert("switched".into(), test.tool("desktops", json!({"action": "switch", "number": other})));
        }
        Value::Object(said)
    };
    let outcome = act();
    let now = test.tool("desktops", json!({}));
    if now["current"] != home {
        test.tool("desktops", json!({"action": "switch", "number": home}));
    }
    drop(app.stdin.take());
    let _ = app.wait();
    let frame = |answer: &Value| (answer["window"]["x"].clone(), answer["window"]["y"].clone(), answer["window"]["width"].clone(), answer["window"]["height"].clone());
    assert_eq!(frame(&outcome["placed"]), (json!(120), json!(140), json!(520), json!(380)), "{outcome}");
    assert_eq!(outcome["minimized"]["window"]["minimized"], true, "{outcome}");
    assert_eq!(outcome["restored"]["window"]["minimized"], false, "{outcome}");
    assert!(outcome["maximized"]["window"]["width"].as_u64().unwrap() > 520, "{outcome}");
    assert_eq!(frame(&outcome["back"]), (json!(120), json!(140), json!(520), json!(380)), "{outcome}");
    if let Some(switched) = outcome.get("switched") {
        assert_ne!(switched["current"], home, "{outcome}");
    }
    assert_eq!(test.tool("desktops", json!({}))["current"], home, "the first workspace is shown again");
}

/// The instance id in a sandboxed process's `.flatpak-info`, and the app's id.
fn flatpak_of(pid: u64) -> Option<(String, String)> {
    let info = std::fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info")).ok()?;
    let field = |key: &str| info.lines().find_map(|line| line.strip_prefix(key)).map(|value| value.trim().to_string());
    Some((field("instance-id=")?, field("name=")?))
}

/// An app in a Flatpak reaches the accessibility bus through its sandbox's proxy, so the bus names the proxy's
/// process and not the window's. Needs an installed Flatpak app whose window describes itself (any toolkit):
/// `MC_FLATPAK_TREE_APP="APP-ID [ARGUMENTS…]"`, as `flatpak run` takes them. AccessKit apps (egui) answer only with
/// role numbers, which this test covers when the app is one.
#[test]
fn a_flatpak_windows_controls_are_read_with_their_roles() {
    if !wanted() {
        return;
    }
    let Some(command) = std::env::var("MC_FLATPAK_TREE_APP").ok().filter(|command| !command.trim().is_empty()) else {
        eprintln!("skipped: set MC_FLATPAK_TREE_APP to an installed Flatpak app (and its arguments) to read a sandboxed window");
        return;
    };
    let _turn = desktop_turn();
    let test = Test::new();
    let words: Vec<&str> = command.split_whitespace().collect();
    let app_id = words.iter().find(|word| !word.starts_with('-')).copied().unwrap_or_default().to_string();
    let ids = |windows: &Value| windows["windows"].as_array().unwrap().iter().map(|window| window["id"].clone()).collect::<Vec<_>>();
    let before = ids(&test.tool("windows", json!({})));
    let mut runner = Command::new("flatpak").arg("run").args(&words).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("flatpak is needed");
    let deadline = Instant::now() + Duration::from_secs(30);
    let window = loop {
        let windows = test.tool("windows", json!({}));
        let new = windows["windows"].as_array().unwrap().iter().find(|window| {
            !before.contains(&window["id"]) && flatpak_of(window["pid"].as_u64().unwrap_or(0)).is_some_and(|(_, name)| name == app_id)
        });
        if let Some(window) = new {
            break window.clone();
        }
        assert!(Instant::now() < deadline, "no new window of {app_id} in 30 s: {windows}");
        std::thread::sleep(Duration::from_millis(250));
    };
    let (instance, _) = flatpak_of(window["pid"].as_u64().unwrap()).unwrap();
    // Some toolkits describe their windows only a moment after they are first asked.
    let deadline = Instant::now() + Duration::from_secs(20);
    let tree = loop {
        let (status, answer) = test.post("/v1/tree", &json!({"window": window["id"], "max_elements": 300}));
        if status == 200 || Instant::now() > deadline {
            break (status, answer);
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let _ = Command::new("flatpak").args(["kill", &instance]).status();
    let _ = runner.wait();
    // The sandbox goes a moment after it is told to; the next test mustn't find it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && Command::new("flatpak").args(["ps", "--columns=instance"]).output().is_ok_and(|listed| String::from_utf8_lossy(&listed.stdout).lines().any(|line| line.trim() == instance)) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let (status, tree) = tree;
    assert_eq!(status, 200, "tree failed: {tree}");
    let elements = tree["elements"].as_array().unwrap();
    assert!(elements.len() > 3, "{tree}");
    assert!(elements.iter().all(|element| !element["role"].as_str().unwrap_or_default().is_empty()), "an element with no role: {tree}");
    assert!(elements.iter().any(|element| element["role"] == "button"), "no button in {tree}");
}

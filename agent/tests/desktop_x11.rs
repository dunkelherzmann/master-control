//! Screen and input on X11, against a private X server (Xvfb) and a window of the test's own that records every
//! key, button and pointer event that reaches it. The user's own session is never touched.
#![cfg(target_os = "linux")]

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::{decode_base64, desktop_turn, png_pixel, Test};
use serde_json::{json, Value};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, WindowClass};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const SCREEN: (u32, u32) = (1280, 800);
const ORANGE: u32 = 0x00ff_8000;

/// Starts one Xvfb for the whole test program and points DISPLAY at it. A small shell loop stops it when the test
/// program is gone, however that ends.
// The shell is left running on purpose: it is what stops Xvfb once this program has gone.
#[allow(clippy::zombie_processes)]
fn private_display() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let script = "Xvfb -displayfd 1 -screen 0 1280x800x24 -nolisten tcp 2>/dev/null & x=$!; \
                      while kill -0 $PPID 2>/dev/null; do sleep 0.3; done; kill $x";
        let mut child = Command::new("sh")
            .args(["-c", script])
            .stdout(Stdio::piped())
            .spawn()
            .expect("sh starts");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let display: u32 = line.trim().parse().expect("Xvfb is needed for these tests (sudo apt install xvfb) and must print its display number");
        std::env::set_var("DISPLAY", format!(":{display}"));
        std::env::remove_var("WAYLAND_DISPLAY");
        // A session bus of the test's own too, so the accessibility bus the apps join is not the user's.
        let script = "dbus-daemon --session --nofork --print-address=1 2>/dev/null & d=$!; \
                      while kill -0 $PPID 2>/dev/null; do sleep 0.3; done; kill $d";
        let mut child = Command::new("sh").args(["-c", script]).stdout(Stdio::piped()).spawn().expect("sh starts");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        assert!(line.starts_with("unix:"), "dbus-daemon is needed for these tests and must print its address, not {line:?}");
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", line.trim());
    });
}

#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Key { symbol: u32, state: u16, down: bool },
    Button { number: u8, x: i16, y: i16, down: bool },
    Motion { x: i16, y: i16 },
}

/// A window of the test's own, filled with one colour, that notes down the input it receives.
struct Fixture {
    title: String,
    frame: (i16, i16, u16, u16),
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    fn open(title: &str, frame: (i16, i16, u16, u16)) -> Fixture {
        let (connection, screen) = RustConnection::connect(None).expect("the private X display is reachable");
        let screen = connection.setup().roots[screen].clone();
        let window = connection.generate_id().unwrap();
        let mask = EventMask::KEY_PRESS | EventMask::KEY_RELEASE | EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;
        connection
            .create_window(
                screen.root_depth,
                window,
                screen.root,
                frame.0,
                frame.1,
                frame.2,
                frame.3,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &CreateWindowAux::new().background_pixel(ORANGE).event_mask(mask),
            )
            .unwrap();
        connection.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, title.as_bytes()).unwrap();
        connection.map_window(window).unwrap();
        connection.get_input_focus().unwrap().reply().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (noted, stopping) = (seen.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let setup = connection.setup().clone();
            while !stopping.load(Ordering::SeqCst) {
                let Some(event) = connection.poll_for_event().unwrap() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let entry = match event {
                    Event::KeyPress(key) | Event::KeyRelease(key) => {
                        // Looked up the moment the key arrives: a character the layout lacks sits on a borrowed
                        // keycode only while it is being typed.
                        let mapping = connection.get_keyboard_mapping(key.detail, 1).unwrap().reply().unwrap();
                        let shifted = key.state.contains(x11rb::protocol::xproto::KeyButMask::SHIFT) && mapping.keysyms.get(1).is_some_and(|symbol| *symbol != 0);
                        let symbol = mapping.keysyms[usize::from(shifted)];
                        let _ = &setup;
                        Seen::Key { symbol, state: u16::from(key.state), down: key.response_type & 0x7f == x11rb::protocol::xproto::KEY_PRESS_EVENT }
                    }
                    Event::ButtonPress(button) => Seen::Button { number: button.detail, x: button.root_x, y: button.root_y, down: true },
                    Event::ButtonRelease(button) => Seen::Button { number: button.detail, x: button.root_x, y: button.root_y, down: false },
                    Event::MotionNotify(motion) => Seen::Motion { x: motion.root_x, y: motion.root_y },
                    _ => continue,
                };
                noted.lock().unwrap().push(entry);
            }
            let _ = connection.destroy_window(window);
            let _ = connection.get_input_focus().map(|cookie| cookie.reply());
        });
        Fixture { title: title.to_string(), frame, seen, stop, thread: Some(thread) }
    }

    fn centre(&self) -> (i64, i64) {
        (i64::from(self.frame.0) + i64::from(self.frame.2) / 2, i64::from(self.frame.1) + i64::from(self.frame.3) / 2)
    }

    /// What arrived since the last call, once input has been quiet for a moment.
    fn take(&self) -> Vec<Seen> {
        let mut count = usize::MAX;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            std::thread::sleep(Duration::from_millis(120));
            let now = self.seen.lock().unwrap().len();
            if now == count || Instant::now() > deadline {
                break;
            }
            count = now;
        }
        std::mem::take(&mut *self.seen.lock().unwrap())
    }

    fn buttons(&self) -> Vec<(u8, i16, i16, bool)> {
        self.take()
            .into_iter()
            .filter_map(|seen| match seen {
                Seen::Button { number, x, y, down } => Some((number, x, y, down)),
                _ => None,
            })
            .collect()
    }

    /// The symbols of the keys that went down, with the modifier state each arrived with.
    fn keys_down(&self) -> Vec<(u32, u16)> {
        self.take()
            .into_iter()
            .filter_map(|seen| match seen {
                Seen::Key { symbol, state, down: true } => Some((symbol, state)),
                _ => None,
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn start(title: &str, frame: (i16, i16, u16, u16)) -> (std::sync::MutexGuard<'static, ()>, Test, Fixture) {
    let turn = desktop_turn();
    private_display();
    let test = Test::new();
    let fixture = Fixture::open(title, frame);
    (turn, test, fixture)
}

fn find<'a>(windows: &'a Value, title: &str) -> &'a Value {
    windows["windows"].as_array().unwrap().iter().find(|window| window["title"] == title).unwrap_or_else(|| panic!("{title} is not in {windows}"))
}

#[test]
fn status_says_screen_and_input_work_and_windows_lists_the_window() {
    let (_turn, test, fixture) = start("mc test: listing", (100, 80, 400, 300));
    let status = test.tool("status", json!({}));
    assert_eq!((&status["capabilities"]["screen"], &status["capabilities"]["input"], &status["session"]), (&json!(true), &json!(true), &json!("x11")));
    let windows = test.tool("windows", json!({}));
    assert_eq!(windows["screen"], json!({"x": 0, "y": 0, "width": SCREEN.0, "height": SCREEN.1}));
    let window = find(&windows, &fixture.title);
    assert_eq!((&window["x"], &window["y"], &window["width"], &window["height"]), (&json!(100), &json!(80), &json!(400), &json!(300)));
    assert_eq!((&window["focused"], &window["minimized"]), (&json!(false), &json!(false)));
    assert!(window["id"].as_u64().unwrap() > 0);

    let focused = test.tool("focus", json!({"window": fixture.title}));
    assert_eq!((&focused["delivered"], &focused["window"]["focused"]), (&json!(true), &json!(true)));
    assert_eq!(find(&test.tool("windows", json!({})), &fixture.title)["focused"], true);
    // By id, and by a part of the title only this window has.
    test.tool("focus", json!({"window": window["id"]}));
    test.tool("focus", json!({"window": "test: listing"}));
}

#[test]
fn a_screenshot_shows_what_is_on_screen() {
    let (_turn, test, fixture) = start("mc test: picture", (200, 150, 300, 200));
    std::thread::sleep(Duration::from_millis(200));
    let whole = test.tool("screenshot", json!({}));
    assert_eq!((&whole["image_width"], &whole["image_height"], &whole["scale"]), (&json!(SCREEN.0), &json!(SCREEN.1), &json!(1.0)));
    let png = decode_base64(whole["png_base64"].as_str().unwrap());
    assert_eq!(png_pixel(&png, 350, 250), ([255, 128, 0], SCREEN), "inside the window");
    assert_eq!(png_pixel(&png, 50, 50).0, [0, 0, 0], "the empty screen beside it");
    // The edges: the window's first and last pixel, and the screen just outside them.
    assert_eq!(png_pixel(&png, 200, 150).0, [255, 128, 0]);
    assert_eq!(png_pixel(&png, 499, 349).0, [255, 128, 0]);
    assert_eq!(png_pixel(&png, 199, 150).0, [0, 0, 0]);
    assert_eq!(png_pixel(&png, 500, 349).0, [0, 0, 0]);

    let window = test.tool("screenshot", json!({"window": fixture.title}));
    assert_eq!((&window["x"], &window["y"], &window["width"], &window["height"]), (&json!(200), &json!(150), &json!(300), &json!(200)));
    assert_eq!(png_pixel(&decode_base64(window["png_base64"].as_str().unwrap()), 0, 0), ([255, 128, 0], (300, 200)));

    let region = test.tool("screenshot", json!({"region": {"x": 190, "y": 140, "width": 20, "height": 20}}));
    let png = decode_base64(region["png_base64"].as_str().unwrap());
    assert_eq!(png_pixel(&png, 9, 9).0, [0, 0, 0]);
    assert_eq!(png_pixel(&png, 10, 10), ([255, 128, 0], (20, 20)));

    let small = test.tool("screenshot", json!({"max_width": 640}));
    assert_eq!((&small["image_width"], &small["image_height"], &small["scale"]), (&json!(640), &json!(400), &json!(0.5)));
    assert_eq!(png_pixel(&decode_base64(small["png_base64"].as_str().unwrap()), 175, 125), ([255, 128, 0], (640, 400)));

    // A region that hangs off the screen is cut to the part that is on it.
    let cut = test.tool("screenshot", json!({"region": {"x": 1270, "y": 790, "width": 100, "height": 100}}));
    assert_eq!((&cut["width"], &cut["height"]), (&json!(10), &json!(10)));

    let path = test.home.join("shots").join("window.png");
    let saved = test.tool("screenshot", json!({"window": fixture.title, "save_to": path.to_str().unwrap()}));
    assert!(saved["png_base64"].is_null());
    assert_eq!(png_pixel(&std::fs::read(&path).unwrap(), 5, 5), ([255, 128, 0], (300, 200)));

    assert_eq!(test.refused("screenshot", json!({"region": {"x": 5000, "y": 0, "width": 10, "height": 10}})), (400, "bad_input".into()));
    assert_eq!(test.refused("screenshot", json!({"region": {"x": 0, "y": 0, "width": 0, "height": 10}})), (400, "bad_input".into()));
    assert_eq!(test.refused("screenshot", json!({"region": {"x": 0, "y": 0}})), (400, "bad_input".into()));
    assert_eq!(test.refused("screenshot", json!({"window": "no such window anywhere"})), (404, "window_not_found".into()));
    assert_eq!(test.refused("screenshot", json!({"max_width": 0})), (400, "bad_input".into()));
}

#[test]
fn clicks_arrive_where_they_were_aimed() {
    let (_turn, test, fixture) = start("mc test: clicks", (100, 80, 400, 300));
    let (x, y) = fixture.centre();
    let answer = test.tool("click", json!({"x": x, "y": y, "window": fixture.title}));
    assert_eq!((&answer["delivered"], &answer["at"]), (&json!(true), &json!({"x": x, "y": y})));
    assert_eq!(fixture.buttons(), [(1, 300, 230, true), (1, 300, 230, false)]);

    test.tool("double_click", json!({"x": 110, "y": 90}));
    assert_eq!(fixture.buttons(), [(1, 110, 90, true), (1, 110, 90, false), (1, 110, 90, true), (1, 110, 90, false)]);

    test.tool("right_click", json!({"x": 499, "y": 379}));
    assert_eq!(fixture.buttons(), [(3, 499, 379, true), (3, 499, 379, false)]);

    test.tool("click", json!({"x": 120, "y": 100, "button": "middle"}));
    assert_eq!(fixture.buttons(), [(2, 120, 100, true), (2, 120, 100, false)]);

    test.tool("move", json!({"x": 150, "y": 160}));
    assert_eq!(fixture.take().last(), Some(&Seen::Motion { x: 150, y: 160 }));

    test.tool("scroll", json!({"x": x, "y": y, "down": 3}));
    assert_eq!(fixture.buttons().iter().map(|button| (button.0, button.3)).collect::<Vec<_>>(), [(5, true), (5, false), (5, true), (5, false), (5, true), (5, false)]);
    test.tool("scroll", json!({"x": x, "y": y, "down": -1, "right": 2}));
    assert_eq!(fixture.buttons().iter().filter(|button| button.3).map(|button| button.0).collect::<Vec<_>>(), [4, 7, 7]);
}

#[test]
fn a_drag_presses_travels_and_releases() {
    let (_turn, test, fixture) = start("mc test: drag", (100, 80, 400, 300));
    let answer = test.tool("drag", json!({"from_x": 150, "from_y": 120, "to_x": 390, "to_y": 300, "window": fixture.title}));
    assert_eq!(answer["delivered"], true);
    let seen = fixture.take();
    let down = seen.iter().position(|event| *event == Seen::Button { number: 1, x: 150, y: 120, down: true }).expect("pressed at the start");
    let up = seen.iter().position(|event| *event == Seen::Button { number: 1, x: 390, y: 300, down: false }).expect("released at the end");
    let between = seen[down..up].iter().filter(|event| matches!(event, Seen::Motion { .. })).count();
    assert!(between >= 10, "the pointer travelled in steps while the button was down: {between}");
    assert_eq!(seen.iter().filter(|event| matches!(event, Seen::Button { .. })).count(), 2);
}

#[test]
fn positions_are_those_of_the_picture_the_client_has() {
    let (_turn, test, fixture) = start("mc test: scale", (100, 80, 400, 300));
    // Half size: a point in the picture is twice as far along on the desktop.
    let small = test.tool("screenshot", json!({"max_width": 640}));
    test.tool("click", json!({"x": 150, "y": 115}));
    assert_eq!(fixture.buttons()[0], (1, 300, 230, true));
    // A picture of the window alone: positions start at the window's corner.
    let window = test.tool("screenshot", json!({"window": fixture.title}));
    test.tool("click", json!({"x": 10, "y": 20}));
    assert_eq!(fixture.buttons()[0], (1, 110, 100, true));
    // An earlier picture can still be named.
    test.tool("click", json!({"x": 150, "y": 115, "capture": small["capture"]}));
    assert_eq!(fixture.buttons()[0], (1, 300, 230, true));
    assert_ne!(small["capture"], window["capture"]);
    assert_eq!(test.refused("click", json!({"x": 1, "y": 1, "capture": 9999})), (400, "bad_input".into()));
}

#[test]
fn text_arrives_as_typed_whatever_the_layout_has_keys_for() {
    let (_turn, test, fixture) = start("mc test: typing", (100, 80, 400, 300));
    let answer = test.tool("type_text", json!({"text": "Hello, World! 123", "window": fixture.title}));
    assert_eq!((&answer["delivered"], &answer["characters"]), (&json!(true), &json!(17)));
    let typed: String = fixture.keys_down().iter().filter(|(symbol, _)| *symbol < 0xff00).map(|(symbol, _)| char::from_u32(*symbol).unwrap()).collect();
    assert_eq!(typed, "Hello, World! 123");

    // Characters a US keyboard has no key for, and a line break.
    test.tool("type_text", json!({"text": "é€ñ\n", "window": fixture.title}));
    let symbols: Vec<u32> = fixture.keys_down().iter().map(|(symbol, _)| *symbol).collect();
    assert_eq!(symbols, [0xe9, 0x0100_20ac, 0xf1, 0xff0d]);
}

#[test]
fn keys_and_chords_arrive_with_their_modifiers() {
    let (_turn, test, fixture) = start("mc test: keys", (100, 80, 400, 300));
    const CONTROL: u16 = 4;
    const SHIFT: u16 = 1;
    test.tool("key", json!({"keys": "ctrl+a", "window": fixture.title}));
    assert_eq!(fixture.keys_down(), [(0xffe3, 0), (0x61, CONTROL)]);
    test.tool("key", json!({"keys": "ctrl+shift+t", "window": fixture.title}));
    let seen = fixture.take();
    let downs: Vec<(u32, u16)> = seen.iter().filter_map(|event| if let Seen::Key { symbol, state, down: true } = event { Some((*symbol, *state)) } else { None }).collect();
    assert_eq!(downs.len(), 3);
    assert_eq!((downs[0], downs[1].0, downs[2].1), ((0xffe3, 0), 0xffe1, CONTROL | SHIFT));
    assert_eq!(seen.iter().filter(|event| matches!(event, Seen::Key { down: false, .. })).count(), 3, "every key was released");

    test.tool("key", json!({"keys": "enter", "window": fixture.title}));
    test.tool("key", json!({"keys": "f5", "window": fixture.title}));
    test.tool("key", json!({"keys": "down", "window": fixture.title, "times": 3}));
    assert_eq!(fixture.keys_down().iter().map(|(symbol, _)| *symbol).collect::<Vec<_>>(), [0xff0d, 0xffc2, 0xff54, 0xff54, 0xff54]);

    // Nothing is left held: a plain key arrives with no modifier.
    test.tool("key", json!({"keys": "x", "window": fixture.title}));
    assert_eq!(fixture.keys_down(), [(0x78, 0)]);
}

#[test]
fn input_without_a_sure_target_is_refused_and_nothing_is_sent() {
    let (_turn, test, fixture) = start("mc test: refusals", (100, 80, 400, 300));
    let other = Fixture::open("mc test: refusals two", (600, 80, 200, 200));
    assert_eq!(test.refused("type_text", json!({"text": "lost"})), (400, "bad_input".into()));
    assert_eq!(test.refused("key", json!({"keys": "enter"})), (400, "bad_input".into()));
    assert_eq!(test.refused("type_text", json!({"text": "lost", "window": "no such window anywhere"})), (404, "window_not_found".into()));
    assert_eq!(test.refused("type_text", json!({"text": "lost", "window": 4242})), (404, "window_not_found".into()));
    assert_eq!(test.refused("type_text", json!({"text": "lost", "window": "mc test: refusals"})).0, 200, "an exact title wins over a longer one");
    fixture.take();
    assert_eq!(test.refused("type_text", json!({"text": "lost", "window": "refusals"})), (400, "bad_input".into()), "two windows match");
    assert_eq!(test.refused("key", json!({"keys": "ctrl+nonsense", "window": fixture.title})), (400, "bad_input".into()));
    assert_eq!(test.refused("key", json!({"keys": "a", "window": fixture.title, "times": 0})), (400, "bad_input".into()));
    // A point outside the named window, off the screen, or missing.
    assert_eq!(test.refused("click", json!({"x": 700, "y": 100, "window": fixture.title})), (400, "bad_input".into()));
    assert_eq!(test.refused("click", json!({"x": 5000, "y": 100})), (400, "bad_input".into()));
    assert_eq!(test.refused("click", json!({"x": 10})), (400, "bad_input".into()));
    assert_eq!(test.refused("click", json!({"x": 10, "y": 10, "button": "fourth"})), (400, "bad_input".into()));
    assert_eq!(test.refused("scroll", json!({"x": 10, "y": 10})), (400, "bad_input".into()));
    assert_eq!(test.refused("drag", json!({"from_x": 700, "from_y": 100, "to_x": 150, "to_y": 150, "window": fixture.title})), (400, "bad_input".into()));
    assert_eq!(fixture.take().iter().filter(|event| !matches!(event, Seen::Motion { .. })).count(), 0, "none of the refused actions sent anything");
    assert_eq!(other.take().iter().filter(|event| !matches!(event, Seen::Motion { .. })).count(), 0);
}

#[test]
fn a_picture_over_mcp_is_an_image() {
    let (_turn, test, fixture) = start("mc test: mcp", (100, 80, 200, 100));
    let (_, answer) = test.post(
        "/mcp",
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "screenshot", "arguments": {"window": fixture.title}}}),
    );
    let content = answer["result"]["content"].as_array().unwrap();
    assert_eq!((&content[0]["type"], &content[1]["type"], &content[1]["mimeType"]), (&json!("text"), &json!("image"), &json!("image/png")));
    assert_eq!(png_pixel(&decode_base64(content[1]["data"].as_str().unwrap()), 1, 1), ([255, 128, 0], (200, 100)));
    assert!(answer["result"]["structuredContent"]["png_base64"].is_null());
    assert_eq!(answer["result"]["structuredContent"]["width"], 200);
    assert!(!content[0]["text"].as_str().unwrap().contains("png_base64"));
}

#[test]
fn the_clipboard_holds_what_was_set_until_another_program_copies() {
    let (_turn, test, _fixture) = start("mc test: clipboard", (100, 80, 200, 100));
    let set = test.tool("clipboard_set", json!({"text": "héllo €\nsecond line"}));
    assert_eq!((&set["delivered"], &set["characters"]), (&json!(true), &json!(19)));
    assert_eq!(test.tool("clipboard_get", json!({}))["text"], "héllo €\nsecond line");
    // Setting again replaces it; the earlier owner steps down.
    test.tool("clipboard_set", json!({"text": "next"}));
    assert_eq!(test.tool("clipboard_get", json!({}))["text"], "next");
    // Larger than fits in one ordinary request.
    let long = "0123456789abcdef".repeat(20_000);
    test.tool("clipboard_set", json!({"text": long}));
    assert_eq!(test.tool("clipboard_get", json!({}))["text"].as_str().unwrap().len(), 320_000);
    test.tool("clipboard_set", json!({"text": ""}));
    assert_eq!(test.tool("clipboard_get", json!({}))["text"], "");
    assert_eq!(test.refused("clipboard_set", json!({})), (400, "bad_input".into()));

    // Another program copies something that isn't text: the agent's text is gone, and there is no text to give.
    let (other, screen) = RustConnection::connect(None).unwrap();
    let window = other.generate_id().unwrap();
    other
        .create_window(0, window, other.setup().roots[screen].root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new())
        .unwrap();
    let clipboard = other.intern_atom(false, b"CLIPBOARD").unwrap().reply().unwrap().atom;
    other.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME).unwrap();
    other.get_input_focus().unwrap().reply().unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    let answering = std::thread::spawn(move || {
        // Refuses every request, as a program holding only a picture would refuse a request for text. Every one:
        // other programs in the session ask too when the clipboard changes hands.
        while !finished.load(Ordering::SeqCst) {
            let Some(event) = other.poll_for_event().unwrap() else {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            };
            if let Event::SelectionRequest(request) = event {
                let refusal = x11rb::protocol::xproto::SelectionNotifyEvent {
                    response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: request.time,
                    requestor: request.requestor,
                    selection: request.selection,
                    target: request.target,
                    property: x11rb::NONE,
                };
                other.send_event(false, request.requestor, EventMask::NO_EVENT, refusal).unwrap();
                other.flush().unwrap();
            }
        }
    });
    assert_eq!(test.tool("clipboard_get", json!({}))["text"], Value::Null);
    done.store(true, Ordering::SeqCst);
    answering.join().unwrap();
}

fn element<'a>(answer: &'a Value, role: &str, name: &str) -> &'a Value {
    answer["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| element["role"] == role && element["name"].as_str().unwrap().contains(name))
        .unwrap_or_else(|| panic!("no {role} named {name} in {answer}"))
}

/// A real GTK dialog with a label, a text field and two buttons. It prints the field's text when OK is pressed.
fn dialog(title: &str) -> std::process::Child {
    Command::new("zenity")
        .args(["--entry", "--title", title, "--text", "Your name", "--entry-text", "start"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("zenity is needed for these tests (sudo apt install zenity)")
}

#[test]
fn the_tree_names_what_is_in_a_window_and_elements_can_be_clicked() {
    let turn = desktop_turn();
    private_display();
    let test = Test::new();
    let title = "mc test: tree";
    let mut app = dialog(title);
    // The app has to start, draw its window and join the accessibility bus.
    let waited = test.tool("wait_for", json!({"window": title, "role": "button", "name": "OK", "timeout_s": 60}));
    assert_eq!(waited["elements"][0]["name"], "OK");

    let tree = test.tool("tree", json!({"window": title}));
    assert_eq!((&tree["truncated"], &tree["window"]["title"], &tree["elements"][0]["depth"]), (&json!(false), &json!(title), &json!(0)));
    let field = element(&tree, "text", "");
    assert_eq!(field["value"], "start");
    assert!(tree["elements"].as_array().unwrap().iter().all(|element| element["enabled"].is_null()), "nothing in the dialog is disabled: {tree}");
    element(&tree, "label", "Your name");
    let ok = element(&tree, "button", "OK");
    let cancel = element(&tree, "button", "Cancel");
    assert!(ok["width"].as_u64().unwrap() > 10 && ok["x"] != cancel["x"], "the buttons have places of their own: {ok} {cancel}");
    assert!(ok["enabled"].is_null(), "only said when unusual: {ok}");

    assert_eq!(test.tool("find", json!({"window": title, "role": "button"}))["elements"].as_array().unwrap().len(), 2);
    assert_eq!(test.tool("find", json!({"window": title, "name": "CANCEL"}))["elements"][0]["role"], "button");
    assert_eq!(test.tool("find", json!({"window": title, "value": "STAR"}))["elements"][0]["role"], "text");
    assert_eq!(test.tool("find", json!({"window": title, "role": "check_box"}))["elements"], json!([]));
    let limited = test.tool("tree", json!({"window": title, "max_elements": 2}));
    assert_eq!((limited["elements"].as_array().unwrap().len(), &limited["truncated"]), (2, &json!(true)));

    // Click the field by its id, replace its text, and read it back from the tree.
    test.tool("click", json!({"element": field["id"]}));
    test.tool("key", json!({"keys": "ctrl+a", "window": title}));
    test.tool("type_text", json!({"text": "typed by the test é", "window": title}));
    let after = test.tool("find", json!({"window": title, "role": "text"}));
    assert_eq!(after["elements"][0]["value"], "typed by the test é");

    assert_eq!(test.refused("click", json!({"element": "9999.1"})), (409, "element_gone".into()));
    assert_eq!(test.refused("click", json!({"element": "the button"})), (400, "bad_input".into()));
    assert_eq!(test.refused("click", json!({"element": ok["id"], "x": 1, "y": 1})), (400, "bad_input".into()));
    assert_eq!(test.refused("find", json!({"window": title})), (400, "bad_input".into()));
    let began = Instant::now();
    assert_eq!(test.refused("wait_for", json!({"window": title, "role": "check_box", "timeout_s": 1})), (408, "timed_out".into()));
    assert!(began.elapsed() >= Duration::from_secs(1) && began.elapsed() < Duration::from_secs(10));
    assert_eq!(test.refused("wait_for", json!({"window": title, "gone": true, "timeout_s": 1})), (408, "timed_out".into()));

    // Pressing OK by its id ends the dialog, which prints what the field held: the click landed on the button.
    test.tool("click", json!({"element": ok["id"]}));
    let output = app.wait_with_output_after(Duration::from_secs(15));
    assert_eq!(output.trim(), "typed by the test é");
    assert_eq!(test.tool("wait_for", json!({"window": title, "gone": true}))["gone"], true);
    assert_eq!(test.refused("click", json!({"element": cancel["id"]})), (409, "element_gone".into()), "the window closed");
    drop(turn);
}

#[test]
fn a_window_that_says_nothing_about_its_contents_is_reported_as_such() {
    let (_turn, test, fixture) = start("mc test: plain", (100, 80, 200, 100));
    assert_eq!(test.refused("tree", json!({"window": fixture.title})), (501, "not_supported".into()));
    assert_eq!(test.refused("tree", json!({"window": "no such window anywhere"})), (404, "window_not_found".into()));
    assert_eq!(test.tool("wait_for", json!({"window": fixture.title}))["window"]["title"], "mc test: plain");
    assert_eq!(test.refused("wait_for", json!({"window": "mc test: never", "timeout_s": 1})), (408, "timed_out".into()));
}

trait WaitWithin {
    fn wait_with_output_after(&mut self, limit: Duration) -> String;
}

impl WaitWithin for std::process::Child {
    /// The program's output once it has ended; it is stopped, and the test fails, if it hasn't by then.
    fn wait_with_output_after(&mut self, limit: Duration) -> String {
        let deadline = Instant::now() + limit;
        while self.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = self.kill();
                panic!("the program was still running after {limit:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut output = String::new();
        std::io::Read::read_to_string(self.stdout.as_mut().unwrap(), &mut output).unwrap();
        output
    }
}

/// Acting on elements themselves, in a real GTK window: the pointer stays where it is and the keyboard goes nowhere.
/// The window writes down what happens to its controls, so the test doesn't have to take the tree's word for it.
#[test]
fn elements_are_pressed_set_ticked_opened_and_selected_without_the_pointer() {
    let turn = desktop_turn();
    private_display();
    let test = Test::new();
    let title = "mc test: actions";
    let state_file = test.home.join("gtk-state.json");
    let mut app = Command::new("python3")
        .args([concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gtk_window.py"), title, state_file.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("python3 with GTK 4 is needed for this test (sudo apt install python3-gi gir1.2-gtk-4.0)");
    let state = || -> Value { serde_json::from_slice(&std::fs::read(&state_file).unwrap_or_default()).unwrap_or(Value::Null) };
    // What a control does shows up in the window's own notes a moment after it is asked.
    let settled = |field: &str, wanted: Value| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while state()[field] != wanted {
            assert!(Instant::now() < deadline, "{field} never became {wanted}: {}", state());
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    test.tool("wait_for", json!({"window": title, "role": "button", "name": "Save it", "timeout_s": 60}));
    let tree = test.tool("tree", json!({"window": title}));
    let pointer = test.tool("move", json!({"x": 5, "y": 5}));
    assert_eq!(pointer["delivered"], true);

    let button = element(&tree, "button", "Save it");
    let pressed = test.tool("press", json!({"element": button["id"]}));
    assert_eq!((&pressed["role"], &pressed["name"]), (&json!("button"), &json!("Save it")));
    // GTK shows a press for a moment, and takes no second one until the first is over.
    settled("presses", json!(1));
    test.tool("press", json!({"element": button["id"]}));
    settled("presses", json!(2));
    assert_eq!(test.tool("find", json!({"window": title, "name": "pressed"}))["elements"][0]["name"], "pressed 2");

    let field = element(&tree, "text", "");
    assert_eq!(test.tool("set_value", json!({"element": field["id"], "value": "set by the test é"}))["value"], "set by the test é");
    settled("text", json!("set by the test é"));

    let switch = tree["elements"].as_array().unwrap().iter().find(|element| matches!(element["role"].as_str(), Some("switch" | "toggle_button"))).unwrap_or_else(|| panic!("no switch in {tree}"));
    assert_eq!(test.tool("toggle", json!({"element": switch["id"], "on": true}))["on"], true, "{switch}");
    settled("switched", json!(true));
    assert_eq!(test.tool("toggle", json!({"element": switch["id"], "on": true}))["on"], true, "already so: left alone");
    assert_eq!(test.tool("toggle", json!({"element": switch["id"]}))["on"], false, "flipped");
    settled("switched", json!(false));
    // GTK 4's check box gives assistive technology nothing to tick it with: that is said, with the way round it,
    // and when it is already as wanted there is nothing to refuse.
    // Typed text goes into the element that is named, which is given the keyboard first.
    let field_now = test.tool("find", json!({"window": title, "role": "text"}))["elements"][0]["id"].clone();
    test.tool("key", json!({"element": field_now, "keys": "ctrl+a"}));
    let typed = test.tool("type_text", json!({"element": field_now, "text": "typed into it"}));
    assert_eq!((&typed["delivered"], &typed["window"]["title"]), (&json!(true), &json!(title)));
    settled("text", json!("typed into it"));
    let check = element(&tree, "check_box", "Enable thing");
    assert_eq!(test.tool("toggle", json!({"element": check["id"], "on": true}))["on"], true, "already ticked");
    let (status, refusal) = test.post("/v1/toggle", &json!({"element": check["id"], "on": false}));
    if status == 200 {
        settled("ticked", json!(false));
    } else {
        assert_eq!((status, refusal["error"]["code"].as_str()), (501, Some("not_supported")), "{refusal}");
        assert!(refusal["error"]["message"].as_str().unwrap().contains("click it by its id"), "{refusal}");
        test.tool("click", json!({"element": check["id"]}));
        settled("ticked", json!(false));
    }

    // GTK names an expander after its title and what is in it, and the name changes as that is shown and hidden:
    // it is found afresh each time, as a client would after anything changed.
    let more = || test.tool("find", json!({"window": title, "role": "button", "name": "More"}))["elements"][0]["id"].clone();
    assert_eq!(test.tool("expand", json!({"element": more()}))["expanded"], true);
    settled("expanded", json!(true));
    assert_eq!(test.tool("expand", json!({"element": more(), "expanded": false}))["expanded"], false);
    settled("expanded", json!(false));

    let two = tree["elements"].as_array().unwrap().iter().find(|element| element["role"] == "list_item" && element["name"] == "Two").or_else(|| {
        // Some toolkit versions name only the label inside the row: its row is the element before it.
        let at = tree["elements"].as_array().unwrap().iter().position(|element| element["name"] == "Two")?;
        tree["elements"].as_array().unwrap().get(at - 1)
    });
    let two = two.unwrap_or_else(|| panic!("no row Two in {tree}"));
    test.tool("select", json!({"element": two["id"]}));
    settled("selected", json!("Two"));

    // Nothing went through the pointer: it is where it was left.
    assert_eq!(test.tool("windows", json!({}))["windows"].as_array().unwrap().iter().filter(|window| window["title"] == title).count(), 1);
    let label = test.tool("find", json!({"window": title, "name": "pressed"}));
    assert_eq!(test.refused("set_value", json!({"element": label["elements"][0]["id"], "value": "x"})), (501, "not_supported".into()), "a label holds nothing to set");
    assert_eq!(test.refused("press", json!({"element": element(&tree, "label", "A label")["id"]})), (409, "element_gone".into()), "the label says something else now");
    assert_eq!(test.refused("set_value", json!({"element": field["id"]})), (400, "bad_input".into()));
    assert_eq!(test.refused("toggle", json!({"element": check["id"], "on": "yes"})), (400, "bad_input".into()));
    assert_eq!(test.refused("press", json!({})), (400, "bad_input".into()));
    // Input in the background is built for macOS only: here it is refused, not sent for real.
    assert_eq!(test.refused("type_text", json!({"text": "x", "window": title, "background": true})), (501, "not_supported".into()));
    assert_eq!(test.refused("key", json!({"keys": "enter", "window": title, "background": true})), (501, "not_supported".into()));
    assert_eq!(test.refused("press", json!({"element": "9999.1"})), (409, "element_gone".into()));
    drop(app.stdin.take());
    let _ = app.wait();
    assert_eq!(test.refused("press", json!({"element": button["id"]})), (409, "element_gone".into()), "the window closed");
    drop(turn);
}

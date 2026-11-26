//! Screen and input on Windows, against a window of the test's own that records every key, button and wheel message
//! that reaches it. These tests need the signed-in desktop session: over plain SSH there is no screen, and they say so.
#![cfg(windows)]

mod common;

use std::ptr::null_mut;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{decode_base64, desktop_turn, png_pixel, Test};
use serde_json::{json, Value};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, CreateSolidBrush};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowRect, IsIconic, IsZoomed, SendMessageW, DispatchMessageW, GetMessageW, PostMessageW, PostQuitMessage, RegisterClassW, TranslateMessage, MSG,
    WM_CHAR, WM_CLOSE, WM_COMMAND, WM_DESTROY, WS_CHILD, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Char(char),
    KeyDown(u32),
    KeyUp(u32),
    Button { what: &'static str, x: i32, y: i32 },
    Wheel { delta: i32, sideways: bool },
    /// A control of the window told it something: for a button, that it was pressed.
    Command(u32),
    Move { x: i32, y: i32 },
}

/// Filled by the window procedure. One list is enough: the tests take turns.
static SEEN: Mutex<Vec<Seen>> = Mutex::new(Vec::new());

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let on_screen = || {
        // The position comes in the window's own pixels, as two signed 16-bit numbers.
        let mut point = POINT { x: i32::from(lparam as u16 as i16), y: i32::from((lparam >> 16) as u16 as i16) };
        unsafe { ClientToScreen(window, &mut point) };
        (point.x, point.y)
    };
    let button = |what: &'static str| {
        let (x, y) = on_screen();
        Some(Seen::Button { what, x, y })
    };
    let entry = match message {
        WM_CHAR => char::from_u32(wparam as u32).map(Seen::Char),
        WM_KEYDOWN | WM_SYSKEYDOWN => Some(Seen::KeyDown(wparam as u32)),
        WM_KEYUP | WM_SYSKEYUP => Some(Seen::KeyUp(wparam as u32)),
        WM_LBUTTONDOWN => button("left down"),
        WM_LBUTTONUP => button("left up"),
        WM_RBUTTONDOWN => button("right down"),
        WM_RBUTTONUP => button("right up"),
        WM_MBUTTONDOWN => button("middle down"),
        WM_MBUTTONUP => button("middle up"),
        WM_COMMAND => Some(Seen::Command(wparam as u32 & 0xffff)),
        WM_MOUSEWHEEL => Some(Seen::Wheel { delta: i32::from((wparam >> 16) as u16 as i16), sideways: false }),
        WM_MOUSEHWHEEL => Some(Seen::Wheel { delta: i32::from((wparam >> 16) as u16 as i16), sideways: true }),
        WM_MOUSEMOVE => {
            let (x, y) = on_screen();
            Some(Seen::Move { x, y })
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            None
        }
        _ => None,
    };
    if let Some(entry) = entry {
        SEEN.lock().unwrap().push(entry);
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// A borderless window, always on top, filled with one colour. Borderless, so its frame is exactly what was asked.
struct Fixture {
    title: String,
    window: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    fn open(title: &str, frame: (i32, i32, i32, i32)) -> Fixture {
        Fixture::open_with(title, frame, false)
    }

    /// With `controls`, the window holds a button (101), a text field (102) and a label.
    fn open_with(title: &str, frame: (i32, i32, i32, i32), controls: bool) -> Fixture {
        let (sender, receiver) = mpsc::channel();
        let name = wide(title);
        let thread = std::thread::spawn(move || unsafe {
            let instance = GetModuleHandleW(null_mut());
            let class = wide("MasterControlTestWindow");
            let mut description: WNDCLASSW = std::mem::zeroed();
            description.lpfnWndProc = Some(procedure);
            description.hInstance = instance;
            // Orange, as 0x00bbggrr.
            description.hbrBackground = CreateSolidBrush(0x0000_80ff);
            description.lpszClassName = class.as_ptr();
            // Fails the second time, because the class is already there: that is fine.
            RegisterClassW(&description);
            let window = CreateWindowExW(
                WS_EX_TOPMOST,
                class.as_ptr(),
                name.as_ptr(),
                WS_POPUP | WS_VISIBLE,
                frame.0,
                frame.1,
                frame.2,
                frame.3,
                null_mut(),
                null_mut(),
                instance,
                null_mut(),
            );
            if controls {
                let child = |class: &str, text: &str, style: u32, place: (i32, i32, i32, i32), id: usize| {
                    CreateWindowExW(0, wide(class).as_ptr(), wide(text).as_ptr(), WS_CHILD | WS_VISIBLE | style, place.0, place.1, place.2, place.3, window, id as _, instance, null_mut())
                };
                child("BUTTON", "Save it", 0, (20, 20, 120, 30), 101);
                // A border, and scrolling sideways when the text is longer than the field.
                child("EDIT", "first text", 0x0080_0000 | 0x80, (20, 70, 200, 26), 102);
                child("STATIC", "A label", 0, (20, 110, 200, 20), 103);
                // A check box that ticks itself when pressed, and a drop-down list of three.
                child("BUTTON", "Enable thing", 0x3, (20, 140, 200, 24), 104);
                let list = child("COMBOBOX", "", 0x3 | 0x0020_0000, (240, 20, 160, 200), 105);
                for item in ["One", "Two", "Three"] {
                    SendMessageW(list, 0x0143, 0, wide(item).as_ptr() as LPARAM);
                }
                SendMessageW(list, 0x014E, 0, 0);
            }
            sender.send(window as isize).unwrap();
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        });
        let window = receiver.recv().unwrap();
        assert_ne!(window, 0, "the test window couldn't be made: {}", std::io::Error::last_os_error());
        std::thread::sleep(Duration::from_millis(400));
        SEEN.lock().unwrap().clear();
        Fixture { title: title.to_string(), window, thread: Some(thread) }
    }

    /// What arrived since the last call, once input has been quiet for a moment.
    fn take(&self) -> Vec<Seen> {
        let mut count = usize::MAX;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            std::thread::sleep(Duration::from_millis(150));
            let now = SEEN.lock().unwrap().len();
            if now == count || Instant::now() > deadline {
                break;
            }
            count = now;
        }
        std::mem::take(&mut *SEEN.lock().unwrap())
    }

    fn buttons(&self) -> Vec<(&'static str, i32, i32)> {
        self.take()
            .into_iter()
            .filter_map(|seen| match seen {
                Seen::Button { what, x, y } => Some((what, x, y)),
                _ => None,
            })
            .collect()
    }

    fn keys_down(&self) -> Vec<u32> {
        self.take().into_iter().filter_map(|seen| if let Seen::KeyDown(key) = seen { Some(key) } else { None }).collect()
    }

    fn typed(&self) -> String {
        self.take().into_iter().filter_map(|seen| if let Seen::Char(character) = seen { Some(character) } else { None }).collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe { PostMessageW(self.window as HWND, WM_CLOSE, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn start(title: &str) -> (std::sync::MutexGuard<'static, ()>, Test, Fixture) {
    let turn = desktop_turn();
    let test = Test::new();
    // Asked before the window is made: the agent's first look at the desktop switches the process to real pixels.
    let status = test.tool("status", json!({}));
    assert_eq!(
        status["capabilities"]["screen"], true,
        "these tests need the signed-in desktop session; run them there (scripts/check.sh does), not over plain SSH"
    );
    let fixture = Fixture::open(title, (200, 150, 500, 350));
    (turn, test, fixture)
}

fn find<'a>(windows: &'a Value, title: &str) -> &'a Value {
    windows["windows"].as_array().unwrap().iter().find(|window| window["title"] == title).unwrap_or_else(|| panic!("{title} is not in {windows}"))
}

#[test]
fn windows_lists_the_window_and_focus_brings_it_forward() {
    let (_turn, test, fixture) = start("mc test: listing");
    let windows = test.tool("windows", json!({}));
    assert!(windows["screen"]["width"].as_u64().unwrap() >= 800);
    let window = find(&windows, &fixture.title);
    assert_eq!((&window["x"], &window["y"], &window["width"], &window["height"]), (&json!(200), &json!(150), &json!(500), &json!(350)));
    assert_eq!((window["pid"].as_u64(), &window["minimized"]), (Some(u64::from(std::process::id())), &json!(false)));
    assert!(window["app"].as_str().unwrap().to_lowercase().ends_with(".exe"));
    assert_eq!(window["id"].as_u64(), Some(fixture.window as u64));

    let focused = test.tool("focus", json!({"window": fixture.title}));
    assert_eq!((&focused["delivered"], &focused["window"]["focused"]), (&json!(true), &json!(true)));
    let again = test.tool("windows", json!({}));
    assert_eq!(find(&again, &fixture.title)["focused"], true);
    assert_eq!(again["windows"].as_array().unwrap().iter().filter(|window| window["focused"] == true).count(), 1);
    test.tool("focus", json!({"window": window["id"]}));
    test.tool("focus", json!({"window": "test: listing"}));
}

#[test]
fn a_screenshot_shows_what_is_on_screen() {
    let (_turn, test, fixture) = start("mc test: picture");
    let whole = test.tool("screenshot", json!({"full_size": true}));
    let size = (whole["image_width"].as_u64().unwrap() as u32, whole["image_height"].as_u64().unwrap() as u32);
    assert_eq!((whole["width"].as_u64(), &whole["scale"]), (Some(u64::from(size.0)), &json!(1.0)));
    let (left, top) = (whole["x"].as_i64().unwrap(), whole["y"].as_i64().unwrap());
    let png = decode_base64(whole["png_base64"].as_str().unwrap());
    // The window's corners, wherever the desktop's own corner is.
    for (x, y) in [(200, 150), (699, 499), (450, 325)] {
        assert_eq!(png_pixel(&png, (x - left) as u32, (y - top) as u32), ([255, 128, 0], size), "{x},{y}");
    }

    let window = test.tool("screenshot", json!({"window": fixture.title}));
    assert_eq!((&window["x"], &window["y"], &window["width"], &window["height"]), (&json!(200), &json!(150), &json!(500), &json!(350)));
    let png = decode_base64(window["png_base64"].as_str().unwrap());
    assert_eq!(png_pixel(&png, 0, 0), ([255, 128, 0], (500, 350)));
    assert_eq!(png_pixel(&png, 499, 349).0, [255, 128, 0]);

    let region = test.tool("screenshot", json!({"region": {"x": 300, "y": 200, "width": 20, "height": 10}}));
    assert_eq!(png_pixel(&decode_base64(region["png_base64"].as_str().unwrap()), 19, 9), ([255, 128, 0], (20, 10)));

    let small = test.tool("screenshot", json!({"window": fixture.title, "max_width": 250}));
    assert_eq!((&small["image_width"], &small["image_height"], &small["scale"]), (&json!(250), &json!(175), &json!(0.5)));

    let path = test.home.join("shots").join("window.png");
    let saved = test.tool("screenshot", json!({"window": fixture.title, "save_to": path.to_str().unwrap()}));
    assert!(saved["png_base64"].is_null());
    assert_eq!(png_pixel(&std::fs::read(&path).unwrap(), 5, 5), ([255, 128, 0], (500, 350)));

    assert_eq!(test.refused("screenshot", json!({"region": {"x": 900000, "y": 0, "width": 10, "height": 10}})), (400, "bad_input".into()));
    assert_eq!(test.refused("screenshot", json!({"window": "no such window anywhere"})), (404, "window_not_found".into()));
}

#[test]
fn clicks_arrive_where_they_were_aimed() {
    let (_turn, test, fixture) = start("mc test: clicks");
    let answer = test.tool("click", json!({"x": 450, "y": 325, "window": fixture.title}));
    assert_eq!((&answer["delivered"], &answer["at"]), (&json!(true), &json!({"x": 450, "y": 325})));
    assert_eq!(fixture.buttons(), [("left down", 450, 325), ("left up", 450, 325)]);

    test.tool("double_click", json!({"x": 210, "y": 160, "window": fixture.title}));
    assert_eq!(fixture.buttons(), [("left down", 210, 160), ("left up", 210, 160), ("left down", 210, 160), ("left up", 210, 160)]);

    test.tool("right_click", json!({"x": 699, "y": 499, "window": fixture.title}));
    assert_eq!(fixture.buttons(), [("right down", 699, 499), ("right up", 699, 499)]);

    test.tool("click", json!({"x": 220, "y": 170, "button": "middle", "window": fixture.title}));
    assert_eq!(fixture.buttons(), [("middle down", 220, 170), ("middle up", 220, 170)]);

    test.tool("move", json!({"x": 250, "y": 260}));
    assert_eq!(fixture.take().iter().rev().find(|seen| matches!(seen, Seen::Move { .. })), Some(&Seen::Move { x: 250, y: 260 }));

    test.tool("scroll", json!({"x": 450, "y": 325, "down": 3, "window": fixture.title}));
    assert_eq!(fixture.take().into_iter().filter(|seen| matches!(seen, Seen::Wheel { .. })).collect::<Vec<_>>(), [Seen::Wheel { delta: -360, sideways: false }]);
    test.tool("scroll", json!({"x": 450, "y": 325, "down": -1, "right": 2, "window": fixture.title}));
    assert_eq!(
        fixture.take().into_iter().filter(|seen| matches!(seen, Seen::Wheel { .. })).collect::<Vec<_>>(),
        [Seen::Wheel { delta: 120, sideways: false }, Seen::Wheel { delta: 240, sideways: true }]
    );
}

#[test]
fn a_click_with_keys_held_has_them_down_around_the_button_only() {
    let (_turn, test, fixture) = start("mc test: held click");
    const CONTROL: u32 = 0x11;
    const SHIFT: u32 = 0x10;
    let answer = test.tool("click", json!({"x": 450, "y": 325, "window": fixture.title, "hold": "ctrl"}));
    assert_eq!(answer["held"], "ctrl");
    let seen: Vec<Seen> = fixture.take().into_iter().filter(|event| !matches!(event, Seen::Move { .. })).collect();
    assert_eq!(seen, [Seen::KeyDown(CONTROL), Seen::Button { what: "left down", x: 450, y: 325 }, Seen::Button { what: "left up", x: 450, y: 325 }, Seen::KeyUp(CONTROL)]);
    test.tool("click", json!({"x": 450, "y": 325, "window": fixture.title, "hold": "ctrl+shift"}));
    assert_eq!(fixture.keys_down(), [CONTROL, SHIFT]);
    // Nothing is left held: a plain key types its plain character.
    test.tool("key", json!({"keys": "x", "window": fixture.title}));
    assert_eq!(fixture.typed(), "x");
    assert_eq!(test.refused("click", json!({"x": 450, "y": 325, "hold": "a"})), (400, "bad_input".into()));
}

/// Where the system itself says the window is, and whether it is minimized or maximized: not the agent's reading.
fn system_says(fixture: &Fixture) -> ((i32, i32, i32, i32), bool, bool) {
    let handle = fixture.window as HWND;
    let mut frame = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetWindowRect(handle, &mut frame) };
    let (minimized, maximized) = unsafe { (IsIconic(handle) != 0, IsZoomed(handle) != 0) };
    ((frame.left, frame.top, frame.right - frame.left, frame.bottom - frame.top), minimized, maximized)
}

#[test]
fn a_window_is_moved_sized_minimized_maximized_and_restored() {
    let (_turn, test, fixture) = start("mc test: arrange");
    let placed = test.tool("arrange", json!({"window": fixture.title, "x": 300, "y": 220, "width": 420, "height": 260}));
    assert_eq!((&placed["window"]["x"], &placed["window"]["y"], &placed["window"]["width"], &placed["window"]["height"]), (&json!(300), &json!(220), &json!(420), &json!(260)), "{placed}");
    assert_eq!(system_says(&fixture), ((300, 220, 420, 260), false, false));
    test.tool("arrange", json!({"window": fixture.title, "y": 240}));
    assert_eq!(system_says(&fixture).0, (300, 240, 420, 260), "what isn't given stays");

    let minimized = test.tool("arrange", json!({"window": fixture.title, "state": "minimized"}));
    assert_eq!((minimized["window"]["minimized"].clone(), system_says(&fixture).1), (json!(true), true), "{minimized}");
    let restored = test.tool("arrange", json!({"window": fixture.title, "state": "normal"}));
    assert_eq!((restored["window"]["minimized"].clone(), system_says(&fixture)), (json!(false), ((300, 240, 420, 260), false, false)), "{restored}");

    let maximized = test.tool("arrange", json!({"window": fixture.title, "state": "maximized"}));
    let screen = test.tool("windows", json!({}))["screen"].clone();
    assert!(system_says(&fixture).2, "the system says maximized: {maximized}");
    assert!(maximized["window"]["width"].as_i64().unwrap() >= screen["width"].as_i64().unwrap() / 2, "{maximized}");
    // A maximized window given a place is restored first.
    test.tool("arrange", json!({"window": fixture.title, "x": 250, "y": 200, "width": 400, "height": 300}));
    assert_eq!(system_says(&fixture), ((250, 200, 400, 300), false, false));

    assert_eq!(test.refused("arrange", json!({"window": fixture.title})), (400, "bad_input".into()));
    assert_eq!(test.refused("arrange", json!({"window": fixture.title, "state": "minimized", "x": 5})), (400, "bad_input".into()));
    assert_eq!(test.refused("arrange", json!({"window": "no such window anywhere", "x": 5})), (404, "window_not_found".into()));
}

#[test]
fn virtual_desktops_are_created_switched_and_removed_and_the_first_is_shown_again() {
    let (_turn, test, _fixture) = start("mc test: desktops");
    let before = test.tool("desktops", json!({}));
    let count = before["desktops"].as_array().unwrap().len();
    let home = before["current"].as_u64().expect("a desktop is shown");
    let outcome = (|| {
        let mut said = serde_json::Map::new();
        said.insert("created".into(), test.tool("desktops", json!({"action": "create"})));
        said.insert("home".into(), test.tool("desktops", json!({"action": "switch", "number": home})));
        said.insert("new".into(), test.tool("desktops", json!({"action": "switch", "number": count + 1})));
        said.insert("removed".into(), test.tool("desktops", json!({"action": "remove", "number": count + 1})));
        Value::Object(said)
    })();
    // Whatever happened, the desktop that was shown is shown again.
    let now = test.tool("desktops", json!({}));
    if now["current"] != home {
        test.tool("desktops", json!({"action": "switch", "number": home}));
    }
    assert_eq!(outcome["created"]["desktops"].as_array().unwrap().len(), count + 1, "{outcome}");
    assert_eq!(outcome["home"]["current"], home, "{outcome}");
    assert_eq!(outcome["new"]["current"], count + 1, "{outcome}");
    assert_eq!(outcome["removed"]["desktops"].as_array().unwrap().len(), count, "{outcome}");
    assert_eq!(test.tool("desktops", json!({}))["current"], home);
    assert_eq!(test.refused("desktops", json!({"action": "switch", "number": 0})), (400, "bad_input".into()));
    assert_eq!(test.refused("desktops", json!({"action": "remove", "number": count + 5})), (400, "bad_input".into()));
}

/// A Start menu shortcut of the test's own, removed when the test ends.
struct Shortcut(std::path::PathBuf);

impl Drop for Shortcut {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn an_app_is_launched_by_its_start_menu_name_and_its_window_awaited() {
    let (_turn, test, _fixture) = start("mc test: launcher");
    let name = format!("MC Test App {}", std::process::id());
    let programs = std::path::PathBuf::from(std::env::var("APPDATA").unwrap()).join("Microsoft\\Windows\\Start Menu\\Programs");
    let shortcut = Shortcut(programs.join(format!("{name}.lnk")));
    // A window that is PowerShell's: a form titled for the test, which the test closes.
    let form = "Add-Type -AssemblyName System.Windows.Forms; $f = New-Object Windows.Forms.Form; $f.Text = 'mc test: launched'; [void]$f.ShowDialog()";
    let made = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &format!("$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath = 'powershell.exe'; $s.Arguments = '-NoProfile -WindowStyle Hidden -Command \"{}\"'; $s.Save()", shortcut.0.display(), form.replace('\'', "''"))])
        .status()
        .unwrap();
    assert!(made.success() && shortcut.0.exists());

    // The Start menu takes a few seconds to list a new shortcut.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut listed = test.tool("apps", json!({"name": name}));
    while listed["apps"].as_array().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        listed = test.tool("apps", json!({"name": name}));
    }
    assert_eq!(listed["apps"].as_array().unwrap().len(), 1, "{listed}");
    assert!(test.tool("apps", json!({}))["apps"].as_array().unwrap().len() > 5, "the Start menu lists the system's apps too");
    let launched = test.tool("launch", json!({"name": name.to_lowercase(), "wait_s": 60}));
    // The first new window may be the terminal PowerShell starts in before it hides; the form is the shortcut's own.
    assert_eq!(launched["app"]["name"], json!(name), "{launched}");
    assert!(launched["window"]["id"].is_u64(), "a new window opened: {launched}");
    let form = test.tool("wait_for", json!({"window": "mc test: launched", "timeout_s": 60}));
    let window = form["window"]["id"].as_u64().unwrap() as usize as HWND;
    unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    assert_eq!(test.tool("wait_for", json!({"window": "mc test: launched", "gone": true}))["gone"], true);
    assert_eq!(test.refused("launch", json!({"name": "no such app anywhere at all"})), (404, "not_found".into()));
}

#[test]
fn a_drag_presses_travels_and_releases() {
    let (_turn, test, fixture) = start("mc test: drag");
    let answer = test.tool("drag", json!({"from_x": 250, "from_y": 190, "to_x": 600, "to_y": 450, "window": fixture.title}));
    assert_eq!(answer["delivered"], true);
    let seen = fixture.take();
    let down = seen.iter().position(|event| *event == Seen::Button { what: "left down", x: 250, y: 190 }).expect("pressed at the start");
    let up = seen.iter().position(|event| *event == Seen::Button { what: "left up", x: 600, y: 450 }).expect("released at the end");
    let between = seen[down..up].iter().filter(|event| matches!(event, Seen::Move { .. })).count();
    assert!(between >= 10, "the pointer travelled in steps while the button was down: {between}");
}

#[test]
fn positions_are_those_of_the_picture_the_client_has() {
    let (_turn, test, fixture) = start("mc test: scale");
    let small = test.tool("screenshot", json!({"window": fixture.title, "max_width": 250}));
    test.tool("click", json!({"x": 100, "y": 50}));
    assert_eq!(fixture.buttons()[0], ("left down", 400, 250));
    let window = test.tool("screenshot", json!({"window": fixture.title}));
    test.tool("click", json!({"x": 10, "y": 20}));
    assert_eq!(fixture.buttons()[0], ("left down", 210, 170));
    test.tool("click", json!({"x": 100, "y": 50, "capture": small["capture"]}));
    assert_eq!(fixture.buttons()[0], ("left down", 400, 250));
    assert_ne!(small["capture"], window["capture"]);
    assert_eq!(test.refused("click", json!({"x": 1, "y": 1, "capture": 9999})), (400, "bad_input".into()));
}

#[test]
fn text_arrives_as_typed_whatever_the_layout_has_keys_for() {
    let (_turn, test, fixture) = start("mc test: typing");
    let answer = test.tool("type_text", json!({"text": "Hello, World! 123", "window": fixture.title}));
    assert_eq!((&answer["delivered"], &answer["characters"]), (&json!(true), &json!(17)));
    assert_eq!(fixture.typed(), "Hello, World! 123");
    test.tool("type_text", json!({"text": "é€ñ\tß\n", "window": fixture.title}));
    assert_eq!(fixture.typed(), "é€ñ\tß\r");
}

#[test]
fn keys_and_chords_arrive_with_their_modifiers() {
    let (_turn, test, fixture) = start("mc test: keys");
    const CONTROL: u32 = 0x11;
    const SHIFT: u32 = 0x10;
    test.tool("key", json!({"keys": "ctrl+a", "window": fixture.title}));
    let seen = fixture.take();
    assert_eq!(seen.iter().filter_map(|event| if let Seen::KeyDown(key) = event { Some(*key) } else { None }).collect::<Vec<_>>(), [CONTROL, 0x41]);
    assert_eq!(seen.iter().filter(|event| matches!(event, Seen::KeyUp(_))).count(), 2, "every key was released");
    // Windows turns Ctrl+A into the control character 1: proof that Ctrl was down when A arrived.
    assert!(seen.contains(&Seen::Char('\u{1}')), "{seen:?}");

    test.tool("key", json!({"keys": "ctrl+shift+t", "window": fixture.title}));
    assert_eq!(fixture.keys_down(), [CONTROL, SHIFT, 0x54]);
    test.tool("key", json!({"keys": "enter", "window": fixture.title}));
    test.tool("key", json!({"keys": "f5", "window": fixture.title}));
    test.tool("key", json!({"keys": "down", "window": fixture.title, "times": 3}));
    assert_eq!(fixture.keys_down(), [0x0d, 0x74, 0x28, 0x28, 0x28]);
    // Nothing is left held: a plain key types its plain character.
    test.tool("key", json!({"keys": "x", "window": fixture.title}));
    assert_eq!(fixture.typed(), "x");
}

#[test]
fn input_without_a_sure_target_is_refused_and_nothing_is_sent() {
    let (_turn, test, fixture) = start("mc test: refusals");
    assert_eq!(test.refused("type_text", json!({"text": "lost"})), (400, "bad_input".into()));
    assert_eq!(test.refused("key", json!({"keys": "enter"})), (400, "bad_input".into()));
    assert_eq!(test.refused("type_text", json!({"text": "lost", "window": "no such window anywhere"})), (404, "window_not_found".into()));
    assert_eq!(test.refused("type_text", json!({"text": "lost", "window": 4242})), (404, "window_not_found".into()));
    assert_eq!(test.refused("key", json!({"keys": "ctrl+nonsense", "window": fixture.title})), (400, "bad_input".into()));
    assert_eq!(test.refused("key", json!({"keys": "a", "window": fixture.title, "times": 0})), (400, "bad_input".into()));
    assert_eq!(test.refused("click", json!({"x": 900, "y": 160, "window": fixture.title})), (400, "bad_input".into()));
    assert_eq!(test.refused("click", json!({"x": 900000, "y": 100})), (400, "bad_input".into()));
    assert_eq!(test.refused("click", json!({"x": 10})), (400, "bad_input".into()));
    assert_eq!(test.refused("click", json!({"x": 210, "y": 160, "button": "fourth"})), (400, "bad_input".into()));
    assert_eq!(test.refused("scroll", json!({"x": 210, "y": 160})), (400, "bad_input".into()));
    assert_eq!(test.refused("drag", json!({"from_x": 900, "from_y": 160, "to_x": 250, "to_y": 250, "window": fixture.title})), (400, "bad_input".into()));
    let sent = fixture.take().into_iter().filter(|seen| !matches!(seen, Seen::Move { .. })).collect::<Vec<_>>();
    assert_eq!(sent, [], "none of the refused actions sent anything");
}

#[test]
fn a_picture_over_mcp_is_an_image() {
    let (_turn, test, fixture) = start("mc test: mcp");
    let (_, answer) = test.post(
        "/mcp",
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "screenshot", "arguments": {"window": fixture.title}}}),
    );
    let content = answer["result"]["content"].as_array().unwrap();
    assert_eq!((&content[0]["type"], &content[1]["type"], &content[1]["mimeType"]), (&json!("text"), &json!("image"), &json!("image/png")));
    assert_eq!(png_pixel(&decode_base64(content[1]["data"].as_str().unwrap()), 1, 1), ([255, 128, 0], (500, 350)));
    assert!(answer["result"]["structuredContent"]["png_base64"].is_null());
}

#[test]
fn the_clipboard_holds_what_was_set() {
    let (_turn, test, _fixture) = start("mc test: clipboard");
    // This is the real session's clipboard: what was on it goes back afterwards. Text can be put back; anything else
    // (a picture, files) can't, so then the test leaves the clipboard alone and says so.
    let before = test.tool("clipboard_get", json!({}))["text"].clone();
    let holds_other_things = before.is_null() && unsafe { windows_sys::Win32::System::DataExchange::CountClipboardFormats() } > 0;
    assert!(!holds_other_things, "the clipboard holds something that isn't text; copy some text or clear it, then run this test again");
    let set = test.tool("clipboard_set", json!({"text": "héllo €\r\nsecond line"}));
    assert_eq!((&set["delivered"], &set["characters"]), (&json!(true), &json!(20)));
    assert_eq!(test.tool("clipboard_get", json!({}))["text"], "héllo €\r\nsecond line");
    // Seen by another program too.
    let output = std::process::Command::new("powershell").args(["-NoProfile", "-Command", "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Clipboard -Raw"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim_end(), "héllo €\r\nsecond line");
    let long = "0123456789abcdef".repeat(20_000);
    test.tool("clipboard_set", json!({"text": long}));
    assert_eq!(test.tool("clipboard_get", json!({}))["text"].as_str().unwrap().len(), 320_000);
    assert_eq!(test.refused("clipboard_set", json!({})), (400, "bad_input".into()));
    test.tool("clipboard_set", json!({"text": before.as_str().unwrap_or("")}));
}

fn element<'a>(answer: &'a Value, role: &str, name: &str) -> &'a Value {
    answer["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| element["role"] == role && element["name"].as_str().unwrap().contains(name))
        .unwrap_or_else(|| panic!("no {role} named {name} in {answer}"))
}

#[test]
fn the_tree_names_what_is_in_a_window_and_elements_can_be_clicked() {
    let turn = desktop_turn();
    let test = Test::new();
    test.tool("status", json!({}));
    let fixture = Fixture::open_with("mc test: tree", (200, 150, 500, 350), true);
    let tree = test.tool("tree", json!({"window": fixture.title}));
    assert_eq!((&tree["truncated"], &tree["window"]["title"]), (&json!(false), &json!("mc test: tree")));
    assert_eq!((&tree["elements"][0]["depth"], &tree["elements"][0]["name"]), (&json!(0), &json!("mc test: tree")));
    let button = element(&tree, "button", "Save it");
    assert_eq!((&button["x"], &button["y"], &button["width"], &button["height"], &button["depth"]), (&json!(220), &json!(170), &json!(120), &json!(30), &json!(1)));
    assert!(button["enabled"].is_null() && button["focused"].is_null(), "only said when unusual: {button}");
    let field = element(&tree, "text", "");
    assert_eq!((&field["value"], &field["x"], &field["y"]), (&json!("first text"), &json!(220), &json!(220)));
    element(&tree, "label", "A label");

    // The drop-down list has a button of its own, which has no name.
    let found = test.tool("find", json!({"window": fixture.title, "role": "button", "name": "save"}));
    assert_eq!(found["elements"].as_array().unwrap().len(), 1);
    assert_eq!(test.tool("find", json!({"window": fixture.title, "name": "SAVE"}))["elements"][0]["role"], "button");
    assert_eq!(test.tool("find", json!({"window": fixture.title, "value": "first"}))["elements"][0]["role"], "text");
    assert_eq!(test.tool("find", json!({"window": fixture.title, "role": "slider"}))["elements"], json!([]));
    let limited = test.tool("tree", json!({"window": fixture.title, "max_elements": 2}));
    assert_eq!((limited["elements"].as_array().unwrap().len(), &limited["truncated"]), (2, &json!(true)));

    // Clicking the button by its id presses it: the window is told so.
    fixture.take();
    let clicked = test.tool("click", json!({"element": button["id"]}));
    assert_eq!((&clicked["delivered"], &clicked["at"]), (&json!(true), &json!({"x": 280, "y": 185})));
    assert!(fixture.take().contains(&Seen::Command(101)));

    // Clicking the field puts the keyboard in it; what is typed shows up as its value. `clear` empties it first
    // (a plain Windows text field has no Ctrl+A), `caret` puts the text at either end of what is there.
    test.tool("click", json!({"element": field["id"]}));
    test.tool("type_text", json!({"text": "typed by the test", "clear": true, "window": fixture.title}));
    let after = test.tool("find", json!({"window": fixture.title, "role": "text"}));
    assert_eq!((&after["elements"][0]["value"], &after["elements"][0]["focused"]), (&json!("typed by the test"), &json!(true)));
    test.tool("type_text", json!({"text": "<", "caret": "start", "window": fixture.title}));
    test.tool("type_text", json!({"text": ">", "caret": "end", "window": fixture.title}));
    assert_eq!(test.tool("find", json!({"window": fixture.title, "role": "text"}))["elements"][0]["value"], "<typed by the test>");

    // The compact view keeps the controls and leaves out the label.
    let compact = test.tool("tree", json!({"window": fixture.title, "interactive": true}));
    let kept: Vec<&str> = compact["elements"].as_array().unwrap().iter().map(|element| element["role"].as_str().unwrap()).collect();
    assert!(["button", "text", "check_box", "combo_box"].iter().all(|role| kept.contains(role)) && !kept.contains(&"label") && !kept.contains(&"window"), "{compact}");

    assert_eq!(test.refused("click", json!({"element": "9999.1"})), (409, "element_gone".into()));
    assert_eq!(test.refused("click", json!({"element": format!("{}.9999", tree["tree"])})), (409, "element_gone".into()));
    assert_eq!(test.refused("click", json!({"element": "the button"})), (400, "bad_input".into()));
    assert_eq!(test.refused("click", json!({"element": button["id"], "x": 1, "y": 1})), (400, "bad_input".into()));
    assert_eq!(test.refused("find", json!({"window": fixture.title})), (400, "bad_input".into()));
    assert_eq!(test.refused("tree", json!({"window": "no such window anywhere"})), (404, "window_not_found".into()));
    drop(fixture);
    assert_eq!(test.refused("click", json!({"element": button["id"]})), (409, "element_gone".into()), "the window closed");
    drop(turn);
}

#[test]
fn wait_for_waits_for_windows_and_what_is_in_them() {
    let turn = desktop_turn();
    let test = Test::new();
    test.tool("status", json!({}));
    let fixture = Fixture::open_with("mc test: waiting", (200, 150, 500, 350), true);
    let there = test.tool("wait_for", json!({"window": fixture.title, "role": "button", "name": "save"}));
    assert_eq!(there["elements"][0]["name"], "Save it");
    assert!(there["waited_ms"].as_u64().unwrap() < 3000);
    assert_eq!(test.tool("wait_for", json!({"window": fixture.title}))["window"]["title"], "mc test: waiting");
    let began = Instant::now();
    assert_eq!(test.refused("wait_for", json!({"window": fixture.title, "role": "slider", "timeout_s": 1})), (408, "timed_out".into()));
    assert!(began.elapsed() >= Duration::from_secs(1) && began.elapsed() < Duration::from_secs(8));
    assert_eq!(test.refused("wait_for", json!({"window": fixture.title, "gone": true, "timeout_s": 1})), (408, "timed_out".into()));
    assert_eq!(test.tool("wait_for", json!({"window": fixture.title, "role": "slider", "gone": true}))["gone"], true);
    assert_eq!(test.refused("wait_for", json!({"role": "button"})), (400, "bad_input".into()));
    assert_eq!(test.refused("wait_for", json!({"window": fixture.title, "timeout_s": 0})), (400, "bad_input".into()));

    // A window that opens later, and one that closes while it is waited for.
    let opener = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(700));
        let late = Fixture::open_with("mc test: late arrival", (750, 150, 300, 200), true);
        std::thread::sleep(Duration::from_millis(1500));
        drop(late);
    });
    let appeared = test.tool("wait_for", json!({"window": "mc test: late arrival", "role": "button"}));
    assert!(appeared["waited_ms"].as_u64().unwrap() >= 300, "{appeared}");
    assert_eq!(test.tool("wait_for", json!({"window": "mc test: late arrival", "gone": true}))["gone"], true);
    opener.join().unwrap();
    drop(fixture);
    drop(turn);
}

/// Acting on elements themselves, through UI Automation's control patterns: the pointer stays where it is and the
/// keyboard goes nowhere.
#[test]
fn elements_are_pressed_set_ticked_opened_and_selected_without_the_pointer() {
    let turn = desktop_turn();
    let test = Test::new();
    test.tool("status", json!({}));
    let fixture = Fixture::open_with("mc test: actions", (200, 150, 500, 350), true);
    let tree = test.tool("tree", json!({"window": fixture.title}));
    fixture.take();

    let button = element(&tree, "button", "Save it");
    let pressed = test.tool("press", json!({"element": button["id"]}));
    assert_eq!((&pressed["role"], &pressed["name"]), (&json!("button"), &json!("Save it")));
    let seen = fixture.take();
    assert!(seen.contains(&Seen::Command(101)), "the window was told its button was pressed: {seen:?}");
    assert!(!seen.iter().any(|seen| matches!(seen, Seen::Button { .. })), "and no mouse button was pressed on it: {seen:?}");

    let field = element(&tree, "text", "");
    assert_eq!(test.tool("set_value", json!({"element": field["id"], "value": "set by the test é"}))["value"], "set by the test é");
    assert_eq!(test.tool("find", json!({"window": fixture.title, "role": "text"}))["elements"][0]["value"], "set by the test é");

    // Typed text goes into the element that is named, which is given the keyboard first; the answer says what
    // had the keyboard.
    test.tool("key", json!({"element": field["id"], "keys": "home"}));
    test.tool("key", json!({"element": field["id"], "keys": "shift+end"}));
    let typed = test.tool("type_text", json!({"element": field["id"], "text": "typed into it"}));
    assert_eq!((&typed["delivered"], &typed["into"]["role"]), (&json!(true), &json!("text")), "{typed}");
    assert_eq!(test.tool("find", json!({"window": fixture.title, "role": "text"}))["elements"][0]["value"], "typed into it");
    let check = element(&tree, "check_box", "Enable thing");
    fixture.take();
    assert_eq!(test.tool("toggle", json!({"element": check["id"], "on": true}))["on"], true);
    assert!(fixture.take().contains(&Seen::Command(104)));
    assert_eq!(test.tool("toggle", json!({"element": check["id"], "on": true}))["on"], true);
    assert!(!fixture.take().contains(&Seen::Command(104)), "already ticked: left alone");
    assert_eq!(test.tool("toggle", json!({"element": check["id"]}))["on"], false, "flipped");

    let list = element(&tree, "combo_box", "");
    assert_eq!(test.tool("expand", json!({"element": list["id"]}))["expanded"], true);
    let open = test.tool("tree", json!({"window": fixture.title}));
    let two = element(&open, "list_item", "Two");
    test.tool("select", json!({"element": two["id"]}));
    let list_now = test.tool("find", json!({"window": fixture.title, "role": "combo_box"}));
    assert_eq!(list_now["elements"][0]["value"], "Two", "{list_now}");
    assert_eq!(test.tool("expand", json!({"element": list_now["elements"][0]["id"], "expanded": false}))["expanded"], false);

    assert_eq!(test.refused("expand", json!({"element": button["id"]})), (501, "not_supported".into()));
    assert_eq!(test.refused("toggle", json!({"element": button["id"]})), (501, "not_supported".into()));
    assert_eq!(test.refused("select", json!({"element": button["id"]})), (501, "not_supported".into()));
    assert_eq!(test.refused("set_value", json!({"element": button["id"], "value": "x"})), (501, "not_supported".into()));
    assert_eq!(test.refused("set_value", json!({"element": field["id"]})), (400, "bad_input".into()));
    assert_eq!(test.refused("toggle", json!({"element": check["id"], "on": "yes"})), (400, "bad_input".into()));
    assert_eq!(test.refused("press", json!({})), (400, "bad_input".into()));
    // Input in the background is built for macOS only: here it is refused, not sent for real.
    assert_eq!(test.refused("type_text", json!({"text": "x", "window": fixture.title, "background": true})), (501, "not_supported".into()));
    assert_eq!(test.refused("key", json!({"keys": "enter", "window": fixture.title, "background": true})), (501, "not_supported".into()));
    assert_eq!(test.refused("press", json!({"element": "9999.1"})), (409, "element_gone".into()));
    drop(fixture);
    assert_eq!(test.refused("press", json!({"element": button["id"]})), (409, "element_gone".into()), "the window closed");
    drop(turn);
}

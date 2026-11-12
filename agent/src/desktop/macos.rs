//! macOS: pictures with the system's own `screencapture`, input with Core Graphics events, windows from the window
//! server's list, the clipboard with `pbcopy` and `pbpaste`. Positions are in points, as macOS counts them; a picture
//! of a dense display has more pixels than points, and the tools account for that.
//!
//! Two permissions, which only the user can grant in System Settings > Privacy & Security: Screen Recording (to see
//! other apps' windows in pictures, and their titles) and Accessibility (to send input and raise windows).

// The Objective-C helper's macros test a setting newer compilers don't know; that is theirs to fix.
#![allow(unexpected_cfgs)]

use std::ffi::c_void;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use objc::runtime::Object;

use super::{Button, Desktop, DesktopError, Image, Key, Rect, Result, WindowInfo};

type Id = *mut Object;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CgRect {
    origin: Point,
    width: f64,
    height: f64,
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn AXIsProcessTrusted() -> bool;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CgRect;
    fn CGEventCreate(source: *const c_void) -> *mut c_void;
    fn CGEventGetLocation(event: *mut c_void) -> Point;
    fn CGEventCreateMouseEvent(source: *const c_void, kind: u32, at: Point, button: u32) -> *mut c_void;
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
    fn CGEventCreateScrollWheelEvent(source: *const c_void, units: u32, wheels: u32, wheel1: i32, ...) -> *mut c_void;
    fn CGEventKeyboardSetUnicodeString(event: *mut c_void, length: usize, units: *const u16);
    fn CGEventSetIntegerValueField(event: *mut c_void, field: u32, value: i64);
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CGEventPostToPid(pid: i32, event: *mut c_void);
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> Id;
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementCopyAttributeValue(element: *mut c_void, attribute: Id, value: *mut Id) -> i32;
    fn AXUIElementPerformAction(element: *mut c_void, action: Id) -> i32;
    fn AXUIElementSetAttributeValue(element: *mut c_void, attribute: Id, value: Id) -> i32;
    fn AXUIElementIsAttributeSettable(element: *mut c_void, attribute: Id, settable: *mut u8) -> i32;
    fn AXValueCreate(kind: u32, value: *const c_void) -> *mut c_void;
    fn AXUIElementCopyElementAtPosition(application: *mut c_void, x: f32, y: f32, element: *mut *mut c_void) -> i32;
    fn CFEqual(one: *const c_void, other: *const c_void) -> u8;
    fn AXUIElementSetMessagingTimeout(element: *mut c_void, seconds: f32) -> i32;
    fn AXValueGetValue(value: *const c_void, kind: u32, into: *mut c_void) -> bool;
    fn CFRelease(object: *const c_void);
}

const SHIFT: u64 = 0x2_0000;
const CONTROL: u64 = 0x4_0000;
const OPTION: u64 = 0x8_0000;
const COMMAND: u64 = 0x10_0000;

/// What has to be remembered between one tool call and the next: the modifier keys held, and the last click, so a
/// second click soon after in the same place is sent as the second of a double click.
struct Held {
    modifiers: u64,
    last_click: Option<(Instant, Point, Button, i64)>,
}

static HELD: Mutex<Held> = Mutex::new(Held { modifiers: 0, last_click: None });

pub struct MacDesktop;

/// Whether pictures can show other apps, and whether input can be sent.
pub fn permissions() -> (bool, bool) {
    // SAFETY: plain calls with no arguments.
    unsafe { (CGPreflightScreenCaptureAccess(), AXIsProcessTrusted()) }
}

fn needs(permission: &str, what: &str) -> DesktopError {
    DesktopError::not_supported(format!(
        "macOS hasn't given this program the {permission} permission, which {what} needs. Grant it in System Settings > Privacy & Security > {permission}, then start the worker again."
    ))
}

fn text(string: &str) -> Id {
    let bytes = std::ffi::CString::new(string.replace('\0', "")).expect("no zero bytes");
    // SAFETY: the bytes are a zero-ended string that outlives the call; the returned object is autoreleased.
    unsafe { msg_send![class!(NSString), stringWithUTF8String: bytes.as_ptr()] }
}

fn string_of(object: Id) -> String {
    if object.is_null() {
        return String::new();
    }
    // SAFETY: the object is an NSString from the window list or an accessibility attribute.
    unsafe {
        let bytes: *const std::os::raw::c_char = msg_send![object, UTF8String];
        if bytes.is_null() { String::new() } else { std::ffi::CStr::from_ptr(bytes).to_string_lossy().into_owned() }
    }
}

/// Runs something inside an autorelease pool, so the objects AppKit hands out are freed when it ends.
fn pooled<T>(act: impl FnOnce() -> T) -> T {
    // SAFETY: a pool made and drained on the same thread around the work.
    unsafe {
        let pool: Id = msg_send![class!(NSAutoreleasePool), new];
        let result = act();
        let _: () = msg_send![pool, drain];
        result
    }
}

fn post(event: *mut c_void) -> Result<()> {
    if event.is_null() {
        return Err(DesktopError::failed("macOS: the event couldn't be made"));
    }
    // SAFETY: the event was just made and is released after it is posted. 0: where hardware events enter.
    unsafe {
        CGEventPost(0, event);
        CFRelease(event);
    }
    Ok(())
}

/// Hands an event to one app only: nothing else on the machine sees it, and the real keyboard and pointer are left
/// as they are.
fn post_to(pid: u32, event: *mut c_void) -> Result<()> {
    if event.is_null() {
        return Err(DesktopError::failed("macOS: the event couldn't be made"));
    }
    // SAFETY: the event was just made and is released after it is posted.
    unsafe {
        CGEventPostToPid(pid as i32, event);
        CFRelease(event);
    }
    Ok(())
}

/// Text as key events, each character riding on one whatever the keyboard layout, handed to `send`.
fn send_text(text: &str, send: &dyn Fn(*mut c_void) -> Result<()>) -> Result<()> {
    for character in text.chars() {
        match character {
            '\r' => continue,
            '\n' | '\t' => {
                let code = if character == '\n' { 36 } else { 48 };
                for down in [true, false] {
                    // SAFETY: plain values; the event is released by `send`.
                    unsafe {
                        let event = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
                        if !event.is_null() {
                            CGEventSetFlags(event, 0);
                        }
                        send(event)?;
                    }
                }
            }
            _ => {
                let mut units = [0u16; 2];
                let units = character.encode_utf16(&mut units);
                for down in [true, false] {
                    // SAFETY: the units outlive the call that copies them into the event.
                    unsafe {
                        let event = CGEventCreateKeyboardEvent(std::ptr::null(), 0, down);
                        if !event.is_null() {
                            CGEventKeyboardSetUnicodeString(event, units.len(), units.as_ptr());
                            CGEventSetFlags(event, 0);
                        }
                        send(event)?;
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    Ok(())
}

/// Makes sure keys handed to the window's app go to this window: it must be the one the app would type into, or
/// become it, without coming to the front.
fn keyboard_stays_in(window: &WindowInfo) -> Result<()> {
    pooled(|| unsafe {
        in_window(window, |ours| {
            let app = AXUIElementCreateApplication(window.pid as i32);
            if app.is_null() {
                return Ok(());
            }
            AXUIElementSetMessagingTimeout(app, 3.0);
            let is_ours = || {
                let focused = attribute(app, "AXFocusedWindow");
                let same = !focused.is_null() && CFEqual(focused as *const c_void, ours as *const c_void) != 0;
                if !focused.is_null() {
                    CFRelease(focused as *const c_void);
                }
                same
            };
            let mut same = is_ours();
            if !same {
                // Made the app's own main window, which is not the same as coming to the front: the app stays
                // behind whatever the user is working in.
                let yes: Id = msg_send![class!(NSNumber), numberWithBool: objc::runtime::YES];
                AXUIElementSetAttributeValue(ours, text("AXMain"), yes);
                for _ in 0..10 {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    same = is_ours();
                    if same {
                        break;
                    }
                }
            }
            // An app that names no window as its own at all still types into the one it has.
            let none_named = {
                let focused = attribute(app, "AXFocusedWindow");
                let none = focused.is_null();
                if !none {
                    CFRelease(focused as *const c_void);
                }
                none
            };
            CFRelease(app);
            if same || none_named {
                Ok(())
            } else {
                Err(DesktopError { code: "focus_refused", message: format!("another window of that app has its keyboard and \"{}\" didn't take it, so nothing was sent", window.title) })
            }
        })
    })
}

/// The key that types a character on a US keyboard, or a named key, as macOS numbers them.
pub fn key_code(key: Key) -> Option<u16> {
    Some(match key {
        Key::Char(character) => {
            const KEYS: &str = "asdfhgzxcv\u{0}bqweryt123465=97-80]ou[ip\u{0}lj'k;\\,/nm.";
            let wanted = character.to_ascii_lowercase();
            match wanted {
                '`' => 50,
                _ => KEYS.chars().position(|known| known == wanted && known != '\0')? as u16,
            }
        }
        Key::Enter => 36,
        Key::Tab => 48,
        Key::Space => 49,
        Key::Backspace => 51,
        Key::Escape => 53,
        Key::Meta => 55,
        Key::Shift => 56,
        Key::CapsLock => 57,
        Key::Alt => 58,
        Key::Ctrl => 59,
        Key::Home => 115,
        Key::PageUp => 116,
        Key::Delete => 117,
        Key::End => 119,
        Key::PageDown => 121,
        Key::Left => 123,
        Key::Right => 124,
        Key::Down => 125,
        Key::Up => 126,
        Key::F(number) => *[122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90].get(usize::from(number).checked_sub(1)?)?,
        // No such keys on a Mac keyboard.
        Key::Insert | Key::PrintScreen | Key::Menu => return None,
    })
}

fn modifier_flag(key: Key) -> u64 {
    match key {
        Key::Shift => SHIFT,
        Key::Ctrl => CONTROL,
        Key::Alt => OPTION,
        Key::Meta => COMMAND,
        _ => 0,
    }
}

impl MacDesktop {
    pub fn open() -> Result<MacDesktop> {
        Ok(MacDesktop)
    }

    fn may_send_input(&self) -> Result<()> {
        if permissions().1 { Ok(()) } else { Err(needs("Accessibility", "sending keys and clicks")) }
    }
}

impl Desktop for MacDesktop {
    fn bounds(&self) -> Result<Rect> {
        // SAFETY: plain calls. The main display: the one with the menu bar, whose corner is 0,0.
        let bounds = unsafe { CGDisplayBounds(CGMainDisplayID()) };
        if bounds.width < 1.0 {
            return Err(DesktopError::not_supported("macOS: there is no display in this session"));
        }
        Ok(Rect { x: bounds.origin.x as i32, y: bounds.origin.y as i32, width: bounds.width as u32, height: bounds.height as u32 })
    }

    fn capture(&self, area: Rect) -> Result<Image> {
        if !permissions().0 {
            return Err(needs("Screen Recording", "taking pictures of the screen"));
        }
        let file = std::env::temp_dir().join(format!("mc-capture-{}-{:?}.png", std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
        // -x: no sound. -R: this rectangle, in points. The picture has the display's real pixels.
        let taken = Command::new("/usr/sbin/screencapture")
            .args(["-x", "-t", "png", "-R", &format!("{},{},{},{}", area.x, area.y, area.width, area.height)])
            .arg(&file)
            .status()
            .map_err(|error| DesktopError::failed(format!("macOS: screencapture couldn't be run: {error}")))?;
        let bytes = std::fs::read(&file);
        let _ = std::fs::remove_file(&file);
        let bytes = bytes.ok().filter(|_| taken.success()).ok_or_else(|| DesktopError::failed("macOS: no picture was taken"))?;
        let failed = |error: &dyn std::fmt::Display| DesktopError::failed(format!("macOS: the picture couldn't be read: {error}"));
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().map_err(|error| failed(&error))?;
        let mut pixels = vec![0; reader.output_buffer_size().ok_or_else(|| failed(&"it is too large"))?];
        let info = reader.next_frame(&mut pixels).map_err(|error| failed(&error))?;
        let step = match (info.color_type, info.bit_depth) {
            (png::ColorType::Rgba, png::BitDepth::Eight) => 4,
            (png::ColorType::Rgb, png::BitDepth::Eight) => 3,
            (kind, depth) => return Err(failed(&format!("{kind:?} at {depth:?} isn't handled"))),
        };
        let rgb = pixels.chunks_exact(step).flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect();
        Ok(Image { width: info.width, height: info.height, rgb })
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        pooled(|| unsafe {
            // 1 | 16: windows on screen, without the desktop's own. Front to back.
            let list = CGWindowListCopyWindowInfo(1 | 16, 0);
            if list.is_null() {
                return Err(DesktopError::failed("macOS: the window list couldn't be read"));
            }
            let workspace: Id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let front: Id = msg_send![workspace, frontmostApplication];
            let front_pid: i32 = if front.is_null() { -1 } else { msg_send![front, processIdentifier] };
            let count: usize = msg_send![list, count];
            let mut windows: Vec<WindowInfo> = Vec::new();
            let mut dialogs: Vec<usize> = Vec::new();
            // Which of the front app's windows has the keyboard is the app's to say. Without an answer, it is taken
            // to be its frontmost window of a real size: a browser's suggestions list and other helper windows sit
            // in front of the window they belong to.
            let focused_number = focused_window_of(front_pid);
            for index in 0..count {
                let entry: Id = msg_send![list, objectAtIndex: index];
                let value = |key: &str| -> Id { msg_send![entry, objectForKey: text(key)] };
                let number = |object: Id| -> f64 { if object.is_null() { 0.0 } else { msg_send![object, doubleValue] } };
                // Layer 0 is where ordinary windows live; the menu bar, the Dock and overlays are on others. Layer 8
                // is where the system puts the dialogs it shows on an app's behalf (a permission prompt, an alert).
                let layer = number(value("kCGWindowLayer"));
                if layer != 0.0 && layer != SYSTEM_DIALOG_LAYER {
                    continue;
                }
                let bounds = value("kCGWindowBounds");
                let part = |key: &str| -> f64 { if bounds.is_null() { 0.0 } else { number(msg_send![bounds, objectForKey: text(key)]) } };
                let frame = Rect { x: part("X") as i32, y: part("Y") as i32, width: part("Width") as u32, height: part("Height") as u32 };
                if frame.width < 2 || frame.height < 2 {
                    continue;
                }
                let pid = number(value("kCGWindowOwnerPID")) as i32;
                let id = number(value("kCGWindowNumber")) as u64;
                // Decided once the whole list is known: see below.
                let focused = false;
                windows.push(WindowInfo {
                    id,
                    // Other apps' titles come only with the Screen Recording permission; empty without it.
                    title: string_of(value("kCGWindowName")),
                    app: string_of(value("kCGWindowOwnerName")),
                    pid: pid.max(0) as u32,
                    frame,
                    focused,
                    minimized: false,
                });
                if layer == SYSTEM_DIALOG_LAYER {
                    dialogs.push(windows.len() - 1);
                }
            }
            CFRelease(list as *const c_void);
            // A system dialog has no title of its own: its question stands in, so it can be named and waited for.
            // Its place is the dialog itself, without the shadow the window list counts.
            for index in dialogs {
                if let Some((question, frame)) = dialog_seen(&windows[index]) {
                    if windows[index].title.is_empty() {
                        windows[index].title = question;
                    }
                    windows[index].frame = frame;
                }
            }
            // The window the app named, when it is in the list; otherwise the app's frontmost window of a real
            // size, and failing that its frontmost window.
            let of_front = |window: &WindowInfo| window.pid as i32 == front_pid;
            let chosen = windows
                .iter()
                .position(|window| of_front(window) && focused_number == Some(window.id))
                .or_else(|| windows.iter().position(|window| of_front(window) && window.frame.width >= 64 && window.frame.height >= 64))
                .or_else(|| windows.iter().position(of_front));
            if let Some(chosen) = chosen {
                windows[chosen].focused = true;
            }
            Ok(windows)
        })
    }

    fn focus(&self, window: u64) -> Result<()> {
        self.may_send_input()?;
        let target = self.windows()?.into_iter().find(|candidate| candidate.id == window).ok_or_else(|| DesktopError::failed("macOS: the window is gone"))?;
        pooled(|| unsafe {
            // The app comes to the front; then the one window among its windows, found by its title, is raised.
            let app: Id = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: target.pid as i32];
            if !app.is_null() {
                // 2: even if another app is in front.
                let _: bool = msg_send![app, activateWithOptions: 2u64];
            }
            // The very window, by the window server's number for it where that can be asked.
            let _ = in_window(&target, |window| {
                AXUIElementPerformAction(window, text("AXRaise"));
                Ok(())
            });
        });
        Ok(())
    }

    fn pointer(&self) -> Result<(i32, i32)> {
        // SAFETY: an empty event carries the pointer's place; it is released at once.
        let at = unsafe {
            let event = CGEventCreate(std::ptr::null());
            let at = CGEventGetLocation(event);
            CFRelease(event);
            at
        };
        Ok((at.x.round() as i32, at.y.round() as i32))
    }

    fn move_pointer(&self, x: i32, y: i32) -> Result<()> {
        self.may_send_input()?;
        // 5: the mouse moved. With a button held, apps expect "dragged" (6 for the left button) instead.
        let dragging = HELD.lock().unwrap().last_click.as_ref().is_some_and(|click| click.3 < 0);
        // SAFETY: plain values; the event is released by `post`.
        post(unsafe { CGEventCreateMouseEvent(std::ptr::null(), if dragging { 6 } else { 5 }, Point { x: f64::from(x), y: f64::from(y) }, 0) })?;
        std::thread::sleep(std::time::Duration::from_millis(8));
        Ok(())
    }

    fn button(&self, button: Button, down: bool) -> Result<()> {
        self.may_send_input()?;
        let (x, y) = self.pointer()?;
        let at = Point { x: f64::from(x), y: f64::from(y) };
        let (kind, number) = match (button, down) {
            (Button::Left, true) => (1, 0),
            (Button::Left, false) => (2, 0),
            (Button::Right, true) => (3, 1),
            (Button::Right, false) => (4, 1),
            (Button::Middle, true) => (25, 2),
            (Button::Middle, false) => (26, 2),
        };
        let mut held = HELD.lock().unwrap();
        // The count macOS sends with each click: 2 for the second of a double click.
        let clicks = match (down, held.last_click.take()) {
            (true, Some((when, place, which, count))) if which == button && place == at && when.elapsed().as_millis() < 450 => count.abs() + 1,
            (true, _) => 1,
            (false, Some((_, _, _, count))) => count.abs(),
            (false, None) => 1,
        };
        // Negative while the button is down, which is how a move in between knows it is a drag.
        held.last_click = Some((Instant::now(), at, button, if down { -clicks } else { clicks }));
        let flags = held.modifiers;
        drop(held);
        // SAFETY: plain values; field 1 is the click count; the event is released by `post`.
        unsafe {
            let event = CGEventCreateMouseEvent(std::ptr::null(), kind, at, number);
            if !event.is_null() {
                CGEventSetIntegerValueField(event, 1, clicks);
                CGEventSetFlags(event, flags);
            }
            post(event)
        }
    }

    fn scroll(&self, right: i32, down: i32) -> Result<()> {
        self.may_send_input()?;
        // 1: by lines. Two wheels: up-and-down (positive is up), then sideways (positive is left).
        // SAFETY: the two wheel amounts match the count of 2.
        post(unsafe { CGEventCreateScrollWheelEvent(std::ptr::null(), 1, 2, -down, -right) })
    }

    fn key(&self, key: Key, down: bool) -> Result<()> {
        self.may_send_input()?;
        let code = key_code(key).ok_or_else(|| DesktopError::failed(format!("macOS: there is no {key:?} key on a Mac keyboard; use type_text for a character")))?;
        let mut held = HELD.lock().unwrap();
        let flag = modifier_flag(key);
        if down {
            held.modifiers |= flag;
        } else {
            held.modifiers &= !flag;
        }
        let flags = held.modifiers;
        drop(held);
        // SAFETY: plain values; the event is released by `post`.
        unsafe {
            let event = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
            if !event.is_null() {
                CGEventSetFlags(event, flags);
            }
            post(event)
        }
    }

    fn text(&self, text: &str) -> Result<()> {
        self.may_send_input()?;
        send_text(text, &post)
    }

    fn keyboard_owner(&self, window: &WindowInfo) -> Option<(String, String)> {
        keyboard_owner_of(window.pid)
    }

    fn quiet_text(&self, window: &WindowInfo, text: &str) -> Result<()> {
        self.may_send_input()?;
        keyboard_stays_in(window)?;
        let pid = window.pid;
        send_text(text, &|event| post_to(pid, event))
    }

    fn quiet_chord(&self, window: &WindowInfo, chord: &[Key]) -> Result<()> {
        self.may_send_input()?;
        let flags = chord.iter().fold(0, |flags, key| flags | modifier_flag(*key));
        let keys: Vec<Key> = chord.iter().copied().filter(|key| modifier_flag(*key) == 0).collect();
        if keys.is_empty() {
            return Err(DesktopError { code: "bad_input", message: "a modifier alone can't be sent in the background: give the key it goes with".into() });
        }
        // A shortcut with Command is, in a Mac app, an item of its menus. Choosing that item does the same and is
        // sure to arrive; the key events themselves reach an app in the background only some of the time.
        if let ([Key::Char(character)], true) = (keys.as_slice(), flags & COMMAND != 0) {
            let modifiers = chord.iter().fold(0u32, |modifiers, key| {
                modifiers | match key {
                    Key::Shift => 1,
                    Key::Alt => 2,
                    Key::Ctrl => 4,
                    _ => 0,
                }
            });
            keyboard_stays_in(window)?;
            // Select All in a text field is done directly, by setting what is selected: the one shortcut that is
            // needed all the time (to replace what a field holds) doesn't then depend on the app's menus.
            if character.eq_ignore_ascii_case(&'a') && modifiers == 0 && pooled(|| unsafe { select_everything(window.pid) }) {
                return Ok(());
            }
            if pooled(|| unsafe { choose_menu_item(window.pid, *character, modifiers) }) {
                return Ok(());
            }
        }
        keyboard_stays_in(window)?;
        for key in keys {
            let code = key_code(key).ok_or_else(|| DesktopError::failed(format!("macOS: there is no {key:?} key on a Mac keyboard; use type_text for a character")))?;
            for down in [true, false] {
                // SAFETY: plain values; the event is released by `post_to`. The modifiers ride on the event itself:
                // nothing is held on the real keyboard.
                unsafe {
                    let event = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
                    if !event.is_null() {
                        CGEventSetFlags(event, flags);
                    }
                    post_to(window.pid, event)?;
                }
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
        }
        Ok(())
    }

    fn quiet_click(&self, window: &WindowInfo, x: i32, y: i32) -> Result<String> {
        self.may_send_input()?;
        pooled(|| unsafe {
            let app = AXUIElementCreateApplication(window.pid as i32);
            if app.is_null() {
                return Err(DesktopError::failed("macOS: the app couldn't be asked what is at that point"));
            }
            AXUIElementSetMessagingTimeout(app, 3.0);
            let mut found: *mut c_void = std::ptr::null_mut();
            let code = AXUIElementCopyElementAtPosition(app, x as f32, y as f32, &mut found);
            let outcome = if code != 0 || found.is_null() {
                Err(DesktopError::not_supported("the app doesn't tell assistive technology what is at that point, so it can't be clicked in the background"))
            } else {
                press_or_focus(found, window)
            };
            if !found.is_null() {
                CFRelease(found);
            }
            CFRelease(app);
            outcome
        })
    }

    fn tree(&self, window: &WindowInfo, limits: super::TreeLimits) -> Result<(super::Element, bool)> {
        if !permissions().1 {
            return Err(needs("Accessibility", "reading what is in a window"));
        }
        pooled(|| unsafe {
            in_window(window, |found| {
                let mut reading = Reading { limits, read: 0, cut: false };
                let element = reading.element(found, 0);
                Ok((element, reading.cut))
            })
        })
    }

    fn act(&self, window: &WindowInfo, target: super::Target, action: &super::ElementAction) -> Result<super::Acted> {
        if !permissions().1 {
            return Err(needs("Accessibility", "acting on what is in a window"));
        }
        pooled(|| unsafe {
            in_window(window, |root| {
                // Down the same way the tree was read, child by child. The lists of children are kept until the end:
                // an element lives as long as the list it came from.
                let mut lists: Vec<Id> = Vec::new();
                let mut at = root;
                let mut missing = false;
                for &index in target.path {
                    let children = attribute(at, "AXChildren");
                    let count: usize = if children.is_null() { 0 } else { msg_send![children, count] };
                    if !children.is_null() {
                        lists.push(children);
                    }
                    if index as usize >= count {
                        missing = true;
                        break;
                    }
                    let child: Id = msg_send![children, objectAtIndex: index as usize];
                    at = child.cast();
                }
                let outcome = if missing { Err(DesktopError::gone("the window no longer has it")) } else { perform(at, target, action) };
                for list in lists {
                    CFRelease(list as *const c_void);
                }
                outcome
            })
        })
    }

    fn clipboard(&self) -> Result<Option<String>> {
        let output = Command::new("/usr/bin/pbpaste").output().map_err(|error| DesktopError::failed(format!("macOS: pbpaste couldn't be run: {error}")))?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        // pbpaste prints nothing both for an empty clipboard and for one that holds no text.
        Ok((!text.is_empty()).then_some(text))
    }

    fn set_clipboard(&self, text: &str) -> Result<()> {
        let failed = |error: std::io::Error| DesktopError::failed(format!("macOS: pbcopy couldn't be run: {error}"));
        let mut child = Command::new("/usr/bin/pbcopy").stdin(Stdio::piped()).spawn().map_err(failed)?;
        child.stdin.take().expect("piped").write_all(text.as_bytes()).map_err(failed)?;
        child.wait().map_err(failed)?;
        Ok(())
    }
}

/// Finds the accessibility element that is this window (the app's window with this title; where two share a title,
/// the one in the same place) and hands it to `with`.
///
/// SAFETY: must run inside an autorelease pool.
unsafe fn in_window<T>(window: &WindowInfo, with: impl FnOnce(*mut c_void) -> Result<T>) -> Result<T> {
    unsafe {
        let app = AXUIElementCreateApplication(window.pid as i32);
        if app.is_null() {
            return Err(DesktopError::failed("macOS: the app couldn't be asked about its windows"));
        }
        // An app that is busy or hung must not hold the tool for ever.
        AXUIElementSetMessagingTimeout(app, 3.0);
        // Apps built on Chromium (browsers, Electron apps) only describe what is in their windows once something
        // says it is listening. Apps that don't know this setting ignore it.
        let yes: Id = msg_send![class!(NSNumber), numberWithBool: objc::runtime::YES];
        AXUIElementSetAttributeValue(app, text("AXManualAccessibility"), yes);
        let windows = attribute(app, "AXWindows");
        let count: usize = if windows.is_null() { 0 } else { msg_send![windows, count] };
        // The same window, by the window server's own number for it; where that can't be asked, by its title (a
        // browser words its title differently in the two places: a tab that plays sound gets a mark in one), and
        // failing that by being exactly where the window is. A system dialog answers none of these (its number can't
        // be asked, the title it is listed under is its question, and the window list counts its shadow where
        // accessibility doesn't): it is the window that sits in the middle of the listed one.
        let mut found: Id = std::ptr::null_mut();
        let mut by_title: Id = std::ptr::null_mut();
        let mut by_place: Id = std::ptr::null_mut();
        let mut by_middle: Id = std::ptr::null_mut();
        for index in 0..count {
            let candidate: Id = msg_send![windows, objectAtIndex: index];
            if window_number(candidate.cast()) == Some(window.id) {
                found = candidate;
                break;
            }
            let frame = frame_of(candidate.cast());
            let here = frame.is_some_and(|frame| frame.x == window.frame.x && frame.y == window.frame.y);
            if string_attribute(candidate.cast(), "AXTitle") == window.title && (by_title.is_null() || here) {
                by_title = candidate;
            }
            if frame == Some(window.frame) && by_place.is_null() {
                by_place = candidate;
            }
            let middle = |frame: Rect| (2 * frame.x + frame.width as i32, 2 * frame.y + frame.height as i32);
            let centred = frame.is_some_and(|frame| {
                let (ours, theirs) = (middle(frame), middle(window.frame));
                frame.width <= window.frame.width && frame.height <= window.frame.height && (ours.0 - theirs.0).abs() <= 4 && (ours.1 - theirs.1).abs() <= 4
            });
            if centred && by_middle.is_null() {
                by_middle = candidate;
            }
        }
        if found.is_null() {
            found = [by_title, by_place, by_middle].into_iter().find(|candidate| !candidate.is_null()).unwrap_or(std::ptr::null_mut());
        }
        let outcome = if found.is_null() {
            Err(DesktopError::not_supported(format!("the app behind \"{}\" doesn't describe that window to assistive technology", window.title)))
        } else {
            with(found.cast())
        };
        if !windows.is_null() {
            CFRelease(windows as *const c_void);
        }
        CFRelease(app);
        outcome
    }
}

/// The window layer of the dialogs macOS shows on an app's behalf (`kCGModalPanelWindowLevel`).
const SYSTEM_DIALOG_LAYER: f64 = 8.0;

/// What a system dialog asks (its first line of text) and where the dialog itself is. Nothing when it can't be
/// read (no Accessibility permission).
///
/// SAFETY: must run inside an autorelease pool.
unsafe fn dialog_seen(window: &WindowInfo) -> Option<(String, Rect)> {
    if !permissions().1 {
        return None;
    }
    unsafe {
        in_window(window, |dialog| {
            let children = attribute(dialog, "AXChildren");
            let count: usize = if children.is_null() { 0 } else { msg_send![children, count] };
            let mut question = String::new();
            for index in 0..count {
                let child: Id = msg_send![children, objectAtIndex: index];
                if string_attribute(child.cast(), "AXRole") == "AXStaticText" {
                    question = string_attribute(child.cast(), "AXValue");
                    if !question.is_empty() {
                        break;
                    }
                }
            }
            if !children.is_null() {
                CFRelease(children as *const c_void);
            }
            Ok((question, frame_of(dialog).unwrap_or(window.frame)))
        })
        .ok()
    }
}

/// The window server's number for the window an accessibility element is, which is also the window's id in the
/// window list. Asked through a call macOS has but doesn't publish; where it is missing, windows are matched by
/// title and place instead.
fn window_number(element: *mut c_void) -> Option<u64> {
    type Lookup = unsafe extern "C" fn(*mut c_void, *mut u32) -> i32;
    static LOOKUP: std::sync::OnceLock<Option<Lookup>> = std::sync::OnceLock::new();
    let lookup = LOOKUP.get_or_init(|| {
        // SAFETY: looks a name up among the libraries already loaded; a missing name gives null.
        let found = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"_AXUIElementGetWindow".as_ptr()) };
        // SAFETY: when present, the function has had this shape in every macOS release that has it.
        (!found.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, Lookup>(found) })
    });
    let mut number = 0u32;
    // SAFETY: the element is live and the number a local.
    (unsafe { (*lookup)?(element, &mut number) } == 0 && number != 0).then_some(u64::from(number))
}

/// What has the keyboard inside an app: its role and name, as the tree would tell them.
fn keyboard_owner_of(pid: u32) -> Option<(String, String)> {
    pooled(|| unsafe {
        let app = AXUIElementCreateApplication(pid as i32);
        if app.is_null() {
            return None;
        }
        AXUIElementSetMessagingTimeout(app, 1.0);
        let focused = attribute(app, "AXFocusedUIElement");
        let owner = (!focused.is_null()).then(|| {
            let (role, name, _) = described(focused.cast());
            (mac_role(&role), name)
        });
        if !focused.is_null() {
            CFRelease(focused as *const c_void);
        }
        CFRelease(app);
        owner
    })
}

/// The window list's id of the window the app in front types into, when it tells.
fn focused_window_of(pid: i32) -> Option<u64> {
    if pid <= 0 || !unsafe { AXIsProcessTrusted() } {
        return None;
    }
    // SAFETY: the app element is released; attributes that come back null are not used.
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return None;
        }
        // An app that is busy must not hold up a look at the window list.
        AXUIElementSetMessagingTimeout(app, 0.5);
        let mut number = None;
        for name in ["AXFocusedWindow", "AXMainWindow"] {
            let window = attribute(app, name);
            if !window.is_null() {
                number = window_number(window.cast());
                CFRelease(window as *const c_void);
            }
            if number.is_some() {
                break;
            }
        }
        CFRelease(app);
        number
    }
}

/// An element's role in macOS's word, its name and what it holds, as the tree tells them.
unsafe fn described(element: *mut c_void) -> (String, String, Option<String>) {
    unsafe {
        let role = string_attribute(element, "AXRole");
        let held = string_attribute(element, "AXValue");
        let title = string_attribute(element, "AXTitle");
        // A label's text is what it holds; other things are named by their title, or failing that their description.
        if role == "AXStaticText" {
            return (role, held, None);
        }
        let name = if title.is_empty() { string_attribute(element, "AXDescription") } else { title };
        let value = Some(held).filter(|held| !held.is_empty() || matches!(role.as_str(), "AXTextField" | "AXTextArea" | "AXSearchField"));
        (role, name, value)
    }
}

/// Selects everything in the text field that has the keyboard in an app. False when what has the keyboard isn't
/// text whose selection can be set.
unsafe fn select_everything(pid: u32) -> bool {
    unsafe {
        let app = AXUIElementCreateApplication(pid as i32);
        if app.is_null() {
            return false;
        }
        AXUIElementSetMessagingTimeout(app, 3.0);
        let focused = attribute(app, "AXFocusedUIElement");
        let mut done = false;
        if !focused.is_null() {
            let field: *mut c_void = focused.cast();
            if settable(field, "AXSelectedTextRange") {
                let length: isize = string_attribute(field, "AXNumberOfCharacters").parse().unwrap_or(0);
                // 4: a range of characters, as a start and a length.
                let everything: [isize; 2] = [0, length];
                let range = AXValueCreate(4, everything.as_ptr().cast());
                if !range.is_null() {
                    done = AXUIElementSetAttributeValue(field, text("AXSelectedTextRange"), range.cast()) == 0;
                    CFRelease(range);
                }
            }
            CFRelease(focused as *const c_void);
        }
        CFRelease(app);
        done
    }
}

/// Chooses the item of an app's menus that has this Command shortcut, if there is one. `modifiers` are the keys
/// held besides Command, as the menus count them: 1 Shift, 2 Option, 4 Control.
unsafe fn choose_menu_item(pid: u32, character: char, modifiers: u32) -> bool {
    unsafe fn search(element: *mut c_void, character: char, modifiers: u32, depth: u32) -> bool {
        unsafe {
            let children = attribute(element, "AXChildren");
            let count: usize = if children.is_null() { 0 } else { msg_send![children, count] };
            let mut chosen = false;
            for index in 0..count {
                let child: Id = msg_send![children, objectAtIndex: index];
                let child: *mut c_void = child.cast();
                let shortcut = string_attribute(child, "AXMenuItemCmdChar");
                let same_key = shortcut.chars().count() == 1 && shortcut.chars().next().is_some_and(|key| key.eq_ignore_ascii_case(&character));
                if same_key && string_attribute(child, "AXMenuItemCmdModifiers").parse::<u32>().unwrap_or(0) == modifiers {
                    chosen = flag_attribute(child, "AXEnabled", true) && AXUIElementPerformAction(child, text("AXPress")) == 0;
                } else if depth < 4 {
                    chosen = search(child, character, modifiers, depth + 1);
                }
                if chosen {
                    break;
                }
            }
            if !children.is_null() {
                CFRelease(children as *const c_void);
            }
            chosen
        }
    }
    unsafe {
        let app = AXUIElementCreateApplication(pid as i32);
        if app.is_null() {
            return false;
        }
        AXUIElementSetMessagingTimeout(app, 3.0);
        let bar = attribute(app, "AXMenuBar");
        let chosen = !bar.is_null() && search(bar.cast(), character, modifiers, 0);
        if !bar.is_null() {
            CFRelease(bar as *const c_void);
        }
        CFRelease(app);
        chosen
    }
}

/// What a click on this element would do, done through assistive technology: a text field takes the keyboard,
/// anything else is pressed — it, or the nearest thing around it that can be.
unsafe fn press_or_focus(found: *mut c_void, window: &WindowInfo) -> Result<String> {
    unsafe {
        // The app answers for whichever of its windows is at the point; it has to be the one that was named.
        let in_window = attribute(found, "AXWindow");
        if !in_window.is_null() {
            let title = string_attribute(in_window.cast(), "AXTitle");
            CFRelease(in_window as *const c_void);
            if title != window.title {
                return Err(DesktopError::failed(format!("the app's window \"{title}\" is at that point, not \"{}\", so nothing was sent", window.title)));
            }
        }
        let mut at = found;
        let mut owned: Vec<Id> = Vec::new();
        let mut outcome = None;
        for _ in 0..5 {
            let (role, name, _) = described(at);
            let what = format!("the {} \"{}\"", mac_role(&role), name);
            if matches!(role.as_str(), "AXTextField" | "AXTextArea" | "AXSearchField" | "AXComboBox") {
                let yes: Id = msg_send![class!(NSNumber), numberWithBool: objc::runtime::YES];
                outcome = Some(match AXUIElementSetAttributeValue(at, text("AXFocused"), yes) {
                    0 => Ok(format!("gave the keyboard to {what}")),
                    code => Err(DesktopError::failed(format!("macOS: {what} didn't take the keyboard (accessibility error {code})"))),
                });
                break;
            }
            if role == "AXWindow" || role.is_empty() {
                break;
            }
            match AXUIElementPerformAction(at, text("AXPress")) {
                0 => {
                    outcome = Some(Ok(format!("pressed {what}")));
                    break;
                }
                // Not something that can be pressed: what it sits in may be.
                -25205 | -25206 | -25208 => {
                    let parent = attribute(at, "AXParent");
                    if parent.is_null() {
                        break;
                    }
                    owned.push(parent);
                    at = parent.cast();
                }
                code => {
                    outcome = Some(Err(DesktopError::failed(format!("macOS: {what} didn't do it (accessibility error {code})"))));
                    break;
                }
            }
        }
        for parent in owned {
            CFRelease(parent as *const c_void);
        }
        outcome.unwrap_or_else(|| Err(DesktopError::not_supported("nothing at that point can be pressed without the pointer; leave `background` out to click it for real")))
    }
}

unsafe fn settable(element: *mut c_void, name: &str) -> bool {
    let mut answer = 0u8;
    unsafe { AXUIElementIsAttributeSettable(element, text(name), &mut answer) == 0 && answer != 0 }
}

/// Does one thing to an element, after checking it is still the one the tree showed.
unsafe fn perform(element: *mut c_void, target: super::Target, action: &super::ElementAction) -> Result<super::Acted> {
    use super::{Acted, ElementAction};
    unsafe {
        let (role, name, _) = described(element);
        target.check(&mac_role(&role), &name)?;
        let cannot = |what: &str| DesktopError::not_supported(format!("a {} can't be {what}", target.role));
        // The accessibility errors that mean "this element doesn't do that", and the one for an element that is gone.
        let done = |code: i32, what: &str| match code {
            0 => Ok(()),
            -25205 | -25206 | -25208 => Err(cannot(what)),
            -25202 => Err(DesktopError::gone("the app says it no longer exists")),
            other => Err(DesktopError::failed(format!("macOS: the app didn't do it (accessibility error {other})"))),
        };
        let yes_or_no = |set: bool| -> Id { msg_send![class!(NSNumber), numberWithBool: if set { objc::runtime::YES } else { objc::runtime::NO }] };
        match action {
            ElementAction::Press => {
                done(AXUIElementPerformAction(element, text("AXPress")), "pressed")?;
                Ok(Acted::default())
            }
            ElementAction::Focus => {
                if !settable(element, "AXFocused") {
                    return Err(cannot("given the keyboard"));
                }
                done(AXUIElementSetAttributeValue(element, text("AXFocused"), yes_or_no(true)), "given the keyboard")?;
                Ok(Acted::default())
            }
            ElementAction::SetValue(wanted) => {
                if !settable(element, "AXValue") {
                    return Err(cannot("given a value"));
                }
                let current = attribute(element, "AXValue");
                let numeric: bool = !current.is_null() && msg_send![current, isKindOfClass: class!(NSNumber)];
                if !current.is_null() {
                    CFRelease(current as *const c_void);
                }
                let value: Id = if numeric {
                    let number: f64 = wanted.trim().parse().map_err(|_| DesktopError { code: "bad_input", message: format!("it holds a number, and \"{wanted}\" isn't one") })?;
                    msg_send![class!(NSNumber), numberWithDouble: number]
                } else {
                    text(wanted)
                };
                done(AXUIElementSetAttributeValue(element, text("AXValue"), value), "given a value")?;
                Ok(Acted { value: Some(string_attribute(element, "AXValue")), ..Acted::default() })
            }
            ElementAction::Toggle(wanted) => {
                if !matches!(role.as_str(), "AXCheckBox" | "AXSwitch" | "AXToggle" | "AXRadioButton") {
                    return Err(cannot("ticked"));
                }
                let on = |element| string_attribute(element, "AXValue") == "1";
                if *wanted != Some(on(element)) {
                    done(AXUIElementPerformAction(element, text("AXPress")), "ticked")?;
                }
                Ok(Acted { on: Some(on(element)), ..Acted::default() })
            }
            ElementAction::Expand(wanted) => {
                let Some(attribute) = ["AXExpanded", "AXDisclosing"].into_iter().find(|name| settable(element, name)) else {
                    return Err(cannot("opened or closed"));
                };
                done(AXUIElementSetAttributeValue(element, text(attribute), yes_or_no(*wanted)), "opened or closed")?;
                Ok(Acted { expanded: Some(flag_attribute(element, attribute, *wanted)), ..Acted::default() })
            }
            ElementAction::Select => {
                if settable(element, "AXSelected") {
                    done(AXUIElementSetAttributeValue(element, text("AXSelected"), yes_or_no(true)), "selected")?;
                } else if matches!(role.as_str(), "AXRadioButton" | "AXMenuItem" | "AXTab") {
                    done(AXUIElementPerformAction(element, text("AXPress")), "selected")?;
                } else {
                    return Err(cannot("selected"));
                }
                Ok(Acted::default())
            }
        }
    }
}

/// One attribute of an accessibility element, or null. The caller releases what it gets.
unsafe fn attribute(element: *mut c_void, name: &str) -> Id {
    let mut value: Id = std::ptr::null_mut();
    if unsafe { AXUIElementCopyAttributeValue(element, text(name), &mut value) } != 0 {
        return std::ptr::null_mut();
    }
    value
}

/// An attribute as text: a string as it is, a number or a yes/no as it prints. Empty when there is none.
unsafe fn string_attribute(element: *mut c_void, name: &str) -> String {
    unsafe {
        let value = attribute(element, name);
        if value.is_null() {
            return String::new();
        }
        let is_string: bool = msg_send![value, isKindOfClass: class!(NSString)];
        let is_number: bool = msg_send![value, isKindOfClass: class!(NSNumber)];
        let said = if is_string {
            string_of(value)
        } else if is_number {
            string_of(msg_send![value, stringValue])
        } else {
            String::new()
        };
        CFRelease(value as *const c_void);
        said
    }
}

unsafe fn flag_attribute(element: *mut c_void, name: &str, otherwise: bool) -> bool {
    unsafe {
        let value = attribute(element, name);
        if value.is_null() {
            return otherwise;
        }
        let is_number: bool = msg_send![value, isKindOfClass: class!(NSNumber)];
        let set: bool = if is_number { msg_send![value, boolValue] } else { otherwise };
        CFRelease(value as *const c_void);
        set
    }
}

/// Where an element is on the desktop, from its position and its size.
unsafe fn frame_of(element: *mut c_void) -> Option<Rect> {
    unsafe {
        let (position, size) = (attribute(element, "AXPosition"), attribute(element, "AXSize"));
        let mut at = Point::default();
        let mut extent = Point::default();
        // 1 and 2: the kinds of value that hold a point and a size.
        let read = !position.is_null() && !size.is_null() && AXValueGetValue(position as *const c_void, 1, (&mut at as *mut Point).cast()) && AXValueGetValue(size as *const c_void, 2, (&mut extent as *mut Point).cast());
        for value in [position, size] {
            if !value.is_null() {
                CFRelease(value as *const c_void);
            }
        }
        (read && extent.x >= 1.0 && extent.y >= 1.0).then(|| Rect { x: at.x.round() as i32, y: at.y.round() as i32, width: extent.x.round() as u32, height: extent.y.round() as u32 })
    }
}

/// macOS's role names in the words used on every platform.
pub fn mac_role(role: &str) -> String {
    let common = match role {
        "AXButton" | "AXDisclosureTriangle" | "AXMenuButton" => "button",
        "AXTextField" | "AXTextArea" | "AXSecureTextField" | "AXSearchField" => "text",
        "AXStaticText" => "label",
        "AXCheckBox" => "check_box",
        "AXRadioButton" => "radio_button",
        "AXPopUpButton" | "AXComboBox" => "combo_box",
        "AXMenuBar" => "menu_bar",
        "AXMenu" => "menu",
        "AXMenuItem" | "AXMenuBarItem" => "menu_item",
        "AXWindow" => "window",
        "AXSheet" | "AXDialog" => "dialog",
        "AXGroup" | "AXScrollArea" | "AXSplitGroup" | "AXLayoutArea" => "pane",
        "AXList" => "list",
        "AXRow" => "list_item",
        "AXCell" => "cell",
        "AXTable" => "table",
        "AXOutline" => "tree",
        "AXTabGroup" => "tab_list",
        "AXImage" => "image",
        "AXSlider" => "slider",
        "AXLink" => "link",
        "AXToolbar" => "tool_bar",
        "AXScrollBar" => "scroll_bar",
        "AXProgressIndicator" => "progress_bar",
        "AXWebArea" => "document",
        "AXIncrementor" => "spinner",
        // The rest under macOS's own name, without its prefix: AXColorWell becomes color_well.
        other => {
            let name = other.strip_prefix("AX").unwrap_or(other);
            let mut words = String::new();
            for (index, character) in name.chars().enumerate() {
                if character.is_uppercase() && index > 0 {
                    words.push('_');
                }
                words.extend(character.to_lowercase());
            }
            return words;
        }
    };
    common.to_string()
}

struct Reading {
    limits: super::TreeLimits,
    read: usize,
    cut: bool,
}

impl Reading {
    /// SAFETY: `element` is a live accessibility element.
    unsafe fn element(&mut self, element: *mut c_void, depth: u32) -> super::Element {
        self.read += 1;
        unsafe {
            let (role, name, value) = described(element);
            let mut read = super::Element {
                role: mac_role(&role),
                name,
                value,
                frame: frame_of(element),
                enabled: flag_attribute(element, "AXEnabled", true),
                focused: flag_attribute(element, "AXFocused", false),
                children: Vec::new(),
            };
            if depth >= self.limits.depth {
                return read;
            }
            let children = attribute(element, "AXChildren");
            let count: usize = if children.is_null() { 0 } else { msg_send![children, count] };
            for index in 0..count {
                if self.read >= self.limits.elements {
                    self.cut = true;
                    break;
                }
                let child: Id = msg_send![children, objectAtIndex: index];
                read.children.push(self.element(child.cast(), depth + 1));
            }
            if !children.is_null() {
                CFRelease(children as *const c_void);
            }
            read
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_get_the_common_words() {
        for (mac, common) in [("AXButton", "button"), ("AXTextField", "text"), ("AXStaticText", "label"), ("AXCheckBox", "check_box"), ("AXMenuItem", "menu_item"), ("AXWindow", "window"), ("AXGroup", "pane"), ("AXColorWell", "color_well"), ("AXUnknown", "unknown")] {
            assert_eq!(mac_role(mac), common, "{mac}");
        }
    }

    #[test]
    fn keys_have_their_mac_numbers() {
        for (key, code) in [(Key::Char('a'), 0), (Key::Char('S'), 1), (Key::Char('v'), 9), (Key::Char('b'), 11), (Key::Char('1'), 18), (Key::Char('5'), 23), (Key::Char('6'), 22), (Key::Char('0'), 29), (Key::Char('l'), 37), (Key::Char('m'), 46), (Key::Char('.'), 47), (Key::Char('`'), 50), (Key::Enter, 36), (Key::Escape, 53), (Key::Meta, 55), (Key::F(1), 122), (Key::F(5), 96), (Key::F(12), 111), (Key::Left, 123), (Key::Up, 126)] {
            assert_eq!(key_code(key), Some(code), "{key:?}");
        }
        assert_eq!(key_code(Key::Char('é')), None, "typed as text, not pressed as a key");
        assert_eq!(key_code(Key::Insert), None);
        assert_eq!(key_code(Key::F(21)), None);
        assert_eq!((modifier_flag(Key::Meta), modifier_flag(Key::Char('a'))), (COMMAND, 0));
    }
}

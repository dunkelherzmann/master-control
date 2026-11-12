//! The window on macOS: one AppKit window whose whole face is the picture the program draws. AppKit and Core Graphics
//! ship with macOS; they are reached through the Objective-C runtime, with no toolkit in between.

// The Objective-C helper's macros test a setting newer compilers don't know; that is theirs to fix, not a problem here.
#![allow(unexpected_cfgs)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;

use objc::declare::ClassDecl;
use objc::runtime::{Object, Sel, BOOL, NO, YES};

use crate::canvas::Canvas;
use crate::ui::{Action, App, Key, Role, Work};

type Id = *mut Object;

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    origin: Point,
    size: Size,
}

// How the runtime describes these structures, so messages carrying them are sent the right way.
unsafe impl objc::Encode for Point {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGPoint=dd}") }
    }
}

unsafe impl objc::Encode for Size {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGSize=dd}") }
    }
}

unsafe impl objc::Encode for Rect {
    fn encode() -> objc::Encoding {
        unsafe { objc::Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "CoreGraphics", kind = "framework")]
#[allow(non_upper_case_globals)]
extern "C" {
    /// The name of the sRGB colour space: the one the screens' colours are written in.
    static kCGColorSpaceSRGB: *const c_void;
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGDataProviderCreateWithData(info: *mut c_void, data: *const c_void, size: usize, release: *const c_void) -> *mut c_void;
    fn CGDataProviderRelease(provider: *mut c_void);
    #[allow(clippy::too_many_arguments)]
    fn CGImageCreate(width: usize, height: usize, bits_per_component: usize, bits_per_pixel: usize, bytes_per_row: usize, space: *mut c_void, bitmap_info: u32, provider: *mut c_void, decode: *const f64, interpolate: bool, intent: i32) -> *mut c_void;
    fn CGImageRelease(image: *mut c_void);
    fn CGContextDrawImage(context: *mut c_void, rect: Rect, image: *mut c_void);
}

/// Each pixel is one 32-bit number, 0x00RRGGBB, in the machine's byte order: "little-endian, skip the first byte".
const PIXEL_LAYOUT: u32 = (2 << 12) | 6;

thread_local! {
    /// The view's methods are plain functions; this is how they reach the app. Only the main thread uses it.
    static STATE: RefCell<Option<(App, Canvas, f64)>> = const { RefCell::new(None) };
}

fn with<T>(act: impl FnOnce(&mut App, &mut Canvas, f64) -> T) -> Option<T> {
    STATE.with(|state| state.borrow_mut().as_mut().map(|(app, canvas, scale)| act(app, canvas, *scale)))
}

fn text(string: &str) -> Id {
    let bytes = std::ffi::CString::new(string).expect("no zero bytes");
    unsafe { msg_send![class!(NSString), stringWithUTF8String: bytes.as_ptr()] }
}

fn redraw(view: &Object) {
    unsafe {
        let _: () = msg_send![view, setNeedsDisplay: YES];
    }
}

/// Where an event happened, in the picture's pixels: AppKit counts in points from the bottom, the picture in
/// pixels from the top.
fn place(view: &Object, event: Id) -> (i32, i32) {
    let (at, bounds): (Point, Rect) = unsafe { (msg_send![event, locationInWindow], msg_send![view, bounds]) };
    with(|_, _, scale| ((at.x * scale) as i32, ((bounds.size.height - at.y) * scale) as i32)).unwrap_or((-1, -1))
}

extern "C" fn draw(view: &Object, _: Sel, _dirty: Rect) {
    unsafe {
        let bounds: Rect = msg_send![view, bounds];
        let current: Id = msg_send![class!(NSGraphicsContext), currentContext];
        let context: *mut c_void = msg_send![current, CGContext];
        with(|app, canvas, _| {
            app.draw(canvas);
            // SAFETY: the provider reads the canvas's pixels only during the draw below, while the canvas is borrowed.
            let provider = CGDataProviderCreateWithData(std::ptr::null_mut(), canvas.pixels.as_ptr().cast(), canvas.pixels.len() * 4, std::ptr::null());
            // sRGB, said outright: left to the display's own space, a wide-colour screen shows the colours washed.
            let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
            let image = CGImageCreate(canvas.width, canvas.height, 8, 32, canvas.width * 4, space, PIXEL_LAYOUT, provider, std::ptr::null(), false, 0);
            CGContextDrawImage(context, bounds, image);
            CGImageRelease(image);
            CGColorSpaceRelease(space);
            CGDataProviderRelease(provider);
        });
    }
}

extern "C" fn mouse_up(view: &Object, _: Sel, event: Id) {
    let (x, y) = place(view, event);
    with(|app, _, _| app.click(x, y));
    redraw(view);
}

extern "C" fn mouse_moved(view: &Object, _: Sel, event: Id) {
    let (x, y) = place(view, event);
    with(|app, _, _| app.pointer(x, y));
    redraw(view);
}

extern "C" fn key_down(view: &Object, _: Sel, event: Id) {
    let typed = unsafe {
        let characters: Id = msg_send![event, characters];
        let bytes: *const std::os::raw::c_char = msg_send![characters, UTF8String];
        if bytes.is_null() { String::new() } else { std::ffi::CStr::from_ptr(bytes).to_string_lossy().into_owned() }
    };
    for character in typed.chars() {
        let key = match character {
            '\u{7f}' | '\u{8}' => Key::Backspace,
            '\r' | '\n' | '\u{3}' => Key::Enter,
            '\u{1b}' => Key::Escape,
            other if !other.is_control() => Key::Character(other),
            _ => continue,
        };
        with(|app, _, _| app.key(key));
    }
    redraw(view);
}

/// "About Master Control" in the app's menu.
extern "C" fn show_about(view: &Object, _: Sel, _sender: Id) {
    with(|app, _, _| app.act(Action::About));
    redraw(view);
}

extern "C" fn tick(view: &Object, _: Sel, _timer: Id) {
    if with(|app, _, _| app.changed()) == Some(true) {
        redraw(view);
    }
}

extern "C" fn yes(_: &Object, _: Sel) -> BOOL {
    YES
}

/// The window was closed: with its one window gone, the program ends.
extern "C" fn closing(_: &Object, _: Sel, _notification: Id) {
    unsafe {
        let app: Id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, terminate: std::ptr::null_mut::<Object>()];
    }
}

pub fn run(work: Arc<dyn Work>) -> Result<(), String> {
    // SAFETY: AppKit is used from the main thread only, which is where the program starts; every object made here is
    // owned by AppKit or lives until the program ends.
    unsafe {
        let application: Id = msg_send![class!(NSApplication), sharedApplication];
        // 0: an ordinary app, with a Dock icon and a menu bar.
        let _: BOOL = msg_send![application, setActivationPolicy: 0i64];
        let screen: Id = msg_send![class!(NSScreen), mainScreen];
        if screen.is_null() {
            return Err("there is no screen to open a window on".into());
        }
        let scale: f64 = msg_send![screen, backingScaleFactor];
        let mut app = App::new(work, scale as f32);
        // For trying the window out without a hand on the mouse: open on one role's menu.
        match std::env::var("MC_START_SCREEN").as_deref() {
            Ok("master") => app.act(Action::Choose(Role::Master)),
            Ok("worker") => app.act(Action::Choose(Role::Worker)),
            _ => {}
        }
        let (width, height) = app.size();
        let frame = Rect { origin: Point { x: 0.0, y: 0.0 }, size: Size { width: width as f64 / scale, height: height as f64 / scale } };
        STATE.with(|state| *state.borrow_mut() = Some((app, Canvas::new(width, height), scale)));

        let mut view_class = ClassDecl::new("MasterControlView", class!(NSView)).ok_or("the view's class couldn't be made")?;
        view_class.add_method(sel!(drawRect:), draw as extern "C" fn(&Object, Sel, Rect));
        view_class.add_method(sel!(mouseUp:), mouse_up as extern "C" fn(&Object, Sel, Id));
        view_class.add_method(sel!(mouseMoved:), mouse_moved as extern "C" fn(&Object, Sel, Id));
        view_class.add_method(sel!(keyDown:), key_down as extern "C" fn(&Object, Sel, Id));
        view_class.add_method(sel!(tick:), tick as extern "C" fn(&Object, Sel, Id));
        view_class.add_method(sel!(showAbout:), show_about as extern "C" fn(&Object, Sel, Id));
        view_class.add_method(sel!(acceptsFirstResponder), yes as extern "C" fn(&Object, Sel) -> BOOL);
        view_class.add_method(sel!(windowWillClose:), closing as extern "C" fn(&Object, Sel, Id));
        let view_class = view_class.register();

        // Titled, closable, can be minimized; not resizable: the screens are drawn for one size.
        let window: Id = msg_send![class!(NSWindow), alloc];
        let window: Id = msg_send![window, initWithContentRect: frame styleMask: 7u64 backing: 2u64 defer: NO];
        let _: () = msg_send![window, setTitle: text("Master Control")];
        let _: () = msg_send![window, center];
        let view: Id = msg_send![view_class, alloc];
        let view: Id = msg_send![view, initWithFrame: frame];
        let _: () = msg_send![window, setContentView: view];
        let _: BOOL = msg_send![window, makeFirstResponder: view];
        let _: () = msg_send![window, setAcceptsMouseMovedEvents: YES];
        let _: () = msg_send![window, setDelegate: view];

        // The menu bar keeps only what macOS always shows: the app's own menu, with About and Quit.
        let bar: Id = msg_send![class!(NSMenu), new];
        let holder: Id = msg_send![class!(NSMenuItem), new];
        let _: () = msg_send![bar, addItem: holder];
        let menu: Id = msg_send![class!(NSMenu), new];
        let about: Id = msg_send![class!(NSMenuItem), alloc];
        let about: Id = msg_send![about, initWithTitle: text("About Master Control") action: sel!(showAbout:) keyEquivalent: text("")];
        let _: () = msg_send![about, setTarget: view];
        let _: () = msg_send![menu, addItem: about];
        let separator: Id = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: separator];
        let quit: Id = msg_send![class!(NSMenuItem), alloc];
        let quit: Id = msg_send![quit, initWithTitle: text("Quit Master Control") action: sel!(terminate:) keyEquivalent: text("q")];
        let _: () = msg_send![menu, addItem: quit];
        let _: () = msg_send![holder, setSubmenu: menu];
        let _: () = msg_send![application, setMainMenu: bar];

        let _: Id = msg_send![class!(NSTimer), scheduledTimerWithTimeInterval: 0.15f64 target: view selector: sel!(tick:) userInfo: std::ptr::null_mut::<Object>() repeats: YES];
        if std::env::var_os("MC_NO_ACTIVATE").is_some() {
            // Shown without taking the keyboard from whatever the user is doing.
            let _: () = msg_send![window, orderFrontRegardless];
        } else {
            let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null_mut::<Object>()];
            let _: () = msg_send![application, activateIgnoringOtherApps: YES];
        }
        let _: () = msg_send![application, run];
    }
    Ok(())
}

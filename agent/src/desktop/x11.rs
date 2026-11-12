//! Linux on X11: pictures with GetImage, input with the XTEST extension, windows through the window manager's EWMH
//! properties (or the server's own window list when no window manager runs).

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask, ImageFormat, InputFocus, MapState,
    StackMode, Window, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use super::{Button, Desktop, DesktopError, Image, Key, Rect, Result, WindowInfo};

const SHIFT_L: u32 = 0xffe1;

pub struct X11Desktop {
    connection: RustConnection,
    root: Window,
    width: u16,
    height: u16,
}

pub(super) fn failed<E: std::fmt::Display>(error: E) -> DesktopError {
    DesktopError::failed(format!("X11: {error}"))
}

impl X11Desktop {
    pub fn open() -> Result<X11Desktop> {
        let (connection, screen) =
            x11rb::connect(None).map_err(|error| DesktopError::not_supported(format!("can't reach the X display: {error}")))?;
        let screen = &connection.setup().roots[screen];
        let (root, width, height) = (screen.root, screen.width_in_pixels, screen.height_in_pixels);
        connection
            .xtest_get_version(2, 2)
            .map_err(failed)?
            .reply()
            .map_err(|_| DesktopError::not_supported("the X server has no XTEST extension, so input can't be sent"))?;
        Ok(X11Desktop { connection, root, width, height })
    }

    fn atom(&self, name: &str) -> Result<Atom> {
        Ok(self.connection.intern_atom(false, name.as_bytes()).map_err(failed)?.reply().map_err(failed)?.atom)
    }

    /// A property's bytes; empty when the window has no such property (or is gone).
    fn property(&self, window: Window, name: &str) -> Vec<u8> {
        let Ok(atom) = self.atom(name) else { return Vec::new() };
        self.connection
            .get_property(false, window, atom, AtomEnum::ANY, 0, u32::MAX / 4)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.value)
            .unwrap_or_default()
    }

    fn property_numbers(&self, window: Window, name: &str) -> Vec<u32> {
        self.property(window, name).as_chunks::<4>().0.iter().map(|bytes| u32::from_ne_bytes(*bytes)).collect()
    }

    /// Whether a window manager that follows EWMH is running.
    fn window_manager(&self) -> bool {
        !self.property_numbers(self.root, "_NET_SUPPORTING_WM_CHECK").is_empty()
    }

    /// Waits for the server to have handled everything sent so far.
    fn settle(&self) -> Result<()> {
        self.connection.get_input_focus().map_err(failed)?.reply().map_err(failed)?;
        Ok(())
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<()> {
        self.connection.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, x, y, 0).map_err(failed)?;
        self.connection.flush().map_err(failed)
    }

    /// The keyboard as it is now: the first keycode, how many symbols each keycode has, and all the symbols.
    fn keyboard(&self) -> Result<(u8, usize, Vec<u32>)> {
        let setup = self.connection.setup();
        let (first, last) = (setup.min_keycode, setup.max_keycode);
        let reply = self.connection.get_keyboard_mapping(first, last - first + 1).map_err(failed)?.reply().map_err(failed)?;
        Ok((first, usize::from(reply.keysyms_per_keycode), reply.keysyms))
    }

    /// The keycode that gives this symbol, and whether Shift must be held for it.
    fn keycode_for(&self, keysym: u32) -> Result<Option<(u8, bool)>> {
        let (first, per_code, symbols) = self.keyboard()?;
        for level in 0..per_code.min(2) {
            if let Some(index) = symbols.chunks(per_code).position(|group| group[level] == keysym) {
                // A key whose two levels are the same symbol (keypad keys) needs no Shift.
                let shift = level == 1 && symbols[index * per_code] != keysym;
                return Ok(Some((first + index as u8, shift)));
            }
        }
        Ok(None)
    }

    fn press(&self, keycode: u8, shift: bool) -> Result<()> {
        let shift_code = if shift { self.keycode_for(SHIFT_L)?.map(|(code, _)| code) } else { None };
        if let Some(code) = shift_code {
            self.fake(KEY_PRESS_EVENT, code, 0, 0)?;
        }
        self.fake(KEY_PRESS_EVENT, keycode, 0, 0)?;
        self.fake(KEY_RELEASE_EVENT, keycode, 0, 0)?;
        if let Some(code) = shift_code {
            self.fake(KEY_RELEASE_EVENT, code, 0, 0)?;
        }
        Ok(())
    }

    /// Types a symbol the keyboard layout has no key for, by lending it a keycode nothing uses for a moment.
    fn press_unmapped(&self, keysym: u32) -> Result<()> {
        let (first, per_code, symbols) = self.keyboard()?;
        let spare = symbols
            .chunks(per_code)
            .rposition(|group| group.iter().all(|symbol| *symbol == 0))
            .ok_or_else(|| DesktopError::failed("X11: the keyboard has no unused keycode to type this character with"))?;
        let keycode = first + spare as u8;
        self.connection.change_keyboard_mapping(1, keycode, per_code as u8, &vec![keysym; per_code]).map_err(failed)?;
        self.settle()?;
        // Programs learn of the change through an event and look the key up when they come to handle it, which can be
        // a while after it was sent. So: time to take the change in before the key, and time to handle the key before
        // the keycode is taken back. With less, GTK 4 drops the character.
        std::thread::sleep(Duration::from_millis(60));
        let typed = self.press(keycode, false).and_then(|()| self.settle());
        std::thread::sleep(Duration::from_millis(120));
        self.connection.change_keyboard_mapping(1, keycode, per_code as u8, &vec![0; per_code]).map_err(failed)?;
        self.settle()?;
        typed
    }

    fn describe(&self, window: Window, focused: Window) -> Option<WindowInfo> {
        let geometry = self.connection.get_geometry(window).ok()?.reply().ok()?;
        let place = self.connection.translate_coordinates(window, self.root, 0, 0).ok()?.reply().ok()?;
        let mut title = self.property(window, "_NET_WM_NAME");
        if title.is_empty() {
            title = self.property(window, "WM_NAME");
        }
        // WM_CLASS holds two names, each ended by a zero byte: the instance, then the class.
        let class = self.property(window, "WM_CLASS");
        let app = class.split(|byte| *byte == 0).rfind(|part| !part.is_empty()).unwrap_or_default();
        let hidden = self.atom("_NET_WM_STATE_HIDDEN").ok()?;
        Some(WindowInfo {
            id: u64::from(window),
            title: String::from_utf8_lossy(&title).into_owned(),
            app: String::from_utf8_lossy(app).into_owned(),
            pid: self.property_numbers(window, "_NET_WM_PID").first().copied().unwrap_or(0),
            frame: Rect { x: i32::from(place.dst_x), y: i32::from(place.dst_y), width: u32::from(geometry.width), height: u32::from(geometry.height) },
            focused: window == focused,
            minimized: self.property_numbers(window, "_NET_WM_STATE").contains(&hidden),
        })
    }
}

fn keysym(key: Key) -> u32 {
    match key {
        Key::Char(character) => character_keysym(character),
        Key::Enter => 0xff0d,
        Key::Tab => 0xff09,
        Key::Escape => 0xff1b,
        Key::Backspace => 0xff08,
        Key::Delete => 0xffff,
        Key::Insert => 0xff63,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::Left => 0xff51,
        Key::Up => 0xff52,
        Key::Right => 0xff53,
        Key::Down => 0xff54,
        Key::Space => 0x20,
        Key::F(number) => 0xffbe + u32::from(number) - 1,
        Key::Ctrl => 0xffe3,
        Key::Shift => SHIFT_L,
        Key::Alt => 0xffe9,
        Key::Meta => 0xffeb,
        Key::CapsLock => 0xffe5,
        Key::PrintScreen => 0xff61,
        Key::Menu => 0xff67,
    }
}

/// The X symbol for a character: Latin-1 as it is, everything else as Unicode.
fn character_keysym(character: char) -> u32 {
    match character {
        '\n' | '\r' => 0xff0d,
        '\t' => 0xff09,
        ' '..='~' | '\u{a0}'..='\u{ff}' => character as u32,
        _ => 0x0100_0000 | character as u32,
    }
}

impl Desktop for X11Desktop {
    fn bounds(&self) -> Result<Rect> {
        Ok(Rect { x: 0, y: 0, width: u32::from(self.width), height: u32::from(self.height) })
    }

    fn capture(&self, area: Rect) -> Result<Image> {
        let reply = self
            .connection
            .get_image(ImageFormat::Z_PIXMAP, self.root, area.x as i16, area.y as i16, area.width as u16, area.height as u16, !0)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let setup = self.connection.setup();
        let bits = setup.pixmap_formats.iter().find(|format| format.depth == reply.depth).map_or(0, |format| format.bits_per_pixel);
        let pixels = (area.width * area.height) as usize;
        if bits != 32 || reply.data.len() < pixels * 4 {
            return Err(DesktopError::not_supported(format!("X11: a screen with {bits} bits a pixel at depth {} isn't handled", reply.depth)));
        }
        // Four bytes a pixel in the server's byte order; with the usual masks that is blue, green, red, unused.
        let lsb_first = setup.image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST;
        let mut rgb = Vec::with_capacity(pixels * 3);
        for pixel in reply.data.as_chunks::<4>().0.iter().take(pixels) {
            if lsb_first {
                rgb.extend([pixel[2], pixel[1], pixel[0]]);
            } else {
                rgb.extend([pixel[1], pixel[2], pixel[3]]);
            }
        }
        Ok(Image { width: area.width, height: area.height, rgb })
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        let managed = self.window_manager();
        let (ids, focused) = if managed {
            // Bottom first in the property.
            let ids = self.property_numbers(self.root, "_NET_CLIENT_LIST_STACKING");
            (ids, self.property_numbers(self.root, "_NET_ACTIVE_WINDOW").first().copied().unwrap_or(0))
        } else {
            let tree = self.connection.query_tree(self.root).map_err(failed)?.reply().map_err(failed)?;
            let shown = tree.children.into_iter().filter(|window| {
                self.connection
                    .get_window_attributes(*window)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .is_some_and(|attributes| attributes.map_state == MapState::VIEWABLE && !attributes.override_redirect)
            });
            (shown.collect(), self.connection.get_input_focus().map_err(failed)?.reply().map_err(failed)?.focus)
        };
        Ok(ids.into_iter().rev().filter_map(|window| self.describe(window, focused)).collect())
    }

    fn focus(&self, window: u64) -> Result<()> {
        let window = window as Window;
        if self.window_manager() {
            // 2 says the request comes from a tool acting for the user, which window managers honour.
            let event = ClientMessageEvent::new(32, window, self.atom("_NET_ACTIVE_WINDOW")?, [2, x11rb::CURRENT_TIME, 0, 0, 0]);
            self.connection
                .send_event(false, self.root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, event)
                .map_err(failed)?;
        } else {
            self.connection.configure_window(window, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE)).map_err(failed)?;
            self.connection.set_input_focus(InputFocus::POINTER_ROOT, window, x11rb::CURRENT_TIME).map_err(failed)?;
        }
        self.settle()
    }

    fn pointer(&self) -> Result<(i32, i32)> {
        let reply = self.connection.query_pointer(self.root).map_err(failed)?.reply().map_err(failed)?;
        Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
    }

    fn move_pointer(&self, x: i32, y: i32) -> Result<()> {
        // Detail 0: the position is absolute.
        self.fake(MOTION_NOTIFY_EVENT, 0, x as i16, y as i16)?;
        self.settle()
    }

    fn button(&self, button: Button, down: bool) -> Result<()> {
        let detail = match button {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 3,
        };
        self.fake(if down { BUTTON_PRESS_EVENT } else { BUTTON_RELEASE_EVENT }, detail, 0, 0)?;
        self.settle()
    }

    fn scroll(&self, right: i32, down: i32) -> Result<()> {
        // The wheel is buttons 4 to 7 on X11: up, down, left, right, one click a notch.
        for (amount, towards, away) in [(down, 5, 4), (right, 7, 6)] {
            let detail = if amount > 0 { towards } else { away };
            for _ in 0..amount.unsigned_abs() {
                self.fake(BUTTON_PRESS_EVENT, detail, 0, 0)?;
                self.fake(BUTTON_RELEASE_EVENT, detail, 0, 0)?;
            }
        }
        self.settle()
    }

    fn key(&self, key: Key, down: bool) -> Result<()> {
        let symbol = keysym(key);
        let (keycode, _) = self
            .keycode_for(symbol)?
            .ok_or_else(|| DesktopError::failed(format!("X11: this keyboard layout has no key for {key:?}; use type_text for the character")))?;
        self.fake(if down { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT }, keycode, 0, 0)?;
        self.settle()
    }

    fn tree(&self, window: &WindowInfo, limits: super::TreeLimits) -> Result<(super::Element, bool)> {
        super::atspi::tree(window, limits)
    }

    fn act(&self, window: &WindowInfo, target: super::Target, action: &super::ElementAction) -> Result<super::Acted> {
        super::atspi::act(window, target, action)
    }

    fn clipboard(&self) -> Result<Option<String>> {
        super::x11_clipboard::read(&self.connection, self.root)
    }

    fn set_clipboard(&self, text: &str) -> Result<()> {
        super::x11_clipboard::own(text)
    }

    fn text(&self, text: &str) -> Result<()> {
        for character in text.chars() {
            let symbol = character_keysym(character);
            match self.keycode_for(symbol)? {
                Some((keycode, shift)) => self.press(keycode, shift)?,
                None => self.press_unmapped(symbol)?,
            }
        }
        self.settle()
    }
}

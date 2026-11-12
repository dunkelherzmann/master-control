//! The window on Linux: plain X11, spoken directly, so the program needs no library from the system.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, ImageFormat, PropMode, WindowClass};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

use crate::canvas::Canvas;
use crate::canvas::Picture;
use crate::ui::{App, Key, Work};

pub fn run(work: std::sync::Arc<dyn Work>) -> Result<(), String> {
    let problem = |error: &dyn std::fmt::Display| format!("the display: {error}");
    let (connection, screen) = x11rb::connect(None).map_err(|error| format!("there is no display to open a window on ({error})"))?;
    let screen = connection.setup().roots[screen].clone();
    if screen.root_depth != 24 && screen.root_depth != 32 {
        return Err(format!("a screen with {} bits of colour isn't handled", screen.root_depth));
    }
    // The display's zoom, as desktops publish it: Xft.dpi among the root window's resources, 96 meaning none.
    let resources = connection.get_property(false, screen.root, AtomEnum::RESOURCE_MANAGER, AtomEnum::STRING, 0, 1 << 20).ok().and_then(|cookie| cookie.reply().ok()).map(|reply| String::from_utf8_lossy(&reply.value).into_owned()).unwrap_or_default();
    let dpi = resources.lines().find_map(|line| line.strip_prefix("Xft.dpi:")).and_then(|value| value.trim().parse::<f32>().ok()).unwrap_or(96.0);
    let mut app = App::new(work, dpi / 96.0);
    let (width, height) = app.size();
    let window = connection.generate_id().map_err(|error| problem(&error))?;
    // The press is asked for too, though only the release is acted on: X11 gives a release to the window that took
    // the press.
    let events = EventMask::EXPOSURE | EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION | EventMask::KEY_PRESS | EventMask::STRUCTURE_NOTIFY;
    let place = (i32::from(screen.width_in_pixels) - width as i32) / 2;
    connection
        .create_window(screen.root_depth, window, screen.root, place.max(0) as i16, 80, width as u16, height as u16, 0, WindowClass::INPUT_OUTPUT, screen.root_visual, &CreateWindowAux::new().event_mask(events).background_pixel(0x1C1C1E))
        .map_err(|error| problem(&error))?;
    let atom = |name: &[u8]| connection.intern_atom(false, name).ok().and_then(|cookie| cookie.reply().ok()).map_or(0, |reply| reply.atom);
    let (protocols, delete, net_name, utf8) = (atom(b"WM_PROTOCOLS"), atom(b"WM_DELETE_WINDOW"), atom(b"_NET_WM_NAME"), atom(b"UTF8_STRING"));
    let _ = connection.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, b"Master Control");
    let _ = connection.change_property8(PropMode::REPLACE, window, net_name, utf8, b"Master Control");
    let _ = connection.change_property8(PropMode::REPLACE, window, AtomEnum::WM_CLASS, AtomEnum::STRING, b"master-control\0Master Control\0");
    let _ = connection.change_property32(PropMode::REPLACE, window, protocols, AtomEnum::ATOM, &[delete]);
    // A fixed size: the smallest and the largest it may be are the same. (Flags: minimum and maximum size given.)
    let (w, h) = (width as u32, height as u32);
    let _ = connection.change_property32(PropMode::REPLACE, window, AtomEnum::WM_NORMAL_HINTS, AtomEnum::WM_SIZE_HINTS, &[0x30, 0, 0, 0, 0, w, h, w, h, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    // The icon, for the window list and the task switcher: its size, then one value a pixel, opacity first.
    if let Some(icon) = Picture::from_png(include_bytes!("../assets/icon.png")) {
        let mut data = vec![icon.width as u32, icon.height as u32];
        data.extend(icon.rgba.as_chunks::<4>().0.iter().map(|pixel| u32::from_be_bytes([pixel[3], pixel[0], pixel[1], pixel[2]])));
        let _ = connection.change_property32(PropMode::REPLACE, window, atom(b"_NET_WM_ICON"), AtomEnum::CARDINAL, &data);
    }
    let context = connection.generate_id().map_err(|error| problem(&error))?;
    connection.create_gc(context, window, &CreateGCAux::new()).map_err(|error| problem(&error))?;
    connection.map_window(window).map_err(|error| problem(&error))?;
    connection.flush().map_err(|error| problem(&error))?;

    // Each frame is put together off screen and shown in one step, so a half-drawn one is never seen.
    let frame = connection.generate_id().map_err(|error| problem(&error))?;
    connection.create_pixmap(screen.root_depth, frame, window, width as u16, height as u16).map_err(|error| problem(&error))?;
    let mut canvas = Canvas::new(width, height);
    let mut dirty = true;
    let mut checked = Instant::now();
    loop {
        while let Some(event) = connection.poll_for_event().map_err(|error| problem(&error))? {
            match event {
                Event::Expose(_) => dirty = true,
                Event::MotionNotify(motion) => {
                    app.pointer(i32::from(motion.event_x), i32::from(motion.event_y));
                    dirty = true;
                }
                Event::ButtonRelease(button) if button.detail == 1 => {
                    app.click(i32::from(button.event_x), i32::from(button.event_y));
                    dirty = true;
                }
                Event::KeyPress(press) => {
                    let symbols = connection.get_keyboard_mapping(press.detail, 1).ok().and_then(|cookie| cookie.reply().ok()).map(|reply| reply.keysyms).unwrap_or_default();
                    let key = symbols.iter().find_map(|symbol| match *symbol {
                        0x30..=0x39 => char::from_u32(*symbol).map(Key::Character),
                        // The number pad's digits.
                        0xffb0..=0xffb9 => char::from_u32(symbol - 0xffb0 + 0x30).map(Key::Character),
                        0xff08 => Some(Key::Backspace),
                        0xff0d | 0xff8d => Some(Key::Enter),
                        0xff1b => Some(Key::Escape),
                        _ => None,
                    });
                    if let Some(key) = key {
                        app.key(key);
                        dirty = true;
                    }
                }
                Event::ClientMessage(message) if message.data.as_data32()[0] == delete => return Ok(()),
                Event::DestroyNotify(_) => return Ok(()),
                _ => {}
            }
        }
        if checked.elapsed() > Duration::from_millis(150) {
            checked = Instant::now();
            dirty |= app.changed();
        }
        if dirty {
            dirty = false;
            app.draw(&mut canvas);
            // Sent in strips: one request has a size limit, and the whole picture is over it on some servers.
            const ROWS: usize = 64;
            for (strip, rows) in canvas.pixels.chunks(width * ROWS).enumerate() {
                let bytes: Vec<u8> = rows.iter().flat_map(|pixel| pixel.to_le_bytes()).collect();
                connection
                    .put_image(ImageFormat::Z_PIXMAP, frame, context, width as u16, (rows.len() / width) as u16, 0, (strip * ROWS) as i16, 0, screen.root_depth, &bytes)
                    .map_err(|error| problem(&error))?;
            }
            connection.copy_area(frame, window, context, 0, 0, 0, 0, width as u16, height as u16).map_err(|error| problem(&error))?;
            connection.flush().map_err(|error| problem(&error))?;
        }
        std::thread::sleep(Duration::from_millis(12));
    }
}

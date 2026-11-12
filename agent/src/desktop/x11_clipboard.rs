//! The X11 clipboard. Reading asks whoever owns the CLIPBOARD selection for its text. Setting means becoming the
//! owner and answering such requests for as long as nobody else takes over, so a thread stays behind to do that.

use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property, SelectionNotifyEvent, Window, WindowClass,
    SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::x11::failed;
use super::{DesktopError, Result};

const WAIT: Duration = Duration::from_secs(3);

fn atom(connection: &RustConnection, name: &str) -> Result<Atom> {
    Ok(connection.intern_atom(false, name.as_bytes()).map_err(failed)?.reply().map_err(failed)?.atom)
}

/// A window nobody sees, to receive the answer on or to own the selection with.
fn hidden_window(connection: &RustConnection, root: Window) -> Result<Window> {
    let window = connection.generate_id().map_err(failed)?;
    connection
        .create_window(0, window, root, -10, -10, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE))
        .map_err(failed)?;
    Ok(window)
}

/// Waits for the next event that `wanted` accepts.
fn next<T>(connection: &RustConnection, mut wanted: impl FnMut(Event) -> Option<T>) -> Result<T> {
    let deadline = Instant::now() + WAIT;
    loop {
        match connection.poll_for_event().map_err(failed)? {
            Some(event) => {
                if let Some(found) = wanted(event) {
                    return Ok(found);
                }
            }
            None if Instant::now() > deadline => return Err(DesktopError::failed("X11: the clipboard's owner didn't answer")),
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

pub fn read(connection: &RustConnection, root: Window) -> Result<Option<String>> {
    let clipboard = atom(connection, "CLIPBOARD")?;
    if connection.get_selection_owner(clipboard).map_err(failed)?.reply().map_err(failed)?.owner == x11rb::NONE {
        return Ok(None);
    }
    let window = hidden_window(connection, root)?;
    let outcome = read_into(connection, window, clipboard);
    let _ = connection.destroy_window(window);
    let _ = connection.flush();
    outcome
}

fn read_into(connection: &RustConnection, window: Window, clipboard: Atom) -> Result<Option<String>> {
    let (utf8, property, incremental) = (atom(connection, "UTF8_STRING")?, atom(connection, "MC_CLIPBOARD")?, atom(connection, "INCR")?);
    connection.convert_selection(window, clipboard, utf8, property, x11rb::CURRENT_TIME).map_err(failed)?;
    connection.flush().map_err(failed)?;
    let answered = next(connection, |event| match event {
        Event::SelectionNotify(notify) if notify.requestor == window => Some(notify.property),
        _ => None,
    })?;
    if answered == x11rb::NONE {
        // The owner has something, but not as text.
        return Ok(None);
    }
    let take = || -> Result<(Atom, Vec<u8>)> {
        let reply = connection.get_property(true, window, property, AtomEnum::ANY, 0, u32::MAX / 4).map_err(failed)?.reply().map_err(failed)?;
        Ok((reply.type_, reply.value))
    };
    let (kind, mut bytes) = take()?;
    if kind == incremental {
        // Too large for one piece: the owner sends parts, each announced by a property change, ending with an empty one.
        bytes.clear();
        loop {
            next(connection, |event| match event {
                Event::PropertyNotify(change) if change.window == window && change.atom == property && change.state == Property::NEW_VALUE => Some(()),
                _ => None,
            })?;
            let (_, part) = take()?;
            if part.is_empty() {
                break;
            }
            bytes.extend(part);
        }
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Takes the clipboard with this text and keeps answering for it on a thread of its own, until another program copies
/// something.
pub fn own(text: &str) -> Result<()> {
    let (connection, screen) = x11rb::connect(None).map_err(failed)?;
    let root = connection.setup().roots[screen].root;
    if text.len() + 1024 > connection.maximum_request_bytes() {
        return Err(DesktopError::failed("X11: the text is too large for this X server to hand over in one piece"));
    }
    let window = hidden_window(&connection, root)?;
    let clipboard = atom(&connection, "CLIPBOARD")?;
    let targets = atom(&connection, "TARGETS")?;
    let text_kinds = [atom(&connection, "UTF8_STRING")?, atom(&connection, "text/plain;charset=utf-8")?, atom(&connection, "TEXT")?, AtomEnum::STRING.into()];
    connection.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME).map_err(failed)?;
    if connection.get_selection_owner(clipboard).map_err(failed)?.reply().map_err(failed)?.owner != window {
        return Err(DesktopError::failed("X11: the clipboard couldn't be taken"));
    }
    let text = text.as_bytes().to_vec();
    std::thread::spawn(move || {
        // Ends when another program takes the clipboard, or when the display goes away.
        while let Ok(event) = connection.wait_for_event() {
            match event {
                Event::SelectionClear(_) => break,
                Event::SelectionRequest(request) => {
                    // Old programs leave the property empty and mean "use the target's name".
                    let property = if request.property == x11rb::NONE { request.target } else { request.property };
                    let stored = if request.target == targets {
                        let offered: Vec<Atom> = std::iter::once(targets).chain(text_kinds).collect();
                        connection.change_property32(PropMode::REPLACE, request.requestor, property, AtomEnum::ATOM, &offered).is_ok()
                    } else if text_kinds.contains(&request.target) {
                        connection.change_property8(PropMode::REPLACE, request.requestor, property, request.target, &text).is_ok()
                    } else {
                        false
                    };
                    let answer = SelectionNotifyEvent {
                        response_type: SELECTION_NOTIFY_EVENT,
                        sequence: 0,
                        time: request.time,
                        requestor: request.requestor,
                        selection: request.selection,
                        target: request.target,
                        property: if stored { property } else { x11rb::NONE },
                    };
                    let _ = connection.send_event(false, request.requestor, EventMask::NO_EVENT, answer);
                    let _ = connection.flush();
                }
                _ => {}
            }
        }
    });
    Ok(())
}

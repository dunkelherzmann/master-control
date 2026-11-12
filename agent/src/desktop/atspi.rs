//! What is in a window on Linux, read through AT-SPI: the accessibility bus that GTK, Qt and other toolkits put their
//! windows' contents on.

use zbus::blocking::connection::Builder;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::{common_role, DesktopError, Element, Rect, Result, TreeLimits, WindowInfo};

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// Bits of the state set, as AT-SPI numbers them.
const STATE_FOCUSED: u32 = 12;
const STATE_SENSITIVE: u32 = 24;
/// Positions relative to the window, which every toolkit can give; desktop positions some can't.
const RELATIVE_TO_WINDOW: u32 = 1;

/// An object on the accessibility bus: the app's connection name and the object's path in it.
type Reference = (String, OwnedObjectPath);

fn failed<E: std::fmt::Display>(error: E) -> DesktopError {
    DesktopError::failed(format!("AT-SPI: {error}"))
}

struct Bus {
    connection: Connection,
}

impl Bus {
    fn open() -> Result<Bus> {
        let unavailable = |error: zbus::Error| {
            DesktopError::not_supported(format!("the accessibility bus can't be reached, so windows' contents can't be read: {error}"))
        };
        let session = Connection::session().map_err(unavailable)?;
        // Many apps (Qt, Chromium, Electron) only describe their windows once something says it is listening.
        let _ = session.call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some(PROPERTIES), "Set", &("org.a11y.Status", "IsEnabled", Value::from(true)));
        let address: String = session
            .call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())
            .map_err(unavailable)?
            .body()
            .deserialize()
            .map_err(unavailable)?;
        let connection = Builder::address(address.as_str()).and_then(|builder| builder.build()).map_err(unavailable)?;
        Ok(Bus { connection })
    }

    fn children(&self, of: &Reference) -> Vec<Reference> {
        self.connection
            .call_method(Some(of.0.as_str()), of.1.as_str(), Some(ACCESSIBLE), "GetChildren", &())
            .ok()
            .and_then(|reply| reply.body().deserialize().ok())
            .unwrap_or_default()
    }

    fn property(&self, of: &Reference, interface: &str, name: &str) -> Option<OwnedValue> {
        self.connection
            .call_method(Some(of.0.as_str()), of.1.as_str(), Some(PROPERTIES), "Get", &(interface, name))
            .ok()
            .and_then(|reply| reply.body().deserialize().ok())
    }

    fn name(&self, of: &Reference) -> String {
        self.property(of, ACCESSIBLE, "Name").and_then(|value| String::try_from(value).ok()).unwrap_or_default()
    }

    fn call<T: serde::de::DeserializeOwned + zbus::zvariant::Type>(&self, of: &Reference, interface: &str, method: &str) -> Option<T> {
        self.connection.call_method(Some(of.0.as_str()), of.1.as_str(), Some(interface), method, &()).ok().and_then(|reply| reply.body().deserialize().ok())
    }

    fn process(&self, connection_name: &str) -> u32 {
        self.connection
            .call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "GetConnectionUnixProcessID", &(connection_name,))
            .ok()
            .and_then(|reply| reply.body().deserialize().ok())
            .unwrap_or(0)
    }

    /// The accessible object that is this window: a top-level object of the window's process with the window's title.
    fn window(&self, window: &WindowInfo) -> Option<Reference> {
        let root = ("org.a11y.atspi.Registry".to_string(), OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root").ok()?);
        let mut by_title_only = None;
        for app in self.children(&root) {
            let same_process = window.pid != 0 && self.process(&app.0) == window.pid;
            if window.pid != 0 && !same_process && by_title_only.is_some() {
                continue;
            }
            let frames = self.children(&app);
            let titled = frames.iter().find(|frame| self.name(frame) == window.title).cloned();
            match (same_process, titled) {
                (true, Some(frame)) => return Some(frame),
                // The process's only window, under a title the toolkit words differently.
                (true, None) if frames.len() == 1 => return frames.into_iter().next(),
                (false, Some(frame)) if by_title_only.is_none() => by_title_only = Some(frame),
                _ => {}
            }
        }
        // A window whose process the X server doesn't know (no _NET_WM_PID) can only be found by its title.
        by_title_only.filter(|_| window.pid == 0)
    }
}

struct Reading<'a> {
    bus: &'a Bus,
    origin: (i32, i32),
    limits: TreeLimits,
    read: usize,
    cut: bool,
}

impl Reading<'_> {
    fn element(&mut self, of: &Reference, depth: u32) -> Element {
        self.read += 1;
        let bus = self.bus;
        let states: Vec<u32> = bus.call(of, ACCESSIBLE, "GetState").unwrap_or_default();
        let has = |bit: u32| states.get((bit / 32) as usize).is_some_and(|word| word & (1 << (bit % 32)) != 0);
        let interfaces: Vec<String> = bus.call(of, ACCESSIBLE, "GetInterfaces").unwrap_or_default();
        let offers = |name: &str| interfaces.iter().any(|interface| interface == name);
        let frame = offers("org.a11y.atspi.Component")
            .then(|| {
                bus.connection
                    .call_method(Some(of.0.as_str()), of.1.as_str(), Some("org.a11y.atspi.Component"), "GetExtents", &(RELATIVE_TO_WINDOW,))
                    .ok()
                    .and_then(|reply| reply.body().deserialize::<(i32, i32, i32, i32)>().ok())
            })
            .flatten()
            .filter(|(_, _, width, height)| *width > 0 && *height > 0)
            .map(|(x, y, width, height)| Rect { x: self.origin.0 + x, y: self.origin.1 + y, width: width as u32, height: height as u32 });
        let value = if offers("org.a11y.atspi.Text") {
            // Asked for by its real length: not every toolkit takes -1 to mean "to the end".
            let length = bus.property(of, "org.a11y.atspi.Text", "CharacterCount").and_then(|value| i32::try_from(value).ok()).unwrap_or(0);
            bus.connection
                .call_method(Some(of.0.as_str()), of.1.as_str(), Some("org.a11y.atspi.Text"), "GetText", &(0i32, length))
                .ok()
                .and_then(|reply| reply.body().deserialize::<String>().ok())
        } else if offers("org.a11y.atspi.Value") {
            bus.property(of, "org.a11y.atspi.Value", "CurrentValue").and_then(|value| f64::try_from(value).ok()).map(|number| number.to_string())
        } else {
            None
        };
        let role: String = bus.call(of, ACCESSIBLE, "GetRoleName").unwrap_or_default();
        let name = bus.name(of);
        // A label's text is its name; saying it twice helps nobody.
        let value = value.filter(|value| *value != name);
        let mut read = Element {
            role: common_role(&role),
            name,
            value,
            frame,
            // "Sensitive" is what every toolkit sets on a widget that can be used; GTK 4 never sets "enabled".
            enabled: has(STATE_SENSITIVE),
            focused: has(STATE_FOCUSED),
            children: Vec::new(),
        };
        if depth >= self.limits.depth {
            return read;
        }
        for child in bus.children(of) {
            if self.read >= self.limits.elements {
                self.cut = true;
                break;
            }
            read.children.push(self.element(&child, depth + 1));
        }
        read
    }
}

const ACTION: &str = "org.a11y.atspi.Action";
const STATE_CHECKED: u32 = 4;
const STATE_EXPANDABLE: u32 = 9;
const STATE_EXPANDED: u32 = 10;
const STATE_PRESSED: u32 = 20;

impl Bus {
    fn has(&self, of: &Reference, bits: &[u32]) -> bool {
        let states: Vec<u32> = self.call(of, ACCESSIBLE, "GetState").unwrap_or_default();
        bits.iter().any(|bit| states.get((bit / 32) as usize).is_some_and(|word| word & (1 << (bit % 32)) != 0))
    }

    /// Does the first of the element's own actions that goes by one of these names; with no names, its first
    /// action. False when it has none of them.
    fn do_action(&self, of: &Reference, names: &[&str]) -> bool {
        let count = self.property(of, ACTION, "NActions").and_then(|count| i32::try_from(count).ok()).unwrap_or(0);
        let name = |index: i32| -> String {
            self.connection.call_method(Some(of.0.as_str()), of.1.as_str(), Some(ACTION), "GetName", &(index,)).ok().and_then(|reply| reply.body().deserialize().ok()).unwrap_or_default()
        };
        let chosen = if names.is_empty() { (count > 0).then_some(0) } else { names.iter().find_map(|wanted| (0..count).find(|index| name(*index).eq_ignore_ascii_case(wanted))) };
        let Some(index) = chosen else { return false };
        self.connection.call_method(Some(of.0.as_str()), of.1.as_str(), Some(ACTION), "DoAction", &(index,)).ok().and_then(|reply| reply.body().deserialize::<bool>().ok()).unwrap_or(false)
    }

    /// The names of the element's own actions, for saying what it does offer.
    fn actions(&self, of: &Reference) -> String {
        let count = self.property(of, ACTION, "NActions").and_then(|count| i32::try_from(count).ok()).unwrap_or(0);
        let names: Vec<String> = (0..count)
            .map(|index| self.connection.call_method(Some(of.0.as_str()), of.1.as_str(), Some(ACTION), "GetName", &(index,)).ok().and_then(|reply| reply.body().deserialize().ok()).unwrap_or_default())
            .collect();
        if names.is_empty() { "it offers no actions of its own".into() } else { format!("it offers: {}", names.join(", ")) }
    }

    /// Waits a moment for a state to become what was asked: toolkits act on a request when their loop next turns.
    fn settles(&self, of: &Reference, bits: &[u32], wanted: bool) -> bool {
        for _ in 0..40 {
            if self.has(of, bits) == wanted {
                return wanted;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        !wanted
    }

    fn held(&self, of: &Reference, interfaces: &[String]) -> Option<String> {
        let offers = |name: &str| interfaces.iter().any(|interface| interface == name);
        if offers("org.a11y.atspi.Text") {
            let length = self.property(of, "org.a11y.atspi.Text", "CharacterCount").and_then(|value| i32::try_from(value).ok()).unwrap_or(0);
            self.connection.call_method(Some(of.0.as_str()), of.1.as_str(), Some("org.a11y.atspi.Text"), "GetText", &(0i32, length)).ok().and_then(|reply| reply.body().deserialize::<String>().ok())
        } else if offers("org.a11y.atspi.Value") {
            self.property(of, "org.a11y.atspi.Value", "CurrentValue").and_then(|value| f64::try_from(value).ok()).map(|number| number.to_string())
        } else {
            None
        }
    }
}

/// Does something to one element of a window, found again by the way the tree led to it.
pub fn act(window: &WindowInfo, target: super::Target, action: &super::ElementAction) -> Result<super::Acted> {
    use super::{Acted, ElementAction};
    let bus = Bus::open()?;
    let Some(mut at) = bus.window(window) else {
        return Err(DesktopError::not_supported(format!("the app behind \"{}\" doesn't describe its windows to assistive technology", window.title)));
    };
    let mut parent = None;
    for &index in target.path {
        let Some(child) = bus.children(&at).into_iter().nth(index as usize) else {
            return Err(DesktopError::gone("the window no longer has it"));
        };
        parent = Some((at, index));
        at = child;
    }
    let role: String = bus.call(&at, ACCESSIBLE, "GetRoleName").unwrap_or_default();
    target.check(&common_role(&role), &bus.name(&at))?;
    let interfaces: Vec<String> = bus.call(&at, ACCESSIBLE, "GetInterfaces").unwrap_or_default();
    let offers = |name: &str| interfaces.iter().any(|interface| interface == name);
    let cannot = |what: &str| DesktopError::not_supported(format!("a {} can't be {what} ({})", target.role, bus.actions(&at)));
    match action {
        ElementAction::Press => {
            if !bus.do_action(&at, &["click", "press", "activate", "jump", "open"]) && !bus.do_action(&at, &[]) {
                return Err(cannot("pressed"));
            }
            Ok(Acted::default())
        }
        ElementAction::SetValue(wanted) => {
            let set = if offers("org.a11y.atspi.EditableText") {
                bus.connection.call_method(Some(at.0.as_str()), at.1.as_str(), Some("org.a11y.atspi.EditableText"), "SetTextContents", &(wanted.as_str(),)).map(|_| ()).map_err(failed)
            } else if offers("org.a11y.atspi.Value") {
                let number: f64 = wanted.trim().parse().map_err(|_| DesktopError { code: "bad_input", message: format!("it holds a number, and \"{wanted}\" isn't one") })?;
                bus.connection.call_method(Some(at.0.as_str()), at.1.as_str(), Some(PROPERTIES), "Set", &("org.a11y.atspi.Value", "CurrentValue", Value::from(number))).map(|_| ()).map_err(failed)
            } else {
                return Err(cannot("given a value"));
            };
            set?;
            // The toolkit takes the new value in when its loop next turns.
            let mut held = bus.held(&at, &interfaces);
            for _ in 0..20 {
                if held.as_deref() == Some(wanted.as_str()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
                held = bus.held(&at, &interfaces);
            }
            Ok(Acted { value: held, ..Acted::default() })
        }
        ElementAction::Toggle(wanted) => {
            let bits = [STATE_CHECKED, STATE_PRESSED];
            let now = bus.has(&at, &bits);
            let on = match wanted {
                Some(wanted) if *wanted == now => now,
                _ => {
                    if !bus.do_action(&at, &["toggle", "click", "press", "activate"]) {
                        // Seen with GTK 4.14's check boxes: nothing to do it with but a click.
                        let mut refusal = cannot("ticked");
                        refusal.message.push_str("; click it by its id instead, which uses the pointer");
                        return Err(refusal);
                    }
                    bus.settles(&at, &bits, !now)
                }
            };
            Ok(Acted { on: Some(on), ..Acted::default() })
        }
        ElementAction::Expand(wanted) => {
            let now = bus.has(&at, &[STATE_EXPANDED]);
            if now != *wanted {
                let able = bus.has(&at, &[STATE_EXPANDABLE, STATE_EXPANDED]) || common_role(&role) == "combo_box";
                if !able || !bus.do_action(&at, &["expand or contract", "expand or collapse", "activate", "click", "press", "toggle"]) {
                    return Err(cannot("opened or closed"));
                }
            }
            Ok(Acted { expanded: Some(bus.settles(&at, &[STATE_EXPANDED], *wanted)), ..Acted::default() })
        }
        ElementAction::Focus => {
            // One that has the keyboard already is left alone: a second click on it soon after the first would
            // be a double click, which selects a word.
            if bus.has(&at, &[STATE_FOCUSED]) {
                return Ok(Acted::default());
            }
            let taken = offers("org.a11y.atspi.Component")
                && bus.connection.call_method(Some(at.0.as_str()), at.1.as_str(), Some("org.a11y.atspi.Component"), "GrabFocus", &()).ok().and_then(|reply| reply.body().deserialize::<bool>().ok()).unwrap_or(false);
            if !taken {
                return Err(cannot("given the keyboard"));
            }
            Ok(Acted::default())
        }
        ElementAction::Select => {
            let through_parent = parent.as_ref().is_some_and(|(parent, index)| {
                let selects: Vec<String> = bus.call(parent, ACCESSIBLE, "GetInterfaces").unwrap_or_default();
                selects.iter().any(|interface| interface == "org.a11y.atspi.Selection")
                    && bus.connection.call_method(Some(parent.0.as_str()), parent.1.as_str(), Some("org.a11y.atspi.Selection"), "SelectChild", &(*index as i32,)).ok().and_then(|reply| reply.body().deserialize::<bool>().ok()).unwrap_or(false)
            });
            if !through_parent && !bus.do_action(&at, &["select", "click", "activate", "press"]) {
                return Err(cannot("selected"));
            }
            Ok(Acted::default())
        }
    }
}

pub fn tree(window: &WindowInfo, limits: TreeLimits) -> Result<(Element, bool)> {
    let bus = Bus::open()?;
    let Some(root) = bus.window(window) else {
        return Err(DesktopError::not_supported(format!(
            "the app behind \"{}\" doesn't describe its windows to assistive technology (or hasn't yet: some apps start doing so a moment after they are first asked)",
            window.title
        )));
    };
    let mut reading = Reading { bus: &bus, origin: (window.frame.x, window.frame.y), limits, read: 0, cut: false };
    let element = reading.element(&root, 0);
    if element.role.is_empty() && element.children.is_empty() {
        return Err(failed("the window stopped answering while it was read"));
    }
    Ok((element, reading.cut))
}

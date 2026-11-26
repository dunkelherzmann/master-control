//! The tools that see and drive the desktop, with the rules of specs.md §3.2: positions are those of the picture the
//! client last received, keys go only to a window that really took the focus, and nothing is reported as done that
//! wasn't.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};

use crate::desktop::{self, keys, Button, Desktop, DesktopError, Element, ElementAction, Rect, Target, TreeLimits, WindowInfo, WindowState};
use crate::tools::{number, required_text, text, ToolError};
use crate::{state, Agent};

/// How long a window gets to come to the front before the action is refused.
const FOCUS_WAIT: Duration = Duration::from_secs(3);

/// A picture a client was given: which part of the desktop it shows, and how much it was shrunk.
#[derive(Clone, Copy)]
struct Capture {
    id: u64,
    area: Rect,
    /// Picture pixels per desktop pixel; 1 when not shrunk.
    scale: f64,
}

#[derive(Default)]
pub struct Captures {
    recent: Mutex<Vec<Capture>>,
}

impl Captures {
    fn add(&self, area: Rect, scale: f64) -> u64 {
        let mut recent = self.recent.lock().unwrap();
        let id = recent.last().map_or(1, |capture| capture.id + 1);
        recent.push(Capture { id, area, scale });
        if recent.len() > 32 {
            recent.remove(0);
        }
        id
    }

    fn find(&self, id: Option<u64>) -> Result<Option<Capture>, ToolError> {
        let recent = self.recent.lock().unwrap();
        match id {
            None => Ok(recent.last().copied()),
            Some(id) => recent
                .iter()
                .find(|capture| capture.id == id)
                .copied()
                .map(Some)
                .ok_or_else(|| ToolError::bad(format!("capture {id} is too old or never existed; take a new screenshot"))),
        }
    }
}

/// A window's contents as a client was given them: where each element was, so a later action can aim at one by id.
struct StoredTree {
    id: u64,
    window: u64,
    window_frame: Rect,
    frames: Vec<Option<Rect>>,
    /// For each element: the way down to it from the window, child by child, and its role and name.
    places: Vec<(Vec<u32>, String, String)>,
}

#[derive(Default)]
pub struct Trees {
    recent: Mutex<Vec<StoredTree>>,
}

impl Trees {
    fn add(&self, window: &WindowInfo, frames: Vec<Option<Rect>>) -> u64 {
        let mut recent = self.recent.lock().unwrap();
        let id = recent.last().map_or(1, |tree| tree.id + 1);
        recent.push(StoredTree { id, window: window.id, window_frame: window.frame, frames, places: Vec::new() });
        if recent.len() > 16 {
            recent.remove(0);
        }
        id
    }

    /// The window an element id belongs to and the element's place, as they were when the tree was read.
    fn element(&self, id: &str) -> Result<(u64, Rect, Rect), ToolError> {
        let gone = |why: &str| ToolError { code: "element_gone", message: format!("element {id}: {why}; read the tree again") };
        let (tree, index) = id
            .split_once('.')
            .and_then(|(tree, index)| Some((tree.parse::<u64>().ok()?, index.parse::<usize>().ok()?)))
            .ok_or_else(|| ToolError::bad(format!("\"{id}\" isn't an element id; they look like 3.17 and come from tree or find")))?;
        let recent = self.recent.lock().unwrap();
        let stored = recent.iter().find(|stored| stored.id == tree).ok_or_else(|| gone("its tree is too old"))?;
        let frame = stored.frames.get(index).ok_or_else(|| gone("the tree has no such element"))?.ok_or_else(|| gone("it has no place on screen"))?;
        Ok((stored.window, stored.window_frame, frame))
    }

    /// The window an element id belongs to, the way down to the element, and its role and name when the tree was
    /// read.
    fn place(&self, id: &str) -> Result<(u64, Vec<u32>, String, String), ToolError> {
        let gone = |why: &str| ToolError { code: "element_gone", message: format!("element {id}: {why}; read the tree again") };
        let (tree, index) = id
            .split_once('.')
            .and_then(|(tree, index)| Some((tree.parse::<u64>().ok()?, index.parse::<usize>().ok()?)))
            .ok_or_else(|| ToolError::bad(format!("\"{id}\" isn't an element id; they look like 3.17 and come from tree or find")))?;
        let recent = self.recent.lock().unwrap();
        let stored = recent.iter().find(|stored| stored.id == tree).ok_or_else(|| gone("its tree is too old"))?;
        let (path, role, name) = stored.places.get(index).ok_or_else(|| gone("the tree has no such element"))?.clone();
        Ok((stored.window, path, role, name))
    }
}

impl From<DesktopError> for ToolError {
    fn from(error: DesktopError) -> ToolError {
        ToolError { code: error.code, message: error.message }
    }
}

fn open() -> Result<Box<dyn Desktop>, ToolError> {
    Ok(desktop::open()?)
}

fn window_json(window: &WindowInfo) -> Value {
    json!({
        "id": window.id,
        "title": window.title,
        "app": window.app,
        "pid": window.pid,
        "x": window.frame.x,
        "y": window.frame.y,
        "width": window.frame.width,
        "height": window.frame.height,
        "focused": window.focused,
        "minimized": window.minimized,
    })
}

/// Finds the window a client named: by id, by its exact title, or by a part of the title that only one window has.
fn find_window(desktop: &dyn Desktop, wanted: &Value) -> Result<WindowInfo, ToolError> {
    // A window can be missing from the list for a moment while the desktop is changing (the user switching desktops,
    // a window being moved between screens): it is looked for a few times before it is said to be gone.
    let mut tries = 0;
    loop {
        match find_window_now(desktop, wanted) {
            Err(error) if error.code == "window_not_found" && tries < 3 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            outcome => return outcome,
        }
    }
}

/// The window a client named, as the desktop is this instant.
fn find_window_now(desktop: &dyn Desktop, wanted: &Value) -> Result<WindowInfo, ToolError> {
    let windows = desktop.windows()?;
    let missing = |what: String| ToolError { code: "window_not_found", message: what };
    match wanted {
        Value::Number(id) => {
            let id = id.as_u64().ok_or_else(|| ToolError::bad("a window id is a whole number"))?;
            windows.into_iter().find(|window| window.id == id).ok_or_else(|| missing(format!("there is no window with id {id}")))
        }
        Value::String(title) if !title.is_empty() => {
            if let Some(exact) = windows.iter().find(|window| window.title == *title) {
                return Ok(exact.clone());
            }
            let lower = title.to_lowercase();
            let mut partial: Vec<&WindowInfo> = windows.iter().filter(|window| window.title.to_lowercase().contains(&lower)).collect();
            match partial.len() {
                0 => Err(missing(format!("no window's title contains \"{title}\""))),
                1 => Ok(partial.remove(0).clone()),
                _ => Err(ToolError::bad(format!(
                    "\"{title}\" matches several windows ({}); give the whole title or the id",
                    partial.iter().map(|window| format!("\"{}\"", window.title)).collect::<Vec<_>>().join(", ")
                ))),
            }
        }
        _ => Err(ToolError::bad("window must be a window's id or its title")),
    }
}

/// Brings a window to the front and waits until it really has the focus.
fn bring_forward(desktop: &dyn Desktop, window: &WindowInfo) -> Result<WindowInfo, ToolError> {
    let deadline = Instant::now() + FOCUS_WAIT;
    desktop.focus(window.id)?;
    loop {
        let now = desktop.windows()?.into_iter().find(|candidate| candidate.id == window.id);
        match now {
            None => return Err(ToolError { code: "window_not_found", message: format!("the window \"{}\" closed", window.title) }),
            Some(now) if now.focused && !now.minimized => return Ok(now),
            Some(_) if Instant::now() >= deadline => {
                // What is in front instead usually says why: a dialog asking something, another app's alert.
                let in_front = desktop.windows().ok().and_then(|windows| windows.into_iter().find(|other| other.focused));
                let instead = in_front.map_or_else(String::new, |other| format!(" (\"{}\" of {} is in front)", other.title, other.app));
                return Err(ToolError {
                    code: "focus_refused",
                    message: format!("the window \"{}\" didn't come to the front{instead}, so nothing was sent", window.title),
                });
            }
            Some(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn signed(args: &Value, name: &str) -> Result<Option<i64>, ToolError> {
    match &args[name] {
        Value::Null => Ok(None),
        value => value
            .as_i64()
            .or_else(|| value.as_f64().filter(|number| number.is_finite()).map(|number| number.round() as i64))
            .map(Some)
            .ok_or_else(|| ToolError::bad(format!("{name} must be a number"))),
    }
}

/// A position a client gave, in the pixels of the picture it has, turned into desktop pixels.
fn position(agent: &Agent, args: &Value, x_name: &str, y_name: &str) -> Result<(i32, i32), ToolError> {
    let x = signed(args, x_name)?.ok_or_else(|| ToolError::bad(format!("{x_name} is required")))?;
    let y = signed(args, y_name)?.ok_or_else(|| ToolError::bad(format!("{y_name} is required")))?;
    Ok(match agent.captures.find(number(args, "capture")?)? {
        None => (x as i32, y as i32),
        Some(capture) => (
            capture.area.x + (x as f64 / capture.scale).round() as i32,
            capture.area.y + (y as f64 / capture.scale).round() as i32,
        ),
    })
}

fn button_named(args: &Value) -> Result<Button, ToolError> {
    match text(args, "button")?.unwrap_or("left") {
        "left" => Ok(Button::Left),
        "middle" => Ok(Button::Middle),
        "right" => Ok(Button::Right),
        other => Err(ToolError::bad(format!("button is left, middle or right, not {other}"))),
    }
}

/// Moves the pointer and makes sure it arrived.
fn move_to(desktop: &dyn Desktop, x: i32, y: i32) -> Result<(), ToolError> {
    if !desktop.bounds()?.contains(x, y) {
        return Err(ToolError::bad(format!("the point {x},{y} is off the screen")));
    }
    // The move is asked of the system and takes effect a moment later; and a person at the machine may be moving
    // the pointer too. It is given time to arrive, sent once more if it didn't, and only then called a failure.
    let mut at = (x, y);
    for _ in 0..2 {
        desktop.move_pointer(x, y)?;
        for _ in 0..15 {
            at = desktop.pointer()?;
            if at == (x, y) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Err(ToolError::failed(format!("the pointer was sent to {x},{y} but is at {},{}: something else is moving it (a person at the machine?), so nothing was clicked", at.0, at.1)))
}

/// The window an action names, in front and focused, with the point checked to be inside it.
fn target(desktop: &dyn Desktop, args: &Value, points: &[(i32, i32)]) -> Result<Option<WindowInfo>, ToolError> {
    if args["window"].is_null() {
        return Ok(None);
    }
    let window = bring_forward(desktop, &find_window(desktop, &args["window"])?)?;
    if let Some((x, y)) = points.iter().find(|(x, y)| !window.frame.contains(*x, *y)) {
        return Err(ToolError::bad(format!("the point {x},{y} is outside the window \"{}\", so nothing was sent", window.title)));
    }
    Ok(Some(window))
}

/// Where a pointer action goes: the centre of the element it names, or the position it gives; with the window it is
/// for brought to the front. An element's window is always brought forward, and must still be where it was.
fn aim(agent: &Agent, desktop: &dyn Desktop, args: &Value) -> Result<((i32, i32), Option<WindowInfo>), ToolError> {
    let Some(id) = text(args, "element")? else {
        let point = position(agent, args, "x", "y")?;
        return Ok((point, target(desktop, args, &[point])?));
    };
    if !args["x"].is_null() || !args["y"].is_null() {
        return Err(ToolError::bad("give element, or x and y, not both"));
    }
    let (window, frame_then, place) = agent.trees.element(id)?;
    let gone = |why: String| ToolError { code: "element_gone", message: format!("element {id}: {why}; read the tree again") };
    let now = desktop.windows()?.into_iter().find(|candidate| candidate.id == window).ok_or_else(|| gone("its window closed".into()))?;
    let now = bring_forward(desktop, &now)?;
    if now.frame != frame_then {
        return Err(gone(format!("the window \"{}\" moved or changed size since the tree was read", now.title)));
    }
    let point = (place.x + (place.width / 2) as i32, place.y + (place.height / 2) as i32);
    Ok((point, Some(now)))
}

fn delivered(window: Option<WindowInfo>, mut answer: Value) -> Value {
    answer["delivered"] = json!(true);
    if let Some(window) = window {
        answer["window"] = window_json(&window);
    }
    answer
}

// ------------------------------------------------------------------------------------------------ tools

pub fn screenshot(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let desktop = open()?;
    let bounds = desktop.bounds()?;
    let mut shown = None;
    let wanted = match (&args["window"], &args["region"]) {
        (Value::Null, Value::Null) => bounds,
        (Value::Null, region) => {
            let part = |name: &str| signed(region, name)?.ok_or_else(|| ToolError::bad(format!("region needs x, y, width and height; {name} is missing")));
            let (width, height) = (part("width")?, part("height")?);
            if width <= 0 || height <= 0 {
                return Err(ToolError::bad("region's width and height must be more than zero"));
            }
            Rect { x: part("x")? as i32, y: part("y")? as i32, width: width as u32, height: height as u32 }
        }
        (window, Value::Null) => {
            let window = find_window(desktop.as_ref(), window)?;
            if window.minimized {
                return Err(ToolError::bad(format!("the window \"{}\" is minimized; focus it first", window.title)));
            }
            let frame = window.frame;
            shown = Some(window);
            frame
        }
        _ => return Err(ToolError::bad("give window or region, not both")),
    };
    let area = wanted.clip(&bounds).ok_or_else(|| ToolError::bad("that area is off the screen"))?;
    let full = desktop.capture(area)?;
    let save_to = text(args, "save_to")?;
    let image = match number(args, "max_width")? {
        Some(0) => return Err(ToolError::bad("max_width must be more than zero")),
        Some(max) => full.scaled_to_width(max.min(u64::from(u32::MAX)) as u32),
        // A picture for a model is kept to what the model sees unshrunk, so its positions stay this picture's pixels.
        None if save_to.is_none() && !crate::tools::flag(args, "full_size")? => full.scaled_to_width(full.width_for_model()),
        None => full,
    };
    let scale = f64::from(image.width) / f64::from(area.width);
    let png = image.to_png()?;
    let mut answer = json!({
        "capture": agent.captures.add(area, scale),
        "x": area.x,
        "y": area.y,
        "width": area.width,
        "height": area.height,
        "image_width": image.width,
        "image_height": image.height,
        "scale": scale,
    });
    if let Some(window) = shown {
        answer["window"] = window_json(&window);
    }
    match save_to {
        Some(path) => {
            let path = state::resolve(path);
            crate::tools::write_file(&path, &png)?;
            answer["saved_to"] = json!(path.display().to_string());
        }
        None => answer["png_base64"] = json!(BASE64.encode(&png)),
    }
    Ok(answer)
}

pub fn windows(_agent: &Agent, _args: &Value) -> Result<Value, ToolError> {
    let desktop = open()?;
    let bounds = desktop.bounds()?;
    Ok(json!({
        "screen": { "x": bounds.x, "y": bounds.y, "width": bounds.width, "height": bounds.height },
        "windows": desktop.windows()?.iter().map(window_json).collect::<Vec<_>>(),
    }))
}

pub fn focus(_agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let desktop = open()?;
    let window = bring_forward(desktop.as_ref(), &find_window(desktop.as_ref(), &args["window"])?)?;
    Ok(delivered(Some(window), json!({})))
}

/// What `arrange` is asked for: a state, a frame (any part left out stays as it is), or a frame in the normal state.
#[derive(Debug, PartialEq)]
struct Arrangement {
    state: Option<WindowState>,
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
}

impl Arrangement {
    fn from(args: &Value) -> Result<Arrangement, ToolError> {
        let state = match text(args, "state")? {
            None => None,
            Some("normal") => Some(WindowState::Normal),
            Some("minimized") => Some(WindowState::Minimized),
            Some("maximized") => Some(WindowState::Maximized),
            Some(other) => return Err(ToolError::bad(format!("state is normal, minimized or maximized, not {other}"))),
        };
        let place = |name: &str| signed(args, name).map(|value| value.map(|value| value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32));
        let size = |name: &str| match signed(args, name)? {
            None => Ok(None),
            Some(value) if (1..=100_000).contains(&value) => Ok(Some(value as u32)),
            Some(_) => Err(ToolError::bad(format!("{name} must be between 1 and 100000 pixels"))),
        };
        let arrangement = Arrangement { state, x: place("x")?, y: place("y")?, width: size("width")?, height: size("height")? };
        let placed = arrangement.placed();
        match arrangement.state {
            None if !placed => Err(ToolError::bad("say what to do: x, y, width and/or height, or state")),
            Some(WindowState::Minimized | WindowState::Maximized) if placed => {
                Err(ToolError::bad("a minimized or maximized window has no place of its own: give the state, or the place, not both"))
            }
            _ => Ok(arrangement),
        }
    }

    fn placed(&self) -> bool {
        self.x.is_some() || self.y.is_some() || self.width.is_some() || self.height.is_some()
    }

    /// The frame asked for, the parts not given taken from where the window is.
    fn frame(&self, now: Rect) -> Rect {
        Rect { x: self.x.unwrap_or(now.x), y: self.y.unwrap_or(now.y), width: self.width.unwrap_or(now.width), height: self.height.unwrap_or(now.height) }
    }
}

/// Moves, sizes, minimizes, maximizes or restores a window, and answers with the window as it then is.
pub fn arrange(_agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let arrangement = Arrangement::from(args)?;
    let desktop = open()?;
    let window = match find_window(desktop.as_ref(), &args["window"]) {
        Err(error) if error.code == "window_not_found" => args["window"].as_u64().and_then(|id| desktop.hidden_window(id)).ok_or(error)?,
        found => found?,
    };
    let current = |desktop: &dyn Desktop| desktop.windows().ok().and_then(|windows| windows.into_iter().find(|candidate| candidate.id == window.id)).or_else(|| desktop.hidden_window(window.id));
    if let Some(state) = arrangement.state {
        desktop.set_window_state(&window, state)?;
    }
    let mut asked = None;
    if arrangement.placed() {
        // Read again: a window just restored is somewhere else than it was.
        let from = if arrangement.state.is_some() { wait_until(|| current(desktop.as_ref()), |now| !now.minimized).unwrap_or(window.clone()) } else { window.clone() };
        let frame = arrangement.frame(from.frame);
        desktop.place_window(&from, frame)?;
        asked = Some(frame);
    }
    let before = window.frame;
    let now = wait_until(
        || current(desktop.as_ref()),
        |now| match (arrangement.state, asked) {
            (_, Some(frame)) => now.frame == frame && !now.minimized,
            (Some(WindowState::Minimized), None) => now.minimized,
            (Some(WindowState::Maximized), None) => !now.minimized && now.frame != before,
            (Some(WindowState::Normal), None) => !now.minimized,
            (None, None) => true,
        },
    )
    .ok_or_else(|| ToolError { code: "window_not_found", message: format!("the window \"{}\" closed", window.title) })?;
    if arrangement.state == Some(WindowState::Minimized) && !now.minimized {
        return Err(ToolError::failed(format!("the window \"{}\" was asked to minimize and didn't", window.title)));
    }
    if matches!(arrangement.state, Some(WindowState::Normal | WindowState::Maximized)) && now.minimized {
        return Err(ToolError::failed(format!("the window \"{}\" was asked to come back from the Dock or the taskbar and didn't", window.title)));
    }
    let mut answer = json!({});
    if let Some(frame) = asked.filter(|frame| *frame != now.frame) {
        // Said, not hidden: windows have smallest sizes, and some apps or window managers keep to sizes of their own.
        answer["asked"] = json!({ "x": frame.x, "y": frame.y, "width": frame.width, "height": frame.height });
        answer["note"] = json!("the window isn't exactly where it was asked to be: the app or the window manager kept it to a place or size it allows");
    }
    Ok(delivered(Some(now), answer))
}

/// The apps the system's launcher offers, by name: all of them, or those whose name contains `name`.
pub fn apps(_agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let wanted = text(args, "name")?.unwrap_or("").to_lowercase();
    let apps = crate::apps::list()?;
    let listed: Vec<Value> = apps.iter().filter(|app| app.name.to_lowercase().contains(&wanted)).map(|app| json!({ "name": app.name, "id": app.id })).collect();
    Ok(json!({ "apps": listed }))
}

/// Starts an app by its name in the launcher, and waits for a window of it to open.
pub fn launch(_agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let wanted = text(args, "name")?.filter(|name| !name.is_empty()).ok_or_else(|| ToolError::bad("name is required: the app's name as the launcher shows it, or its id from `apps`"))?;
    let wait = number(args, "wait_s")?.unwrap_or(10);
    if wait > 60 {
        return Err(ToolError::bad("wait_s is at most 60"));
    }
    let apps = crate::apps::list()?;
    let app = crate::apps::choose(&apps, wanted)?;
    // The windows there were before, so the new one can be told apart. Without a desktop there is none to wait for.
    let desktop = desktop::open().ok();
    let before: Vec<u64> = desktop.as_ref().and_then(|desktop| desktop.windows().ok()).unwrap_or_default().iter().map(|window| window.id).collect();
    crate::apps::start(app)?;
    let mut answer = json!({ "app": { "name": app.name, "id": app.id }, "delivered": true });
    let Some(desktop) = desktop.filter(|_| wait > 0) else {
        return Ok(answer);
    };
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        // A window that has a title: apps put up empty helper windows first.
        let new = desktop.windows().ok().and_then(|windows| windows.into_iter().find(|window| !before.contains(&window.id) && !window.title.is_empty()));
        if let Some(window) = new {
            answer["window"] = window_json(&window);
            return Ok(answer);
        }
        if Instant::now() >= deadline {
            answer["window"] = Value::Null;
            answer["note"] = json!(format!("no new window opened within {wait} s: the app may still be starting, may have no window, or may have shown one it already had"));
            return Ok(answer);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The window as it is once `done` holds, or as it is after two seconds; None if it is gone for all that time. A
/// window coming back from the Dock is listed nowhere for a moment while it animates.
fn wait_until(read: impl Fn() -> Option<WindowInfo>, done: impl Fn(&WindowInfo) -> bool) -> Option<WindowInfo> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last = None;
    loop {
        if let Some(now) = read() {
            if done(&now) {
                return Some(now);
            }
            last = Some(now);
        }
        if Instant::now() >= deadline {
            return last;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn press(desktop: &dyn Desktop, button: Button, times: u32) -> Result<(), ToolError> {
    for index in 0..times {
        if index > 0 {
            std::thread::sleep(Duration::from_millis(80));
        }
        desktop.button(button, true)?;
        std::thread::sleep(Duration::from_millis(20));
        desktop.button(button, false)?;
    }
    Ok(())
}

/// Refuses `background` where it isn't offered, rather than ignoring it and acting for real.
fn not_in_the_background(args: &Value, what: &str) -> Result<(), ToolError> {
    if crate::tools::flag(args, "background")? {
        return Err(ToolError { code: "not_supported", message: format!("{what} can't be sent in the background: only a plain click, typed text and keys can") });
    }
    Ok(())
}

/// A click that leaves the pointer and the window order alone: what is at the point is pressed, or given the
/// keyboard, through assistive technology.
fn click_quietly(agent: &Agent, desktop: &dyn Desktop, args: &Value) -> Result<Value, ToolError> {
    let (point, window) = match text(args, "element")? {
        Some(id) => {
            if !args["x"].is_null() || !args["y"].is_null() {
                return Err(ToolError::bad("give element, or x and y, not both"));
            }
            let (window, frame_then, place) = agent.trees.element(id)?;
            let gone = |why: String| ToolError { code: "element_gone", message: format!("element {id}: {why}; read the tree again") };
            let now = desktop.windows()?.into_iter().find(|candidate| candidate.id == window).ok_or_else(|| gone("its window closed".into()))?;
            if now.frame != frame_then {
                return Err(gone(format!("the window \"{}\" moved or changed size since the tree was read", now.title)));
            }
            ((place.x + (place.width / 2) as i32, place.y + (place.height / 2) as i32), now)
        }
        None => {
            if args["window"].is_null() {
                return Err(ToolError::bad("window is required for a background click: the window the point is in"));
            }
            let window = find_window(desktop, &args["window"])?;
            let point = position(agent, args, "x", "y")?;
            if !window.frame.contains(point.0, point.1) {
                return Err(ToolError::bad(format!("the point {},{} is outside the window \"{}\", so nothing was sent", point.0, point.1, window.title)));
            }
            (point, window)
        }
    };
    let did = desktop.quiet_click(&window, point.0, point.1)?;
    Ok(delivered(Some(window), json!({ "at": { "x": point.0, "y": point.1 }, "background": true, "did": did })))
}

/// The modifier keys a click is to be made with (`hold`: "ctrl", "shift", "ctrl+shift"), checked to be modifiers.
fn held_keys(args: &Value) -> Result<Vec<keys::Key>, ToolError> {
    let Some(hold) = text(args, "hold")?.filter(|hold| !hold.is_empty()) else {
        return Ok(Vec::new());
    };
    let chord = keys::parse_chord(hold).map_err(ToolError::bad)?;
    if let Some(key) = chord.iter().find(|key| !key.is_modifier()) {
        return Err(ToolError::bad(format!("hold takes modifier keys only (ctrl, shift, alt, cmd/win), not {key:?}")));
    }
    Ok(chord)
}

/// Runs `action` with the keys held down, and lets them go afterwards whatever happened: a modifier left held would
/// change every key the user or the next tool presses.
fn holding<T>(desktop: &dyn Desktop, held: &[keys::Key], action: impl FnOnce() -> Result<T, ToolError>) -> Result<T, ToolError> {
    let mut down = Vec::new();
    let pressed = held.iter().try_for_each(|key| {
        desktop.key(*key, true)?;
        down.push(*key);
        std::thread::sleep(Duration::from_millis(10));
        Ok::<(), DesktopError>(())
    });
    let outcome = pressed.map_err(ToolError::from).and_then(|()| action());
    let released: Vec<_> = down.iter().rev().map(|key| desktop.key(*key, false)).collect();
    let value = outcome?;
    released.into_iter().collect::<Result<Vec<_>, _>>()?;
    Ok(value)
}

fn click_with(agent: &Agent, args: &Value, button: Button, times: u32) -> Result<Value, ToolError> {
    let desktop = open()?;
    let held = held_keys(args)?;
    if crate::tools::flag(args, "background")? {
        if button != Button::Left || times != 1 || !held.is_empty() {
            return not_in_the_background(args, "a double, right, middle or held-key click").map(|()| Value::Null);
        }
        return click_quietly(agent, desktop.as_ref(), args);
    }
    let ((x, y), window) = aim(agent, desktop.as_ref(), args)?;
    move_to(desktop.as_ref(), x, y)?;
    holding(desktop.as_ref(), &held, || press(desktop.as_ref(), button, times))?;
    let mut answer = json!({ "at": { "x": x, "y": y } });
    if !held.is_empty() {
        answer["held"] = json!(text(args, "hold")?);
    }
    Ok(delivered(window, answer))
}

pub fn click(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    click_with(agent, args, button_named(args)?, 1)
}

pub fn double_click(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    click_with(agent, args, Button::Left, 2)
}

pub fn right_click(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    click_with(agent, args, Button::Right, 1)
}

pub fn move_pointer(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let desktop = open()?;
    let (x, y) = position(agent, args, "x", "y")?;
    move_to(desktop.as_ref(), x, y)?;
    Ok(delivered(None, json!({ "at": { "x": x, "y": y } })))
}

pub fn drag(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    not_in_the_background(args, "a drag")?;
    let desktop = open()?;
    let button = button_named(args)?;
    let from = position(agent, args, "from_x", "from_y")?;
    let to = position(agent, args, "to_x", "to_y")?;
    let window = target(desktop.as_ref(), args, &[from])?;
    move_to(desktop.as_ref(), from.0, from.1)?;
    desktop.button(button, true)?;
    // In steps, as a hand would: programs start a drag only once the pointer has travelled.
    let dragged = (1..=12).try_for_each(|step| {
        std::thread::sleep(Duration::from_millis(15));
        let along = |a: i32, b: i32| a + ((i64::from(b) - i64::from(a)) * step / 12) as i32;
        move_to(desktop.as_ref(), along(from.0, to.0), along(from.1, to.1))
    });
    std::thread::sleep(Duration::from_millis(30));
    // Whatever happened on the way, the button must not stay down.
    let released = desktop.button(button, false);
    dragged?;
    released?;
    Ok(delivered(window, json!({ "from": { "x": from.0, "y": from.1 }, "to": { "x": to.0, "y": to.1 } })))
}

pub fn scroll(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    not_in_the_background(args, "scrolling")?;
    let desktop = open()?;
    let down = signed(args, "down")?.unwrap_or(0);
    let right = signed(args, "right")?.unwrap_or(0);
    if (down == 0 && right == 0) || down.abs() > 100 || right.abs() > 100 {
        return Err(ToolError::bad("give down and/or right in notches, between -100 and 100 (negative is up or left)"));
    }
    let ((x, y), window) = aim(agent, desktop.as_ref(), args)?;
    move_to(desktop.as_ref(), x, y)?;
    desktop.scroll(right as i32, down as i32)?;
    Ok(delivered(window, json!({ "at": { "x": x, "y": y } })))
}

/// The window keys must go to. It is required — named, or the one an `element` is in: without it they would land in
/// whatever happens to be in front. With `element`, that element is given the keyboard first, so the keys go into
/// it and not into whatever part of the window had the keyboard (a browser's address bar, say).
fn keyboard_target(agent: &Agent, desktop: &dyn Desktop, args: &Value, forward: bool) -> Result<WindowInfo, ToolError> {
    let element = text(args, "element")?.filter(|id| !id.is_empty());
    let window = match (element, args["window"].is_null()) {
        (None, true) => return Err(ToolError::bad("window is required: name the window the keys are for (its id or title), or the element they go into")),
        (_, false) => find_window(desktop, &args["window"])?,
        (Some(id), true) => {
            let (window, ..) = agent.trees.place(id)?;
            find_window(desktop, &json!(window)).map_err(|_| ToolError { code: "element_gone", message: format!("element {id}: its window is gone; read the tree again") })?
        }
    };
    let window = if forward { bring_forward(desktop, &window)? } else { window };
    if let Some(id) = element {
        let (of, path, role, name) = agent.trees.place(id)?;
        if of != window.id {
            return Err(ToolError::bad(format!("element {id} isn't in the window \"{}\"", window.title)));
        }
        match desktop.act(&window, Target { path: &path, role: &role, name: &name }, &ElementAction::Focus) {
            Ok(_) => {}
            // Some toolkits (GTK 4) give assistive technology no way to hand over the keyboard: a click on the
            // element does it, where the window may come forward.
            Err(error) if error.code == "not_supported" && forward => {
                let (_, frame_then, place) = agent.trees.element(id)?;
                if window.frame != frame_then {
                    return Err(ToolError { code: "element_gone", message: format!("element {id}: the window \"{}\" moved or changed size since the tree was read; read the tree again", window.title) });
                }
                move_to(desktop, place.x + (place.width / 2) as i32, place.y + (place.height / 2) as i32)?;
                press(desktop, Button::Left, 1)?;
            }
            Err(error) => return Err(ToolError { code: error.code, message: format!("element {id} ({role} \"{name}\"): {}", error.message) }),
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    Ok(window)
}

/// The answer to typed text or keys: that they were sent, to which window, and — where the system can tell — what
/// in that window had the keyboard, since being sent says nothing about where they landed.
fn typed(desktop: &dyn Desktop, window: WindowInfo, mut answer: Value) -> Value {
    if let Some((role, name)) = desktop.keyboard_owner(&window) {
        answer["into"] = json!({ "role": role, "name": name });
    }
    delivered(Some(window), answer)
}

pub fn clipboard_get(_agent: &Agent, _args: &Value) -> Result<Value, ToolError> {
    Ok(json!({ "text": open()?.clipboard()? }))
}

pub fn clipboard_set(_agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let wanted = text(args, "text")?.ok_or_else(|| ToolError::bad("text is required (it may be empty)"))?;
    if wanted.len() > 4 * 1024 * 1024 {
        return Err(ToolError::bad("at most 4 MB of text goes on the clipboard"));
    }
    let desktop = open()?;
    desktop.set_clipboard(wanted)?;
    // Read back: the clipboard belongs to the whole session, and another program may have refused or replaced it.
    let now = desktop.clipboard()?;
    if now.as_deref() != Some(wanted) {
        return Err(ToolError::failed("the clipboard doesn't hold the text that was set"));
    }
    Ok(json!({ "delivered": true, "characters": wanted.chars().count() }))
}

/// Presses a chord: its keys down in order, then up in reverse, even when a later key failed.
fn press_chord(desktop: &dyn Desktop, chord: &[keys::Key]) -> Result<(), ToolError> {
    let (last, held) = chord.split_last().ok_or_else(|| ToolError::bad("no keys"))?;
    holding(desktop, held, || {
        desktop.key(*last, true)?;
        std::thread::sleep(Duration::from_millis(10));
        desktop.key(*last, false)?;
        Ok(())
    })?;
    std::thread::sleep(Duration::from_millis(15));
    Ok(())
}

/// The keys `type_text` presses around the text: before it, to empty the field or move the caret; after it, Enter.
#[derive(Debug, PartialEq)]
struct Around {
    before: Vec<Vec<keys::Key>>,
    after: Vec<Vec<keys::Key>>,
}

/// What `clear`, `caret` and `enter` ask for, in the keys of this system: Command on macOS, Ctrl elsewhere.
fn around(args: &Value, mac: bool) -> Result<Around, ToolError> {
    use keys::Key;
    let mut before = Vec::new();
    if crate::tools::flag(args, "clear")? {
        // From the start to the end, selected, then deleted: Select All is a menu command some fields lack (a plain
        // Windows text field has no Ctrl+A).
        let (start, select_to_end) = if mac { (vec![Key::Meta, Key::Up], vec![Key::Meta, Key::Shift, Key::Down]) } else { (vec![Key::Ctrl, Key::Home], vec![Key::Ctrl, Key::Shift, Key::End]) };
        before.extend([start, select_to_end, vec![Key::Backspace]]);
    }
    match text(args, "caret")? {
        None => {}
        Some(_) if !before.is_empty() => return Err(ToolError::bad("give clear or caret, not both: a cleared field has nowhere else for the caret to be")),
        Some("start") => before.push(if mac { vec![Key::Meta, Key::Up] } else { vec![Key::Ctrl, Key::Home] }),
        Some("end") => before.push(if mac { vec![Key::Meta, Key::Down] } else { vec![Key::Ctrl, Key::End] }),
        Some(other) => return Err(ToolError::bad(format!("caret is start or end, not {other}"))),
    }
    let after = if crate::tools::flag(args, "enter")? { vec![vec![Key::Enter]] } else { Vec::new() };
    Ok(Around { before, after })
}

pub fn type_text(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let around = around(args, cfg!(target_os = "macos"))?;
    let text_given = match text(args, "text")? {
        Some(text) if !text.is_empty() => text,
        // Only emptying a field, or only pressing Enter, needs no text.
        _ if !around.before.is_empty() || !around.after.is_empty() => "",
        _ => return Err(ToolError::bad("text is required")),
    };
    let desktop = open()?;
    let background = crate::tools::flag(args, "background")?;
    let window = keyboard_target(agent, desktop.as_ref(), args, !background)?;
    let chord = |keys: &[keys::Key]| if background { desktop.quiet_chord(&window, keys).map_err(ToolError::from) } else { press_chord(desktop.as_ref(), keys) };
    for keys in &around.before {
        chord(keys)?;
    }
    if !text_given.is_empty() {
        if background {
            desktop.quiet_text(&window, text_given)?;
        } else {
            desktop.text(text_given)?;
        }
    }
    for keys in &around.after {
        chord(keys)?;
    }
    let mut answer = json!({ "characters": text_given.chars().count() });
    if background {
        answer["background"] = json!(true);
    }
    Ok(typed(desktop.as_ref(), window, answer))
}

pub fn key(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let chord = keys::parse_chord(required_text(args, "keys")?).map_err(ToolError::bad)?;
    let times = number(args, "times")?.unwrap_or(1);
    if times == 0 || times > 100 {
        return Err(ToolError::bad("times must be between 1 and 100"));
    }
    let desktop = open()?;
    if crate::tools::flag(args, "background")? {
        let window = keyboard_target(agent, desktop.as_ref(), args, false)?;
        for _ in 0..times {
            desktop.quiet_chord(&window, &chord)?;
            std::thread::sleep(Duration::from_millis(15));
        }
        return Ok(typed(desktop.as_ref(), window, json!({ "keys": required_text(args, "keys")?, "times": times, "background": true })));
    }
    let window = keyboard_target(agent, desktop.as_ref(), args, true)?;
    let owner = desktop.keyboard_owner(&window);
    for _ in 0..times {
        press_chord(desktop.as_ref(), &chord)?;
    }
    // What had the keyboard before the keys went: after them it may be something else (a dialog they closed).
    let mut answer = json!({ "keys": required_text(args, "keys")?, "times": times });
    if let Some((role, name)) = owner {
        answer["into"] = json!({ "role": role, "name": name });
    }
    Ok(delivered(Some(window), answer))
}

// ------------------------------------------------------------------------------------------------ what is in a window

/// What a client is looking for in a window. Empty fields match anything.
struct Wanted {
    role: Option<String>,
    name: Option<String>,
    value: Option<String>,
    /// Only what can be acted on: a control with a place on screen, or whatever has the keyboard.
    interactive: bool,
}

/// The roles of the elements a person or an agent can act on.
const INTERACTIVE: &[&str] = &[
    "button", "text", "check_box", "radio_button", "combo_box", "menu_item", "list_item", "tree_item", "tab", "link",
    "slider", "spinner", "scroll_bar", "cell", "menu_bar_item", "switch", "toggle_button", "split_button", "hyperlink",
];

impl Wanted {
    fn from(args: &Value) -> Result<Wanted, ToolError> {
        let lower = |name: &str| Ok::<_, ToolError>(text(args, name)?.filter(|text| !text.is_empty()).map(str::to_lowercase));
        Ok(Wanted { role: lower("role")?, name: lower("name")?, value: lower("value")?, interactive: crate::tools::flag(args, "interactive")? })
    }

    fn anything(&self) -> bool {
        self.role.is_none() && self.name.is_none() && self.value.is_none() && !self.interactive
    }

    /// The role must be the same; the name and value need only contain what was asked, in any case.
    fn matches(&self, element: &Element) -> bool {
        let actionable = || element.focused || (INTERACTIVE.contains(&element.role.as_str()) && element.frame.is_some_and(|frame| frame.width > 0 && frame.height > 0));
        (!self.interactive || actionable())
            && self.role.as_ref().is_none_or(|role| element.role == *role)
            && self.name.as_ref().is_none_or(|name| element.name.to_lowercase().contains(name))
            && self.value.as_ref().is_none_or(|value| element.value.as_ref().is_some_and(|held| held.to_lowercase().contains(value)))
    }
}

/// The tree as a list, parents before children, each entry with its depth. Only what differs from the usual is said:
/// `enabled` appears when false, `focused` when true, `value` and the place when there is one.
fn flatten(element: &Element, path: &mut Vec<u32>, tree: u64, wanted: Option<&Wanted>, kept: &mut Kept, list: &mut Vec<Value>) {
    let depth = path.len() as u32;
    let index = kept.frames.len();
    kept.frames.push(element.frame);
    kept.places.push((path.clone(), element.role.clone(), element.name.clone()));
    if wanted.is_none_or(|wanted| wanted.matches(element)) {
        let mut entry = json!({ "id": format!("{tree}.{index}"), "depth": depth, "role": element.role, "name": element.name });
        if let Some(value) = &element.value {
            entry["value"] = json!(value);
        }
        if let Some(frame) = element.frame {
            entry["x"] = json!(frame.x);
            entry["y"] = json!(frame.y);
            entry["width"] = json!(frame.width);
            entry["height"] = json!(frame.height);
        }
        if !element.enabled {
            entry["enabled"] = json!(false);
        }
        if element.focused {
            entry["focused"] = json!(true);
        }
        list.push(entry);
    }
    for (position, child) in element.children.iter().enumerate() {
        path.push(position as u32);
        flatten(child, path, tree, wanted, kept, list);
        path.pop();
    }
}

/// What is remembered of a tree's elements, by their number in it.
#[derive(Default)]
struct Kept {
    frames: Vec<Option<Rect>>,
    places: Vec<(Vec<u32>, String, String)>,
}

fn limits(args: &Value) -> Result<TreeLimits, ToolError> {
    let depth = number(args, "max_depth")?.unwrap_or(60);
    let elements = number(args, "max_elements")?.unwrap_or(1000);
    if depth == 0 || elements == 0 || elements > 20_000 {
        return Err(ToolError::bad("max_depth must be at least 1 and max_elements between 1 and 20000"));
    }
    Ok(TreeLimits { depth: depth.min(1000) as u32, elements: elements as usize })
}

/// Reads a window and answers with the elements that match (all of them when nothing is asked for).
fn read_window(agent: &Agent, desktop: &dyn Desktop, window: &WindowInfo, limits: TreeLimits, wanted: Option<&Wanted>) -> Result<Value, ToolError> {
    let (root, cut) = desktop.tree(window, limits)?;
    let (mut kept, mut list) = (Kept::default(), Vec::new());
    // The id is needed while the list is made, and the places while it is stored: number it first.
    let id = agent.trees.add(window, Vec::new());
    flatten(&root, &mut Vec::new(), id, wanted, &mut kept, &mut list);
    if let Some(stored) = agent.trees.recent.lock().unwrap().iter_mut().find(|stored| stored.id == id) {
        stored.frames = kept.frames;
        stored.places = kept.places;
    }
    Ok(json!({ "tree": id, "window": window_json(window), "elements": list, "truncated": cut }))
}

pub fn tree(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let wanted = Wanted::from(args)?;
    let desktop = open()?;
    let window = find_window(desktop.as_ref(), &args["window"])?;
    read_window(agent, desktop.as_ref(), &window, limits(args)?, (!wanted.anything()).then_some(&wanted))
}

pub fn find(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let wanted = Wanted::from(args)?;
    if wanted.anything() {
        return Err(ToolError::bad("say what to find: role, name and/or value"));
    }
    let desktop = open()?;
    let window = find_window(desktop.as_ref(), &args["window"])?;
    read_window(agent, desktop.as_ref(), &window, limits(args)?, Some(&wanted))
}

pub fn wait_for(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let wanted = Wanted::from(args)?;
    let gone = crate::tools::flag(args, "gone")?;
    let timeout = number(args, "timeout_s")?.unwrap_or(10);
    if timeout == 0 || timeout > 120 {
        return Err(ToolError::bad("timeout_s must be between 1 and 120"));
    }
    if args["window"].is_null() {
        return Err(ToolError::bad("window is required: the window to wait for, or to wait for something in"));
    }
    let limits = limits(args)?;
    let desktop = open()?;
    let began = Instant::now();
    let mut last_trouble = None;
    loop {
        let waited = || json!(began.elapsed().as_millis() as u64);
        match find_window_now(desktop.as_ref(), &args["window"]) {
            Err(error) if error.code == "window_not_found" => {
                if gone {
                    return Ok(json!({ "waited_ms": waited(), "gone": true }));
                }
                last_trouble = Some(error);
            }
            Err(error) => return Err(error),
            Ok(window) if wanted.anything() => {
                if !gone {
                    return Ok(json!({ "waited_ms": waited(), "window": window_json(&window) }));
                }
            }
            Ok(window) => match read_window(agent, desktop.as_ref(), &window, limits, Some(&wanted)) {
                Ok(mut answer) => {
                    let found = answer["elements"].as_array().is_some_and(|elements| !elements.is_empty());
                    if found != gone {
                        answer["waited_ms"] = waited();
                        if gone {
                            answer["gone"] = json!(true);
                        }
                        return Ok(answer);
                    }
                }
                // An app that has just started may not describe its window yet; keep asking until the time is up.
                Err(error) => last_trouble = Some(error),
            },
        }
        if began.elapsed() >= Duration::from_secs(timeout) {
            let what = if gone { "it was still there" } else { "it didn't appear" };
            let detail = last_trouble.map(|error| format!(" (last answer: {})", error.message)).unwrap_or_default();
            return Err(ToolError { code: "timed_out", message: format!("after {timeout} s {what}{detail}") });
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

// ------------------------------------------------------------------------------------------------ element actions

/// Does something to the element a client named, through assistive technology: the pointer and the keyboard are
/// left alone and the window needn't be in front.
fn act(agent: &Agent, args: &Value, action: ElementAction) -> Result<Value, ToolError> {
    let id = text(args, "element")?.filter(|id| !id.is_empty()).ok_or_else(|| ToolError::bad("element is required: an id from tree or find, like 3.17"))?;
    let (window, path, role, name) = agent.trees.place(id)?;
    let desktop = open()?;
    let window = desktop
        .windows()?
        .into_iter()
        .find(|candidate| candidate.id == window)
        .ok_or_else(|| ToolError { code: "element_gone", message: format!("element {id}: its window is gone; read the tree again") })?;
    let acted = desktop.act(&window, Target { path: &path, role: &role, name: &name }, &action).map_err(|error| ToolError { code: error.code, message: format!("element {id} ({role} \"{name}\"): {}", error.message) })?;
    let mut answer = json!({ "element": id, "role": role, "name": name });
    if let Some(value) = acted.value {
        answer["value"] = json!(value);
    }
    if let Some(on) = acted.on {
        answer["on"] = json!(on);
    }
    if let Some(expanded) = acted.expanded {
        answer["expanded"] = json!(expanded);
    }
    Ok(answer)
}

pub fn press_element(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    act(agent, args, ElementAction::Press)
}

pub fn set_value(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let value = match &args["value"] {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => return Err(ToolError::bad("value is required: the text or the number to set")),
    };
    act(agent, args, ElementAction::SetValue(value))
}

pub fn toggle(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let on = match &args["on"] {
        Value::Null => None,
        Value::Bool(on) => Some(*on),
        _ => return Err(ToolError::bad("on is true or false; leave it out to flip the element")),
    };
    act(agent, args, ElementAction::Toggle(on))
}

pub fn expand(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let expanded = match &args["expanded"] {
        Value::Null => true,
        Value::Bool(expanded) => *expanded,
        _ => return Err(ToolError::bad("expanded is true or false")),
    };
    act(agent, args, ElementAction::Expand(expanded))
}

pub fn select(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    act(agent, args, ElementAction::Select)
}

#[cfg(test)]
mod tests {
    use super::*;
    use keys::Key;

    #[test]
    fn typing_helpers_press_the_keys_of_each_system() {
        let none = around(&json!({}), false).unwrap();
        assert_eq!(none, Around { before: vec![], after: vec![] });
        let clear = around(&json!({"clear": true, "enter": true}), false).unwrap();
        assert_eq!(clear.before, [vec![Key::Ctrl, Key::Home], vec![Key::Ctrl, Key::Shift, Key::End], vec![Key::Backspace]]);
        assert_eq!(clear.after, [vec![Key::Enter]]);
        assert_eq!(around(&json!({"clear": true}), true).unwrap().before, [vec![Key::Meta, Key::Up], vec![Key::Meta, Key::Shift, Key::Down], vec![Key::Backspace]], "Command on macOS");
        assert_eq!(around(&json!({"caret": "start"}), false).unwrap().before, [vec![Key::Ctrl, Key::Home]]);
        assert_eq!(around(&json!({"caret": "end"}), false).unwrap().before, [vec![Key::Ctrl, Key::End]]);
        assert_eq!(around(&json!({"caret": "start"}), true).unwrap().before, [vec![Key::Meta, Key::Up]]);
        assert_eq!(around(&json!({"caret": "end"}), true).unwrap().before, [vec![Key::Meta, Key::Down]]);
        assert_eq!(around(&json!({"caret": "middle"}), false).unwrap_err().code, "bad_input");
        assert_eq!(around(&json!({"clear": true, "caret": "end"}), false).unwrap_err().code, "bad_input");
        assert_eq!(around(&json!({"enter": "yes"}), false).unwrap_err().code, "bad_input");
    }

    #[test]
    fn an_arrangement_is_a_state_or_a_place() {
        let placed = Arrangement::from(&json!({"x": 10, "width": 300})).unwrap();
        assert_eq!(placed.frame(Rect { x: 1, y: 2, width: 3, height: 4 }), Rect { x: 10, y: 2, width: 300, height: 4 }, "what isn't given stays");
        assert_eq!(Arrangement::from(&json!({"state": "maximized"})).unwrap().state, Some(WindowState::Maximized));
        let restored = Arrangement::from(&json!({"state": "normal", "x": 5, "y": 6})).unwrap();
        assert_eq!((restored.state, restored.placed()), (Some(WindowState::Normal), true), "restored, then placed");
        for wrong in [json!({}), json!({"state": "huge"}), json!({"state": "minimized", "x": 1}), json!({"state": "maximized", "width": 9}), json!({"width": 0}), json!({"height": -5}), json!({"x": "left"})] {
            assert_eq!(Arrangement::from(&wrong).unwrap_err().code, "bad_input", "{wrong}");
        }
    }

    #[test]
    fn held_keys_are_modifiers_only() {
        assert_eq!(held_keys(&json!({})).unwrap(), []);
        assert_eq!(held_keys(&json!({"hold": "ctrl+shift"})).unwrap(), [Key::Ctrl, Key::Shift]);
        assert_eq!(held_keys(&json!({"hold": "cmd"})).unwrap(), [Key::Meta]);
        assert_eq!(held_keys(&json!({"hold": "ctrl+a"})).unwrap_err().code, "bad_input");
        assert_eq!(held_keys(&json!({"hold": "nonsense"})).unwrap_err().code, "bad_input");
    }

    #[test]
    fn the_compact_view_keeps_what_can_be_acted_on() {
        let wanted = Wanted::from(&json!({"interactive": true})).unwrap();
        assert!(!wanted.anything());
        let place = Some(Rect { x: 1, y: 1, width: 10, height: 10 });
        let element = |role: &str, frame: Option<Rect>, focused: bool| Element { role: role.into(), frame, focused, enabled: true, ..Element::default() };
        assert!(wanted.matches(&element("button", place, false)));
        assert!(wanted.matches(&element("text", place, false)));
        assert!(!wanted.matches(&element("label", place, false)), "a label can't be acted on");
        assert!(!wanted.matches(&element("pane", place, false)));
        assert!(!wanted.matches(&element("button", None, false)), "a button with no place isn't shown");
        assert!(!wanted.matches(&element("button", Some(Rect { x: 0, y: 0, width: 0, height: 5 }), false)));
        assert!(wanted.matches(&element("pane", None, true)), "what has the keyboard always is");
        let buttons = Wanted::from(&json!({"interactive": true, "role": "button"})).unwrap();
        assert!(buttons.matches(&element("button", place, false)) && !buttons.matches(&element("text", place, false)));
        assert!(Wanted::from(&json!({})).unwrap().anything(), "a plain tree filters nothing");
    }
}

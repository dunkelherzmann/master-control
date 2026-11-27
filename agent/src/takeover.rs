//! Asking before taking over a computer someone is using (specs.md §3.4). Before the pointer, the keyboard or bringing
//! a window forward, if a person used the real mouse or keyboard within the last minute (or as long as the machine is
//! set to), a pop-up on that machine asks them, with OK and Cancel and no time limit. OK lets the agent's session
//! carry on until it has been quiet a while and the person is back; Cancel refuses those actions with `user_declined`
//! and is recorded, so the agent can say why it couldn't act. When a session that was let in is done, a pop-up with
//! one OK button tells the person they can use the computer again.
//!
//! What is remembered (the setting, each session's answer, when the agent last sent input itself) is in
//! `take-over.json` in the agent's home, so it holds across processes (`mctl call` is one process a call); every
//! answer is added to `take-over.log`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::desktop::{self, Acted, Button, Desktop, Element, ElementAction, Image, Key, Rect, Target, TreeLimits, VirtualDesktop, WindowInfo, WindowState};
use crate::tools::{number, text, ToolError};
use crate::{platform, state, Agent};

/// Input within this long counts as someone using the machine, unless the machine is set otherwise.
pub const DEFAULT_ACTIVE_WITHIN_S: u64 = 60;
/// Input this soon after the agent's own counts as the agent's: the system may still be delivering it.
const OWN_INPUT_SLACK_MS: u64 = 1500;
/// A session that was let in and has sent nothing for this long is done: the machine is handed back.
pub const DONE_AFTER: Duration = Duration::from_secs(5 * 60);
const SETTINGS_FILE: &str = "take-over.json";
const LOG_FILE: &str = "take-over.log";

/// One question at a time, and no two calls reading and writing the file at once.
static TURN: Mutex<()> = Mutex::new(());

/// What a session was told.
#[derive(Clone, Debug, Default, PartialEq)]
struct Session {
    /// OK was clicked (or nobody was there to ask): the session may use the machine.
    granted: bool,
    /// The pop-up was shown and answered OK, so the person is told when the machine is theirs again.
    asked: bool,
    /// Cancel was clicked: refused until the agent asks again with `take_over`.
    declined_ms: Option<u64>,
    /// When this session last sent input.
    last_action_ms: Option<u64>,
    /// What the agent said it needs the machine for.
    reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Settings {
    active_within_s: u64,
    /// When any session of this agent last sent input: input just after it is the agent's own, not a person's.
    last_action_ms: Option<u64>,
    sessions: Map<String, Value>,
}

impl Settings {
    fn load(home: &Path) -> Settings {
        let saved: Value = fs::read(home.join(SETTINGS_FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
        Settings {
            active_within_s: saved["active_within_s"].as_u64().unwrap_or(DEFAULT_ACTIVE_WITHIN_S),
            last_action_ms: saved["last_action_ms"].as_u64(),
            sessions: saved["sessions"].as_object().cloned().unwrap_or_default(),
        }
    }

    fn save(&self, home: &Path) {
        let saved = json!({ "active_within_s": self.active_within_s, "last_action_ms": self.last_action_ms, "sessions": self.sessions });
        // Losing the file only means asking again.
        let _ = state::write_private(&home.join(SETTINGS_FILE), saved.to_string().as_bytes());
    }

    fn session(&self, name: &str) -> Session {
        let saved = &self.sessions.get(name).cloned().unwrap_or(Value::Null);
        Session {
            granted: saved["granted"].as_bool().unwrap_or(false),
            asked: saved["asked"].as_bool().unwrap_or(false),
            declined_ms: saved["declined_ms"].as_u64(),
            last_action_ms: saved["last_action_ms"].as_u64(),
            reason: saved["reason"].as_str().map(str::to_string),
        }
    }

    fn set_session(&mut self, name: &str, session: &Session) {
        if *session == Session::default() {
            self.sessions.remove(name);
            return;
        }
        self.sessions.insert(
            name.to_string(),
            json!({
                "granted": session.granted,
                "asked": session.asked,
                "declined_ms": session.declined_ms,
                "last_action_ms": session.last_action_ms,
                "reason": session.reason,
            }),
        );
    }
}

/// What a step that needs the machine in front may do.
#[derive(Debug, PartialEq)]
enum Gate {
    Go,
    Ask,
    Declined,
}

/// Whether someone is using the machine: real input, not the agent's own, within the setting's time.
fn someone_active(now_ms: u64, user_input_ms: Option<u64>, own_last_ms: Option<u64>, active_within_s: u64) -> bool {
    let Some(input) = user_input_ms else { return false };
    let ours = own_last_ms.is_some_and(|own| input <= own + OWN_INPUT_SLACK_MS);
    !ours && now_ms.saturating_sub(input) <= active_within_s * 1000
}

fn decide(now_ms: u64, user_input_ms: Option<u64>, settings: &Settings, session: &Session) -> Gate {
    if settings.active_within_s == 0 {
        return Gate::Go;
    }
    if session.declined_ms.is_some() {
        return Gate::Declined;
    }
    if !someone_active(now_ms, user_input_ms, settings.last_action_ms, settings.active_within_s) {
        return Gate::Go;
    }
    // Let in, and still at work: the person said OK, so their hand on the mouse doesn't stop it. Let in but quiet
    // for a while, with the person back at the machine: they are asked again.
    let busy = session.last_action_ms.is_some_and(|last| now_ms.saturating_sub(last) <= settings.active_within_s * 1000);
    if session.granted && busy {
        Gate::Go
    } else {
        Gate::Ask
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

/// The session a call belongs to: its working folder's name.
fn session_name() -> String {
    state::working_folder().file_name().map_or_else(|| "shared".to_string(), |name| name.to_string_lossy().into_owned())
}

/// Whether a tool, with these arguments, moves the real pointer, presses real keys or brings a window forward.
pub(crate) fn needs_front(tool: &str, args: &Value) -> bool {
    match tool {
        "click" | "double_click" | "right_click" | "move" | "drag" | "scroll" | "type_text" | "key" => args["background"] != Value::Bool(true),
        "focus" => true,
        "desktops" => !matches!(args["action"].as_str(), None | Some("list")),
        _ => false,
    }
}

/// Whether a tool sends input of its own (in front or in the background), which the system may count as someone's.
fn sends_input(tool: &str, args: &Value) -> bool {
    needs_front(tool, args) || matches!(tool, "click" | "type_text" | "key")
}

fn doing(tool: &str) -> &'static str {
    match tool {
        "type_text" => "type",
        "key" => "press keys",
        "focus" => "bring a window to the front",
        "desktops" => "change the virtual desktop shown",
        "move" | "drag" | "scroll" => "use the mouse",
        _ => "click",
    }
}

fn question(reason: Option<&str>, tool: &str) -> String {
    let general = format!(
        "An AI agent working through Master Control needs to use this computer's mouse and keyboard{}.\n\nFinish what \
         you're doing, then click OK to let it take over. Cancel keeps the computer yours: the agent will say what it \
         couldn't do.",
        if reason.is_some_and(|reason| !reason.trim().is_empty()) { String::new() } else { format!(" to {}", doing(tool)) }
    );
    // The agent's own words come last, where the person's eye ends up.
    match reason.map(str::trim).filter(|reason| !reason.is_empty()) {
        Some(reason) => format!("{general}\n\nWhat for: {reason}"),
        None => general,
    }
}

const DONE: &str = "The AI agent is done with this computer. You can start using it again.";

fn declined(session: &Session, now: u64) -> ToolError {
    let ago = session.declined_ms.map_or(0, |when| now.saturating_sub(when) / 1000);
    let reason = session.reason.as_deref().map_or_else(String::new, |reason| format!(" (asked for: {reason})"));
    ToolError {
        code: "user_declined",
        message: format!(
            "the person using {} clicked Cancel when asked to let the agent take over, {ago} s ago{reason}; nothing was \
             sent. Tell them you can't do this step without the machine and what that leaves unchecked; ask again with \
             take_over only if they agree",
            platform::hostname()
        ),
    }
}

fn record(home: &Path, session: &str, reason: Option<&str>, answer: &str) {
    let line = json!({ "time_ms": now_ms(), "machine": platform::hostname(), "session": session, "reason": reason, "answer": answer });
    let path = home.join(LOG_FILE);
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = state::restrict_to_user(&path);
        let _ = writeln!(file, "{line}");
    }
}

fn decisions(home: &Path, count: usize) -> Vec<Value> {
    let text = fs::read_to_string(home.join(LOG_FILE)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(count)..].iter().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

/// Whether input may go ahead at all: where it can't (macOS without the Accessibility permission), the tool itself
/// says why, and nobody is asked.
pub(crate) fn input_allowed() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::desktop::ready().1
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Shows the pop-up and keeps the answer. Called with the turn held.
fn ask_now(home: &Path, desktop: &dyn Desktop, settings: &mut Settings, name: &str, session: &mut Session, tool: &str) -> Result<(), ToolError> {
    let ok = desktop.ask(&question(session.reason.as_deref(), tool))?;
    let now = now_ms();
    if ok {
        session.granted = true;
        session.asked = true;
        session.declined_ms = None;
        // Counts as at work from now, so the person's last keystrokes before OK don't ask again at once.
        session.last_action_ms = Some(now);
        settings.last_action_ms = Some(now);
    } else {
        session.granted = false;
        session.declined_ms = Some(now);
    }
    settings.set_session(name, session);
    settings.save(home);
    record(home, name, session.reason.as_deref(), if ok { "ok" } else { "cancel" });
    if ok { Ok(()) } else { Err(declined(session, now)) }
}

/// The call being run on this thread: the agent's home, the tool, and whether the person has been asked already.
struct Call {
    home: PathBuf,
    tool: String,
    cleared: bool,
}

thread_local! {
    static CALL: std::cell::RefCell<Option<Call>> = const { std::cell::RefCell::new(None) };
}

/// Runs one tool call: the desktop it opens asks the person before the first real input (see [`Asking`]), and when
/// the agent sent input itself, that is noted.
pub(crate) fn around<T>(agent: &Agent, tool: &str, args: &Value, work: impl FnOnce() -> T) -> T {
    around_in(&agent.home, tool, args, work)
}

fn around_in<T>(home: &Path, tool: &str, args: &Value, work: impl FnOnce() -> T) -> T {
    let call = Call { home: home.to_path_buf(), tool: tool.to_string(), cleared: false };
    let before = CALL.with(|current| current.replace(Some(call)));
    let result = work();
    CALL.with(|current| *current.borrow_mut() = before);
    acted(home, tool, args);
    result
}

/// Asks first, once per call, if someone is using the machine.
fn before_input(desktop: &dyn Desktop) -> crate::desktop::Result<()> {
    let call = CALL.with(|current| current.borrow().as_ref().filter(|call| !call.cleared).map(|call| (call.home.clone(), call.tool.clone())));
    let Some((home, tool)) = call else { return Ok(()) };
    gate(&home, &tool, desktop).map_err(|error| crate::desktop::DesktopError { code: error.code, message: error.message })?;
    CALL.with(|current| {
        if let Some(call) = current.borrow_mut().as_mut() {
            call.cleared = true;
        }
    });
    Ok(())
}

fn gate(home: &Path, tool: &str, desktop: &dyn Desktop) -> Result<(), ToolError> {
    let _turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut settings = Settings::load(home);
    if settings.active_within_s == 0 {
        return Ok(());
    }
    let name = session_name();
    let mut session = settings.session(&name);
    let now = now_ms();
    let user_input = desktop.idle().map(|idle| now.saturating_sub(idle.as_millis() as u64));
    match decide(now, user_input, &settings, &session) {
        Gate::Go => Ok(()),
        Gate::Declined => Err(declined(&session, now)),
        Gate::Ask => ask_now(home, desktop, &mut settings, &name, &mut session, tool),
    }
}

/// A desktop that asks the person using it before the pointer, the keyboard or bringing a window forward; quiet
/// actions (in the background, through assistive technology) go straight through.
pub(crate) struct Asking(pub Box<dyn Desktop>);

impl Desktop for Asking {
    fn bounds(&self) -> crate::desktop::Result<Rect> {
        self.0.bounds()
    }
    fn capture(&self, area: Rect) -> crate::desktop::Result<Image> {
        self.0.capture(area)
    }
    fn windows(&self) -> crate::desktop::Result<Vec<WindowInfo>> {
        self.0.windows()
    }
    fn focus(&self, window: u64) -> crate::desktop::Result<()> {
        before_input(self.0.as_ref())?;
        self.0.focus(window)
    }
    fn pointer(&self) -> crate::desktop::Result<(i32, i32)> {
        self.0.pointer()
    }
    fn move_pointer(&self, x: i32, y: i32) -> crate::desktop::Result<()> {
        before_input(self.0.as_ref())?;
        self.0.move_pointer(x, y)
    }
    fn button(&self, button: Button, down: bool) -> crate::desktop::Result<()> {
        if down {
            before_input(self.0.as_ref())?;
        }
        self.0.button(button, down)
    }
    fn scroll(&self, right: i32, down: i32) -> crate::desktop::Result<()> {
        before_input(self.0.as_ref())?;
        self.0.scroll(right, down)
    }
    fn key(&self, key: Key, down: bool) -> crate::desktop::Result<()> {
        if down {
            before_input(self.0.as_ref())?;
        }
        self.0.key(key, down)
    }
    fn text(&self, text: &str) -> crate::desktop::Result<()> {
        before_input(self.0.as_ref())?;
        self.0.text(text)
    }
    fn tree(&self, window: &WindowInfo, limits: TreeLimits) -> crate::desktop::Result<(Element, bool)> {
        self.0.tree(window, limits)
    }
    fn act(&self, window: &WindowInfo, target: Target, action: &ElementAction) -> crate::desktop::Result<Acted> {
        self.0.act(window, target, action)
    }
    fn quiet_click(&self, window: &WindowInfo, x: i32, y: i32) -> crate::desktop::Result<String> {
        self.0.quiet_click(window, x, y)
    }
    fn keyboard_owner(&self, window: &WindowInfo) -> Option<(String, String)> {
        self.0.keyboard_owner(window)
    }
    fn quiet_text(&self, window: &WindowInfo, text: &str) -> crate::desktop::Result<()> {
        self.0.quiet_text(window, text)
    }
    fn quiet_chord(&self, window: &WindowInfo, chord: &[Key]) -> crate::desktop::Result<()> {
        self.0.quiet_chord(window, chord)
    }
    fn place_window(&self, window: &WindowInfo, frame: Rect) -> crate::desktop::Result<()> {
        self.0.place_window(window, frame)
    }
    fn set_window_state(&self, window: &WindowInfo, state: WindowState) -> crate::desktop::Result<()> {
        self.0.set_window_state(window, state)
    }
    fn hidden_window(&self, id: u64) -> Option<WindowInfo> {
        self.0.hidden_window(id)
    }
    fn desktops(&self) -> crate::desktop::Result<Vec<VirtualDesktop>> {
        self.0.desktops()
    }
    fn switch_desktop(&self, index: usize) -> crate::desktop::Result<()> {
        before_input(self.0.as_ref())?;
        self.0.switch_desktop(index)
    }
    fn create_desktop(&self) -> crate::desktop::Result<()> {
        before_input(self.0.as_ref())?;
        self.0.create_desktop()
    }
    fn remove_desktop(&self, index: usize) -> crate::desktop::Result<()> {
        before_input(self.0.as_ref())?;
        self.0.remove_desktop(index)
    }
    fn idle(&self) -> Option<Duration> {
        self.0.idle()
    }
    fn ask(&self, message: &str) -> crate::desktop::Result<bool> {
        self.0.ask(message)
    }
    fn tell(&self, message: &str) {
        self.0.tell(message)
    }
    fn clipboard(&self) -> crate::desktop::Result<Option<String>> {
        self.0.clipboard()
    }
    fn set_clipboard(&self, text: &str) -> crate::desktop::Result<()> {
        self.0.set_clipboard(text)
    }
}

/// Called after every tool: notes when the agent itself sent input.
fn acted(home: &Path, tool: &str, args: &Value) {
    if !sends_input(tool, args) {
        return;
    }
    let _turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut settings = Settings::load(home);
    let now = now_ms();
    settings.last_action_ms = Some(now);
    let name = session_name();
    let mut session = settings.session(&name);
    if needs_front(tool, args) {
        session.last_action_ms = Some(now);
        settings.set_session(&name, &session);
    }
    settings.save(home);
}

/// Hands the machine back from one session; the person is told if they had been asked.
fn hand_back(home: &Path, settings: &mut Settings, name: &str, tell: impl FnOnce()) {
    let session = settings.session(name);
    if session.granted && session.asked {
        tell();
        record(home, name, session.reason.as_deref(), "done");
    }
    settings.set_session(name, &Session::default());
}

fn tell_done() {
    if let Ok(desktop) = desktop::open() {
        desktop.tell(DONE);
    }
}

/// The session is over: the machine is the person's again.
pub(crate) fn end_session(home: &Path, name: &str) {
    let _turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut settings = Settings::load(home);
    if !settings.sessions.contains_key(name) {
        return;
    }
    hand_back(home, &mut settings, name, tell_done);
    settings.save(home);
}

/// Hands the machine back from every session that was let in and has sent nothing for `quiet`.
pub fn hand_back_quiet(home: &Path, quiet: Duration) {
    hand_back_quiet_telling(home, quiet, tell_done);
}

fn hand_back_quiet_telling(home: &Path, quiet: Duration, tell: impl Fn()) {
    let _turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut settings = Settings::load(home);
    let now = now_ms();
    let done: Vec<String> = settings
        .sessions
        .keys()
        .filter(|name| {
            let session = settings.session(name);
            session.granted && session.last_action_ms.is_none_or(|last| now.saturating_sub(last) >= quiet.as_millis() as u64)
        })
        .cloned()
        .collect();
    if done.is_empty() {
        return;
    }
    for name in done {
        hand_back(home, &mut settings, &name, &tell);
    }
    settings.save(home);
}

/// The `take_over` tool: ask now, hand back, or say how things stand; and the machine's setting.
pub(crate) fn tool(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let action = text(args, "action")?.unwrap_or("ask");
    if !matches!(action, "ask" | "release" | "status") {
        return Err(ToolError::bad("action is ask, release or status"));
    }
    let reason = text(args, "reason")?.map(str::trim).filter(|reason| !reason.is_empty()).map(str::to_string);
    let within = number(args, "active_within_s")?;
    if within.is_some_and(|within| within > 24 * 3600) {
        return Err(ToolError::bad("active_within_s is at most 86400 (a day)"));
    }
    let home: PathBuf = agent.home.clone();
    let name = session_name();
    let answer = {
        // How things stand is read without waiting for a question another call has open.
        let _turn = (action != "status" || within.is_some()).then(|| TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        let mut settings = Settings::load(&home);
        if let Some(within) = within {
            settings.active_within_s = within;
            settings.save(&home);
        }
        let mut session = settings.session(&name);
        if reason.is_some() {
            session.reason = reason.clone();
        }
        let desktop = if input_allowed() { desktop::open().ok() } else { None };
        let now = now_ms();
        let idle = desktop.as_ref().and_then(|desktop| desktop.idle());
        let user_input = idle.map(|idle| now.saturating_sub(idle.as_millis() as u64));
        let active = settings.active_within_s > 0 && someone_active(now, user_input, settings.last_action_ms, settings.active_within_s);
        let mut asked = false;
        match action {
            "release" => hand_back(&home, &mut settings, &name, tell_done),
            "ask" => {
                // Asked outright, an earlier Cancel is asked again; nobody at the machine means nobody to ask.
                let busy = session.last_action_ms.is_some_and(|last| now.saturating_sub(last) <= settings.active_within_s * 1000);
                if active && !(session.granted && busy) {
                    let desktop = desktop.as_ref().ok_or_else(|| ToolError { code: "not_supported", message: "this machine's desktop can't be reached to ask".into() })?;
                    asked = true;
                    ask_now(&home, desktop.as_ref(), &mut settings, &name, &mut session, "take_over")?;
                } else if !session.granted {
                    session.granted = true;
                    session.declined_ms = None;
                    session.last_action_ms = Some(now);
                    settings.set_session(&name, &session);
                }
            }
            _ => {}
        }
        if action != "release" {
            settings.set_session(&name, &session);
        }
        settings.save(&home);
        let session = settings.session(&name);
        json!({
            "machine": platform::hostname(),
            "granted": session.granted,
            "asked": asked,
            "declined": session.declined_ms.is_some(),
            "reason": session.reason,
            "someone_active": active,
            "idle_s": idle.map(|idle| idle.as_secs()),
            "active_within_s": settings.active_within_s,
            "decisions": decisions(&home, 10),
        })
    };
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(last_action_ms: Option<u64>) -> Settings {
        Settings { active_within_s: 60, last_action_ms, sessions: Map::new() }
    }

    const NOW: u64 = 1_000_000_000;

    #[test]
    fn nobody_at_the_machine_means_go() {
        assert_eq!(decide(NOW, None, &settings(None), &Session::default()), Gate::Go);
        assert_eq!(decide(NOW, Some(NOW - 61_000), &settings(None), &Session::default()), Gate::Go);
    }

    #[test]
    fn someone_using_it_is_asked() {
        assert_eq!(decide(NOW, Some(NOW - 5_000), &settings(None), &Session::default()), Gate::Ask);
        assert_eq!(decide(NOW, Some(NOW - 60_000), &settings(None), &Session::default()), Gate::Ask);
    }

    #[test]
    fn the_agents_own_input_is_not_someone_using_it() {
        assert_eq!(decide(NOW, Some(NOW - 5_000), &settings(Some(NOW - 6_000)), &Session::default()), Gate::Go);
        // Well after the agent's own, it is a person's.
        assert_eq!(decide(NOW, Some(NOW - 1_000), &settings(Some(NOW - 6_000)), &Session::default()), Gate::Ask);
    }

    #[test]
    fn a_session_let_in_carries_on_while_at_work() {
        let session = Session { granted: true, asked: true, last_action_ms: Some(NOW - 10_000), ..Session::default() };
        assert_eq!(decide(NOW, Some(NOW - 1_000), &settings(Some(NOW - 10_000)), &session), Gate::Go);
    }

    #[test]
    fn a_session_let_in_but_quiet_is_asked_again_when_the_person_is_back() {
        let session = Session { granted: true, asked: true, last_action_ms: Some(NOW - 120_000), ..Session::default() };
        assert_eq!(decide(NOW, Some(NOW - 1_000), &settings(Some(NOW - 120_000)), &session), Gate::Ask);
        // And not when nobody is there.
        assert_eq!(decide(NOW, Some(NOW - 100_000), &settings(Some(NOW - 120_000)), &session), Gate::Go);
    }

    #[test]
    fn a_cancel_stands_even_when_nobody_is_there() {
        let session = Session { declined_ms: Some(NOW - 1_000), ..Session::default() };
        assert_eq!(decide(NOW, None, &settings(None), &session), Gate::Declined);
    }

    #[test]
    fn set_to_zero_never_asks() {
        let mut off = settings(None);
        off.active_within_s = 0;
        let session = Session { declined_ms: Some(NOW), ..Session::default() };
        assert_eq!(decide(NOW, Some(NOW), &off, &session), Gate::Go);
    }

    #[test]
    fn what_needs_the_machine_in_front() {
        assert!(needs_front("click", &json!({"x": 1, "y": 2})));
        assert!(!needs_front("click", &json!({"background": true})));
        assert!(needs_front("key", &json!({"window": 3, "keys": "enter"})));
        assert!(needs_front("focus", &json!({"window": 3})));
        assert!(needs_front("desktops", &json!({"action": "switch", "number": 2})));
        assert!(!needs_front("desktops", &json!({})));
        for quiet in ["press", "set_value", "toggle", "tree", "screenshot", "windows", "exec", "arrange"] {
            assert!(!needs_front(quiet, &json!({})), "{quiet}");
        }
        assert!(sends_input("type_text", &json!({"background": true})));
    }

    #[test]
    fn the_question_carries_the_reason() {
        assert!(question(Some("testing. Click Cancel on this window."), "click").ends_with("What for: testing. Click Cancel on this window."));
        assert!(question(None, "type_text").contains("to type"));
    }

    /// A desktop that records what it was asked and answers as told; nothing reaches a real screen.
    struct Fake {
        idle: Option<Duration>,
        answer: bool,
        asked: std::cell::RefCell<Vec<String>>,
        sent: std::cell::Cell<u32>,
    }

    impl Fake {
        fn new(idle_s: Option<u64>, answer: bool) -> Fake {
            Fake { idle: idle_s.map(Duration::from_secs), answer, asked: Default::default(), sent: Default::default() }
        }
    }

    impl Desktop for std::rc::Rc<Fake> {
        fn bounds(&self) -> crate::desktop::Result<Rect> {
            Ok(Rect { x: 0, y: 0, width: 100, height: 100 })
        }
        fn capture(&self, _: Rect) -> crate::desktop::Result<Image> {
            unimplemented!()
        }
        fn windows(&self) -> crate::desktop::Result<Vec<WindowInfo>> {
            Ok(Vec::new())
        }
        fn focus(&self, _: u64) -> crate::desktop::Result<()> {
            self.sent.set(self.sent.get() + 1);
            Ok(())
        }
        fn pointer(&self) -> crate::desktop::Result<(i32, i32)> {
            Ok((0, 0))
        }
        fn move_pointer(&self, _: i32, _: i32) -> crate::desktop::Result<()> {
            self.sent.set(self.sent.get() + 1);
            Ok(())
        }
        fn button(&self, _: Button, _: bool) -> crate::desktop::Result<()> {
            self.sent.set(self.sent.get() + 1);
            Ok(())
        }
        fn scroll(&self, _: i32, _: i32) -> crate::desktop::Result<()> {
            self.sent.set(self.sent.get() + 1);
            Ok(())
        }
        fn key(&self, _: Key, _: bool) -> crate::desktop::Result<()> {
            self.sent.set(self.sent.get() + 1);
            Ok(())
        }
        fn text(&self, _: &str) -> crate::desktop::Result<()> {
            self.sent.set(self.sent.get() + 1);
            Ok(())
        }
        fn tree(&self, _: &WindowInfo, _: TreeLimits) -> crate::desktop::Result<(Element, bool)> {
            unimplemented!()
        }
        fn act(&self, _: &WindowInfo, _: Target, _: &ElementAction) -> crate::desktop::Result<Acted> {
            unimplemented!()
        }
        fn quiet_text(&self, _: &WindowInfo, _: &str) -> crate::desktop::Result<()> {
            self.sent.set(self.sent.get() + 1);
            Ok(())
        }
        fn idle(&self) -> Option<Duration> {
            self.idle
        }
        fn ask(&self, message: &str) -> crate::desktop::Result<bool> {
            self.asked.borrow_mut().push(message.to_string());
            Ok(self.answer)
        }
        fn clipboard(&self) -> crate::desktop::Result<Option<String>> {
            Ok(None)
        }
        fn set_clipboard(&self, _: &str) -> crate::desktop::Result<()> {
            Ok(())
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("mc-take-over-{name}-{}-{}", std::process::id(), now_ms()));
        fs::create_dir_all(&home).unwrap();
        home
    }

    /// One tool call on the fake, the way `tools::call` runs it, in its own session.
    fn run(home: &Path, tool: &str, args: Value, desktop: &Asking, work: impl FnOnce(&Asking) -> crate::desktop::Result<()>) -> crate::desktop::Result<()> {
        state::with_working_folder(home.join("work").join("one"), || around_in(home, tool, &args, || work(desktop)))
    }

    /// The fake and the asking desktop around it.
    fn asking(idle_s: Option<u64>, answer: bool) -> (std::rc::Rc<Fake>, Asking) {
        let fake = std::rc::Rc::new(Fake::new(idle_s, answer));
        (fake.clone(), Asking(Box::new(fake)))
    }

    #[test]
    fn someone_at_the_machine_is_asked_once_and_ok_lets_the_session_carry_on() {
        let home = scratch("ok");
        let (seen, desktop) = asking(Some(2), true);
        let reason = "checking the window";
        let mut kept = Settings::load(&home);
        kept.set_session("one", &Session { reason: Some(reason.into()), ..Session::default() });
        kept.save(&home);
        run(&home, "drag", json!({}), &desktop, |desktop| {
            desktop.button(Button::Left, true)?;
            desktop.move_pointer(5, 5)?;
            desktop.button(Button::Left, false)
        })
        .unwrap();
        assert_eq!(seen.asked.borrow().len(), 1, "one question for the whole call");
        assert!(seen.asked.borrow()[0].contains(reason));
        assert_eq!(seen.sent.get(), 3);
        // The next call carries on: the person said OK and the session is at work.
        run(&home, "key", json!({}), &desktop, |desktop| desktop.key(Key::Char('a'), true)).unwrap();
        assert_eq!(seen.asked.borrow().len(), 1);
        let session = Settings::load(&home).session("one");
        assert!(session.granted && session.asked && session.declined_ms.is_none(), "{session:?}");
        assert_eq!(decisions(&home, 5).iter().map(|line| line["answer"].clone()).collect::<Vec<_>>(), [json!("ok")]);
        // Handed back after a quiet spell, the person is told; a second sweep tells nobody.
        let told = std::cell::Cell::new(0);
        hand_back_quiet_telling(&home, Duration::ZERO, || told.set(told.get() + 1));
        hand_back_quiet_telling(&home, Duration::ZERO, || told.set(told.get() + 1));
        assert_eq!(told.get(), 1);
        assert!(!Settings::load(&home).session("one").granted);
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn cancel_refuses_and_keeps_refusing_without_asking_again() {
        let home = scratch("cancel");
        let (seen, desktop) = asking(Some(2), false);
        let refused = run(&home, "click", json!({}), &desktop, |desktop| desktop.button(Button::Left, true)).unwrap_err();
        assert_eq!(refused.code, "user_declined");
        assert_eq!(seen.sent.get(), 0, "nothing was sent");
        let again = run(&home, "type_text", json!({}), &desktop, |desktop| desktop.text("x")).unwrap_err();
        assert_eq!(again.code, "user_declined");
        assert_eq!((seen.asked.borrow().len(), seen.sent.get()), (1, 0));
        assert_eq!(decisions(&home, 5)[0]["answer"], "cancel");
        assert!(seen.asked.borrow()[0].contains("to click"));
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn nobody_at_the_machine_and_quiet_actions_are_never_asked() {
        let home = scratch("quiet");
        let (away_seen, away) = asking(Some(600), false);
        run(&home, "click", json!({}), &away, |desktop| desktop.button(Button::Left, true)).unwrap();
        let (unknown_seen, unknown) = asking(None, false);
        run(&home, "click", json!({}), &unknown, |desktop| desktop.button(Button::Left, true)).unwrap();
        // Long after the agent's own input, a person typing a moment ago is someone; quiet input still asks nobody.
        let mut kept = Settings::load(&home);
        kept.last_action_ms = Some(now_ms() - 30_000);
        kept.save(&home);
        let (here_seen, here) = asking(Some(1), false);
        let window = WindowInfo { id: 1, title: String::new(), app: String::new(), pid: 1, frame: Rect { x: 0, y: 0, width: 1, height: 1 }, focused: true, minimized: false };
        run(&home, "type_text", json!({"background": true}), &here, |desktop| desktop.quiet_text(&window, "x")).unwrap();
        assert!(here_seen.asked.borrow().is_empty() && away_seen.asked.borrow().is_empty() && unknown_seen.asked.borrow().is_empty());
        // The quiet input is the agent's own: the person's last keystroke, just before it, doesn't count.
        let session = Settings::load(&home);
        assert!(session.last_action_ms.unwrap() > now_ms() - 5_000);
        assert!(session.session("one").last_action_ms.is_some(), "the earlier clicks were the session's");
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn ending_the_session_hands_the_machine_back() {
        let home = scratch("end");
        let mut kept = Settings::load(&home);
        kept.set_session("one", &Session { granted: true, ..Session::default() });
        kept.set_session("two", &Session { granted: true, last_action_ms: Some(now_ms()), ..Session::default() });
        kept.save(&home);
        end_session(&home, "one");
        let after = Settings::load(&home);
        assert!(!after.sessions.contains_key("one") && after.sessions.contains_key("two"));
        // Only a session quiet for as long as asked is handed back.
        hand_back_quiet_telling(&home, Duration::from_secs(600), || {});
        assert!(Settings::load(&home).sessions.contains_key("two"));
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn answers_and_settings_are_kept_on_disk() {
        let home = std::env::temp_dir().join(format!("mc-take-over-{}-{}", std::process::id(), now_ms()));
        fs::create_dir_all(&home).unwrap();
        let mut kept = Settings::load(&home);
        assert_eq!(kept.active_within_s, DEFAULT_ACTIVE_WITHIN_S);
        kept.active_within_s = 30;
        let session = Session { declined_ms: Some(5), reason: Some("a test".into()), ..Session::default() };
        kept.set_session("one", &session);
        kept.save(&home);
        let read = Settings::load(&home);
        assert_eq!(read.active_within_s, 30);
        assert_eq!(read.session("one"), session);
        record(&home, "one", Some("a test"), "cancel");
        let logged = decisions(&home, 10);
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0]["answer"], "cancel");
        assert_eq!(logged[0]["reason"], "a test");
        // Handing back forgets the session; one never asked isn't told.
        let mut told = false;
        let mut settings = Settings::load(&home);
        hand_back(&home, &mut settings, "one", || told = true);
        assert!(!told && !settings.sessions.contains_key("one"));
        let mut settings = Settings::load(&home);
        settings.set_session("two", &Session { granted: true, asked: true, ..Session::default() });
        hand_back(&home, &mut settings, "two", || told = true);
        assert!(told);
        fs::remove_dir_all(&home).unwrap();
    }
}

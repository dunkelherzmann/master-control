//! The screens (specs.md §6): choose Master or Worker; then Start, Pair and Cleanup for that role. The same code
//! draws them on every system. What a button does runs on a thread of its own, and its lines appear as they come.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fontdue::{Font, FontSettings};

use crate::canvas::{Canvas, Colour, Picture, Rect};

/// The window's size at a zoom of 1. Everything is drawn larger by the display's zoom, so it stays sharp.
const WIDTH: i32 = 560;
const HEIGHT: i32 = 700;

const BACKGROUND: Colour = 0x18181A;
const CARD: Colour = 0x232326;
const CARD_HOVER: Colour = 0x2B2B2F;
const LINE: Colour = 0x38383C;
const TILE: Colour = 0x2E2E32;
const TEXT: Colour = 0xF5F5F7;
// The two greys of secondary text, light enough to read easily on the dark ground.
const SOFT: Colour = 0xDEDEE3;
const FAINT: Colour = 0xB0B0B8;
const BLUE: Colour = 0x0A84FF;
const BLUE_HOVER: Colour = 0x3D9BFF;
const GREEN: Colour = 0x30D158;
// A status light with traffic: lit brighter than at rest, with a glow around it, and nearly out in between.
const LIGHT_ON: Colour = 0x8CFFAA;
const LIGHT_GLOW: Colour = 0x1E6B36;
const LIGHT_OFF: Colour = 0x17502A;
const PURPLE: Colour = 0xBF5AF2;
const ORANGE: Colour = 0xFF9F0A;
const RED: Colour = 0xFF453A;
const RED_HOVER: Colour = 0xFF6961;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Role {
    Master,
    Worker,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Screen {
    Choose,
    Menu(Role),
    /// The master shows its code and the workers that have paired.
    ShowCode,
    /// The worker asks for the master's code.
    AskCode,
    /// "Remove everything?", with Cancel first.
    ConfirmCleanup(Role),
    /// What this program is: its name, version, author and where it lives.
    About,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Choose(Role),
    Start,
    Pair,
    Cleanup,
    ReallyCleanup,
    SubmitCode,
    TogglePublic,
    Back,
    About,
    /// Opens the project's page in the browser.
    OpenProject,
}

pub enum Key {
    Character(char),
    Backspace,
    Enter,
    Escape,
}

/// What the buttons do, apart from moving between screens. The real one calls the worker's and the master's own
/// functions; tests use one that only notes what was asked.
pub trait Work: Send + Sync + 'static {
    fn start(&self, role: Role, public_networks: bool) -> Vec<String>;
    fn cleanup(&self, role: Role) -> Vec<String>;
    /// Opens the master's pairing window; returns a way to read its code and the workers paired so far.
    fn show_code(&self) -> Result<Box<dyn Showing>, String>;
    fn pair_with(&self, code: &str) -> Vec<String>;
    /// This machine's host name, and its system release and hardware address.
    fn identity(&self) -> (String, String);
    /// Opens a web address in the user's browser.
    fn open(&self, address: &str);
    /// Whether the role is installed here, and who it is paired with (for the master, its workers).
    fn status(&self, role: Role) -> Status;
    /// What is going on right now, for the status section: asked for about once a second, away from the window's
    /// own thread, so it may take a moment (it looks whether each worker answers).
    fn live(&self) -> Live;
    /// The names of the status lines with something going to or from them this very moment. Asked for several
    /// times a second, so it must be quick.
    fn busy(&self) -> Vec<String>;
}

/// One line of the status section: a dot, a name, and what is true of it now.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Some(true): up and well (green). Some(false): wanted but not there (orange). None: not set up (grey).
    pub well: Option<bool>,
    pub name: String,
    pub says: String,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Live {
    /// What this machine is (the master, or a worker), then each of the master's workers.
    pub lines: Vec<Line>,
    /// The master's latest calls, newest last: how long ago, on which machine, which tool, how it ended.
    pub calls: Vec<String>,
    /// The lines with traffic right now: their lights flicker, as a disk's does when it is read or written.
    pub busy: Vec<String>,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Status {
    pub installed: bool,
    pub paired_with: Vec<String>,
    /// The AI agents found on this machine, and whether each has Master Control. Only the master's status has them.
    pub agents: Vec<(String, bool)>,
}

pub trait Showing: Send {
    fn code(&self) -> String;
    /// Each paired worker's name and address.
    fn paired(&self) -> Vec<(String, String)>;
    fn seconds_left(&self) -> u64;
}

pub struct App {
    pub screen: Screen,
    scale: f32,
    work: Arc<dyn Work>,
    regular: Font,
    bold: Font,
    icon: Option<Picture>,
    buttons: Vec<(Rect, Action)>,
    pointer: (i32, i32),
    log: Arc<Mutex<Vec<String>>>,
    busy: Arc<AtomicBool>,
    showing: Option<Box<dyn Showing>>,
    pub code: String,
    pub public_networks: bool,
    status: Status,
    identity: (String, String),
    drawn_log: usize,
    ticks: u32,
    /// The screen About was opened from, to go back to.
    before_about: Screen,
    /// What the status section shows, kept fresh by a thread of its own, and what of it has been drawn.
    live: Arc<Mutex<Live>>,
    drawn_live: Live,
}

impl App {
    /// `scale` is the display's zoom: 1 for a plain display, 1.5 or 2 for a dense one.
    pub fn new(work: Arc<dyn Work>, scale: f32) -> App {
        let identity = work.identity();
        // Looked at about once a second for as long as the window lives; the thread ends when the window's state is
        // dropped.
        let live = Arc::new(Mutex::new(Live::default()));
        let (watching, watched) = (Arc::downgrade(&live), work.clone());
        std::thread::spawn(move || {
            // Who is busy is looked at several times a second, for the lights; the rest once a second.
            let mut looks = 0u32;
            while let Some(live) = watching.upgrade() {
                let mut now = if looks.is_multiple_of(7) { watched.live() } else { live.lock().unwrap().clone() };
                now.busy = watched.busy();
                *live.lock().unwrap() = now;
                drop(live);
                looks = looks.wrapping_add(1);
                std::thread::sleep(std::time::Duration::from_millis(140));
            }
        });
        let font = |bytes: &[u8]| Font::from_bytes(bytes, FontSettings { scale: 40.0 * scale, ..FontSettings::default() }).expect("the built-in font is sound");
        App {
            screen: Screen::Choose,
            scale: scale.clamp(1.0, 4.0),
            regular: font(include_bytes!("../assets/NotoSans-Regular.ttf")),
            bold: font(include_bytes!("../assets/NotoSans-Bold.ttf")),
            icon: Picture::from_png(include_bytes!("../assets/icon.png")),
            work,
            buttons: Vec::new(),
            pointer: (-1, -1),
            log: Arc::new(Mutex::new(Vec::new())),
            busy: Arc::new(AtomicBool::new(false)),
            showing: None,
            code: String::new(),
            public_networks: false,
            status: Status::default(),
            identity,
            drawn_log: 0,
            ticks: 0,
            before_about: Screen::Choose,
            live,
            drawn_live: Live::default(),
        }
    }

    /// Shows another machine's name and details than this one's: pictures of the screens are kept with the project,
    /// and a real machine's name and address don't belong there.
    pub fn set_identity(&mut self, name: &str, details: &str) {
        self.identity = (name.to_string(), details.to_string());
    }

    /// Shows this in the status section, in place of what is really going on: pictures of the screens are kept
    /// with the project, and real machines' names and addresses don't belong there.
    pub fn set_live(&mut self, live: Live) {
        *self.live.lock().unwrap() = live;
    }

    /// Looks at what is going on now, without waiting for the next look of the thread that keeps it fresh.
    pub fn look_now(&mut self) {
        let now = self.work.live();
        *self.live.lock().unwrap() = now;
    }

    /// The window's size in real pixels.
    pub fn size(&self) -> (usize, usize) {
        (self.u(WIDTH) as usize, self.u(HEIGHT) as usize)
    }

    /// A length of the design, in real pixels.
    fn u(&self, length: i32) -> i32 {
        (length as f32 * self.scale).round() as i32
    }

    fn rect(&self, x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x: self.u(x), y: self.u(y), w: self.u(w), h: self.u(h) }
    }

    fn go(&mut self, screen: Screen) {
        self.screen = screen;
        if let Screen::Menu(role) = screen {
            self.status = self.work.status(role);
        }
    }

    /// Runs something slow away from the window, adding its lines to the log as it ends.
    fn run(&mut self, title: &str, job: impl FnOnce(&dyn Work) -> Vec<String> + Send + 'static) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (work, log, busy) = (self.work.clone(), self.log.clone(), self.busy.clone());
        *log.lock().unwrap() = vec![format!("{title}…")];
        std::thread::spawn(move || {
            let lines = job(work.as_ref());
            log.lock().unwrap().extend(lines);
            busy.store(false, Ordering::SeqCst);
        });
    }

    pub fn act(&mut self, action: Action) {
        let role = match self.screen {
            Screen::Menu(role) | Screen::ConfirmCleanup(role) => Some(role),
            Screen::ShowCode => Some(Role::Master),
            Screen::AskCode => Some(Role::Worker),
            Screen::Choose | Screen::About => None,
        };
        match (action, role) {
            (Action::About, _) => {
                if self.screen != Screen::About {
                    self.before_about = self.screen;
                    self.screen = Screen::About;
                }
            }
            (Action::OpenProject, _) => self.work.open(mc_core::PROJECT),
            (Action::Back, _) if self.screen == Screen::About => self.screen = self.before_about,
            (Action::Choose(role), _) => {
                self.log.lock().unwrap().clear();
                self.go(Screen::Menu(role));
            }
            (Action::Start, Some(role)) => {
                let public = self.public_networks;
                self.run("Starting", move |work| work.start(role, public));
            }
            (Action::Pair, Some(Role::Master)) => match self.work.show_code() {
                Ok(showing) => {
                    self.showing = Some(showing);
                    self.screen = Screen::ShowCode;
                }
                Err(problem) => *self.log.lock().unwrap() = vec![problem],
            },
            (Action::Pair, Some(Role::Worker)) => {
                self.code.clear();
                self.screen = Screen::AskCode;
            }
            (Action::Cleanup, Some(role)) => self.screen = Screen::ConfirmCleanup(role),
            (Action::ReallyCleanup, Some(role)) => {
                self.screen = Screen::Menu(role);
                self.run("Cleaning up", move |work| work.cleanup(role));
            }
            (Action::SubmitCode, _) if self.code.len() == 8 => {
                let code = format!("{} {}", &self.code[..4], &self.code[4..]);
                self.screen = Screen::Menu(Role::Worker);
                self.run("Pairing", move |work| work.pair_with(&code));
            }
            (Action::TogglePublic, _) => self.public_networks = !self.public_networks,
            (Action::Back, Some(role)) if matches!(self.screen, Screen::ShowCode | Screen::AskCode | Screen::ConfirmCleanup(_)) => {
                // Leaving the code screen closes the master's pairing window.
                self.showing = None;
                self.go(Screen::Menu(role));
            }
            (Action::Back, _) => self.screen = Screen::Choose,
            _ => {}
        }
    }

    pub fn click(&mut self, x: i32, y: i32) {
        if let Some((_, action)) = self.buttons.iter().find(|(rect, _)| rect.contains(x, y)).copied() {
            self.act(action);
        }
    }

    pub fn pointer(&mut self, x: i32, y: i32) {
        self.pointer = (x, y);
    }

    pub fn key(&mut self, key: Key) {
        match (self.screen, key) {
            (Screen::AskCode, Key::Character(digit)) if digit.is_ascii_digit() && self.code.len() < 8 => self.code.push(digit),
            (Screen::AskCode, Key::Backspace) => {
                self.code.pop();
            }
            (Screen::AskCode, Key::Enter) => self.act(Action::SubmitCode),
            (Screen::Choose, Key::Escape) => {}
            (_, Key::Escape) => self.act(Action::Back),
            _ => {}
        }
    }

    /// Whether what is on screen may have changed by itself since it was last drawn (a job's lines, a paired worker,
    /// the dots that show something is running).
    pub fn changed(&mut self) -> bool {
        self.ticks = self.ticks.wrapping_add(1);
        let lines = self.log.lock().unwrap().len();
        let busy = self.busy.load(Ordering::SeqCst);
        // Something new to show; or traffic, whose lights flicker for as long as it lasts.
        let live_changed = self.screen == Screen::Choose && {
            let live = self.live.lock().unwrap();
            *live != self.drawn_live || !live.busy.is_empty()
        };
        let changed = lines != self.drawn_log || busy || live_changed || matches!(self.screen, Screen::ShowCode | Screen::AskCode);
        if lines != self.drawn_log && !busy {
            // A job has ended: what the status says may no longer be true.
            if let Screen::Menu(role) = self.screen {
                self.status = self.work.status(role);
            }
        }
        self.drawn_log = lines;
        changed
    }

    fn hovered(&self, rect: Rect) -> bool {
        !self.busy.load(Ordering::SeqCst) && rect.contains(self.pointer.0, self.pointer.1)
    }

    /// Notes a place as pressable, unless something is running.
    fn pressable(&mut self, rect: Rect, action: Action) {
        if !self.busy.load(Ordering::SeqCst) {
            self.buttons.push((rect, action));
        }
    }

    /// A plain button: filled when it is the main choice, outlined otherwise.
    fn button(&mut self, canvas: &mut Canvas, rect: Rect, label: &str, fill: Option<(Colour, Colour)>, action: Action) {
        let hovered = self.hovered(rect);
        let busy = self.busy.load(Ordering::SeqCst);
        match fill {
            Some((colour, hover)) => canvas.rounded(rect, self.u(11), if busy { LINE } else if hovered { hover } else { colour }),
            None => canvas.bordered(rect, self.u(11), self.u(1).max(1), LINE, if hovered { CARD_HOVER } else { BACKGROUND }),
        }
        let size = 15.0 * self.scale;
        canvas.centred(&self.bold, rect.x + rect.w / 2, rect.y + rect.h / 2 + self.u(5), size, if busy { FAINT } else { TEXT }, label);
        self.pressable(rect, action);
    }

    /// A small rounded label with a coloured dot: "Installed", "Paired with master".
    fn pill(&self, canvas: &mut Canvas, x: i32, y: i32, dot: Colour, label: &str) -> i32 {
        let size = 12.5 * self.scale;
        let width = Canvas::text_width(&self.regular, size, label) + self.u(34);
        canvas.rounded(Rect { x, y, w: width, h: self.u(26) }, self.u(13), CARD);
        canvas.circle(x + self.u(14), y + self.u(13), self.u(4), dot);
        canvas.text(&self.regular, x + self.u(24), y + self.u(18), size, SOFT, label);
        x + width + self.u(8)
    }

    /// The round badge at the left of a card, with a small drawing in it.
    fn badge(&self, canvas: &mut Canvas, cx: i32, cy: i32, radius: i32, colour: Colour, glyph: Glyph) {
        canvas.circle(cx, cy, radius, colour);
        let (x, y, s) = (cx as f32, cy as f32, self.scale);
        let white = 0xFFFFFF;
        match glyph {
            // A hub with three machines around it.
            Glyph::Master => {
                for (dx, dy) in [(0.0, -11.0), (-10.0, 8.0), (10.0, 8.0)] {
                    canvas.line((x, y), (x + dx * s, y + dy * s), 2.2 * s, white);
                    canvas.circle((x + dx * s) as i32, (y + dy * s) as i32, (3.6 * s) as i32, white);
                }
                canvas.circle(cx, cy, (5.5 * s) as i32, white);
            }
            // A screen on its stand.
            Glyph::Worker => {
                canvas.bordered(Rect { x: cx - (12.0 * s) as i32, y: cy - (10.0 * s) as i32, w: (24.0 * s) as i32, h: (16.0 * s) as i32 }, (3.0 * s) as i32, (2.2 * s) as i32, white, colour);
                canvas.line((x, y + 6.0 * s), (x, y + 10.0 * s), 2.2 * s, white);
                canvas.line((x - 6.0 * s, y + 11.0 * s), (x + 6.0 * s, y + 11.0 * s), 2.2 * s, white);
            }
            Glyph::Number(number) => {
                canvas.centred(&self.bold, cx, cy + self.u(5), 14.0 * s, white, &number.to_string());
            }
            Glyph::Warning => {
                canvas.line((x, y - 8.0 * s), (x, y + 2.0 * s), 3.0 * s, white);
                canvas.circle(cx, cy + (8.0 * s) as i32, (2.0 * s) as i32, white);
            }
        }
    }

    /// A wide card that is pressed as a whole: a badge, a title, a line under it, and an arrow.
    #[allow(clippy::too_many_arguments)]
    fn card(&mut self, canvas: &mut Canvas, rect: Rect, colour: Colour, glyph: Glyph, title: &str, title_colour: Colour, detail: &[&str], action: Action) {
        let hovered = self.hovered(rect);
        canvas.bordered(rect, self.u(16), self.u(1).max(1), if hovered { colour } else { LINE }, if hovered { CARD_HOVER } else { CARD });
        let radius = if rect.h > self.u(90) { self.u(26) } else { self.u(16) };
        let centre = rect.x + self.u(22) + radius;
        self.badge(canvas, centre, rect.y + rect.h / 2, radius, colour, glyph);
        let left = centre + radius + self.u(16);
        let lines = detail.len() as i32;
        let top = rect.y + rect.h / 2 - self.u(9 * lines);
        canvas.text(&self.bold, left, top + self.u(4), 16.0 * self.scale, title_colour, title);
        for (row, line) in detail.iter().enumerate() {
            canvas.text(&self.regular, left, top + self.u(24 + 18 * row as i32), 12.5 * self.scale, FAINT, line);
        }
        // The arrow at the right.
        let (ax, ay, s) = ((rect.x + rect.w - self.u(26)) as f32, (rect.y + rect.h / 2) as f32, self.scale);
        canvas.line((ax - 4.0 * s, ay - 6.0 * s), (ax + 2.0 * s, ay), 2.0 * s, FAINT);
        canvas.line((ax + 2.0 * s, ay), (ax - 4.0 * s, ay + 6.0 * s), 2.0 * s, FAINT);
        self.pressable(rect, action);
    }

    /// The eight places of a pairing code, in two groups of four.
    fn code_tiles(&self, canvas: &mut Canvas, top: i32, digits: &str, entering: bool) {
        let (tile, gap, middle) = (self.u(50), self.u(8), self.u(22));
        let total = 8 * tile + 6 * gap + middle;
        let mut x = (self.u(WIDTH) - total) / 2;
        for place in 0..8 {
            let digit = digits.chars().nth(place);
            let rect = Rect { x, y: top, w: tile, h: self.u(66) };
            // While typing, the next empty place is outlined, and its line blinks.
            let next = entering && place == digits.len();
            canvas.bordered(rect, self.u(11), self.u(2).max(1), if next { BLUE } else { TILE }, TILE);
            match digit {
                Some(digit) => canvas.centred(&self.bold, x + tile / 2, top + self.u(44), 30.0 * self.scale, TEXT, &digit.to_string()),
                None if next && self.ticks % 6 < 3 => canvas.rounded(Rect { x: x + tile / 2 - self.u(1), y: top + self.u(20), w: self.u(2), h: self.u(26) }, 0, BLUE),
                None => {}
            }
            x += tile + if place == 3 { middle } else { gap };
        }
    }

    /// The About screen: the icon, the name, the version, what it is, who made it and where it lives.
    fn draw_about(&mut self, canvas: &mut Canvas) {
        let s = self.scale;
        let middle = self.u(WIDTH / 2);
        if let Some(icon) = &self.icon {
            canvas.picture(icon, middle - self.u(64), self.u(96), self.u(128));
        }
        canvas.centred(&self.bold, middle, self.u(276), 25.0 * s, TEXT, "Master Control");
        canvas.centred(&self.regular, middle, self.u(306), 13.5 * s, SOFT, &format!("Version {0} ({0})", mc_core::VERSION));
        canvas.centred(&self.regular, middle, self.u(356), 15.0 * s, TEXT, mc_core::DESCRIPTION);
        canvas.centred(&self.regular, middle, self.u(410), 13.5 * s, SOFT, &format!("By {}", mc_core::AUTHOR));
        let width = Canvas::text_width(&self.regular, 13.5 * s, mc_core::PROJECT);
        let link = Rect { x: middle - width / 2 - self.u(6), y: self.u(418), w: width + self.u(12), h: self.u(26) };
        let colour = if self.hovered(link) { BLUE_HOVER } else { BLUE };
        canvas.centred(&self.regular, middle, self.u(436), 13.5 * s, colour, mc_core::PROJECT);
        let under = self.u(440) as f32;
        canvas.line(((middle - width / 2) as f32, under), ((middle + width / 2) as f32, under), s, colour);
        self.pressable(link, Action::OpenProject);
        let (host, rest) = self.identity.clone();
        canvas.centred(&self.regular, middle, self.u(500), 12.0 * s, FAINT, &format!("This machine: {host}"));
        canvas.centred(&self.regular, middle, self.u(520), 12.0 * s, FAINT, &rest);
        self.button(canvas, self.rect(32, HEIGHT - 88, WIDTH - 64, 48), "Done", None, Action::Back);
    }

    pub fn draw(&mut self, canvas: &mut Canvas) {
        self.buttons.clear();
        canvas.clear(BACKGROUND);
        let s = self.scale;
        let margin = 32;
        let wide = WIDTH - 2 * margin;

        if self.screen == Screen::About {
            return self.draw_about(canvas);
        }
        // The header: the icon, the name, and what this program is.
        if let Some(icon) = &self.icon {
            canvas.picture(icon, self.u(margin - 4), self.u(22), self.u(52));
        }
        canvas.text(&self.bold, self.u(margin + 60), self.u(48), 21.0 * s, TEXT, "Master Control");
        let end = canvas.text(&self.regular, self.u(margin + 60), self.u(67), 12.5 * s, FAINT, &format!("Version {}  ·  ", mc_core::VERSION));
        let about = Rect { x: end - self.u(4), y: self.u(53), w: Canvas::text_width(&self.regular, 12.5 * s, "About") + self.u(8), h: self.u(20) };
        canvas.text(&self.regular, end, self.u(67), 12.5 * s, if self.hovered(about) { BLUE_HOVER } else { BLUE }, "About");
        self.pressable(about, Action::About);
        // At the right: which machine this is.
        let (host, rest) = self.identity.clone();
        let right = self.u(WIDTH - margin);
        canvas.text(&self.bold, right - Canvas::text_width(&self.bold, 13.0 * s, &host), self.u(46), 13.0 * s, SOFT, &host);
        canvas.text(&self.regular, right - Canvas::text_width(&self.regular, 11.5 * s, &rest), self.u(65), 11.5 * s, FAINT, &rest);
        canvas.rounded(self.rect(0, 94, WIDTH, 1), 0, LINE);

        match self.screen {
            Screen::Choose => {
                canvas.text(&self.bold, self.u(margin), self.u(144), 25.0 * s, TEXT, "Set up this machine");
                canvas.text(&self.regular, self.u(margin), self.u(170), 14.0 * s, FAINT, "Choose what this machine is. Cleanup undoes it at any time.");
                self.card(canvas, self.rect(margin, 200, wide, 118), BLUE, Glyph::Master, "Master", TEXT, &["The machine your AI agent runs on.", "It controls itself and its workers."], Action::Choose(Role::Master));
                self.card(canvas, self.rect(margin, 334, wide, 118), PURPLE, Glyph::Worker, "Worker", TEXT, &["A machine the master controls.", "Nothing is signed in here."], Action::Choose(Role::Worker));
                // What is going on right now: the master, its workers and this machine's own worker, then the
                // master's latest calls as they happen.
                canvas.text(&self.bold, self.u(margin), self.u(480), 10.5 * s, FAINT, "STATUS");
                let panel = self.rect(margin, 490, wide, 168);
                canvas.bordered(panel, self.u(14), self.u(1).max(1), LINE, CARD);
                let live = self.live.lock().unwrap().clone();
                let room = 7usize;
                let lines: Vec<&Line> = live.lines.iter().take(room - 1).collect();
                let mut y = panel.y + self.u(26);
                if lines.is_empty() {
                    canvas.text(&self.regular, panel.x + self.u(16), y, 12.5 * s, FAINT, "Looking…");
                }
                for line in &lines {
                    let dot = match line.well {
                        Some(true) => GREEN,
                        Some(false) => ORANGE,
                        None => LINE,
                    };
                    let (cx, cy) = (panel.x + self.u(18), y - self.u(4));
                    if line.well == Some(true) && live.busy.contains(&line.name) {
                        // Traffic: the light flickers the way a disk's does, on and off at uneven moments, each line
                        // in its own rhythm.
                        let beat = self.ticks.wrapping_add(line.name.bytes().map(u32::from).sum::<u32>()).wrapping_mul(2_654_435_761) >> 29;
                        if beat >= 3 {
                            canvas.circle(cx, cy, self.u(6), LIGHT_GLOW);
                            canvas.circle(cx, cy, self.u(4), LIGHT_ON);
                        } else {
                            canvas.circle(cx, cy, self.u(4), LIGHT_OFF);
                        }
                    } else {
                        canvas.circle(cx, cy, self.u(4), dot);
                    }
                    let end = canvas.text(&self.bold, panel.x + self.u(32), y, 12.5 * s, TEXT, &line.name);
                    let mut says: String = line.says.chars().take(54).collect();
                    if says.len() < line.says.len() {
                        says.push('…');
                    }
                    canvas.text(&self.regular, end + self.u(10), y, 12.5 * s, SOFT, &says);
                    y += self.u(22);
                }
                // The calls take the rows that are left, the newest at the bottom.
                let left = room.saturating_sub(lines.len().max(1));
                let calls: Vec<&String> = live.calls.iter().rev().take(left).rev().collect();
                if !calls.is_empty() {
                    canvas.rounded(Rect { x: panel.x + self.u(16), y: y - self.u(14), w: panel.w - self.u(32), h: self.u(1).max(1) }, 0, LINE);
                    y += self.u(4);
                }
                for call in calls {
                    canvas.text(&self.regular, panel.x + self.u(18), y, 12.0 * s, FAINT, call);
                    y += self.u(20);
                }
                self.drawn_live = live;
                canvas.centred(&self.regular, self.u(WIDTH / 2), self.u(HEIGHT - 18), 11.0 * s, FAINT, "Master and workers find each other on your local network.");
            }
            Screen::Menu(role) => {
                let (name, colour) = if role == Role::Master { ("Master", BLUE) } else { ("Worker", PURPLE) };
                canvas.text(&self.bold, self.u(margin), self.u(144), 25.0 * s, TEXT, name);
                let status = self.status.clone();
                let mut x = self.pill(canvas, self.u(margin), self.u(158), if status.installed { GREEN } else { FAINT }, if status.installed { "Installed" } else { "Not installed" });
                let paired = match (role, status.paired_with.as_slice()) {
                    (_, []) => (FAINT, if role == Role::Master { "No workers paired".to_string() } else { "Not paired".to_string() }),
                    (Role::Master, [one]) => (GREEN, format!("1 worker: {one}")),
                    (Role::Master, many) => (GREEN, format!("{} workers: {}", many.len(), many.join(", "))),
                    (Role::Worker, [master, ..]) => (GREEN, format!("Paired with {master}")),
                };
                x = self.pill(canvas, x, self.u(158), paired.0, &paired.1);
                let _ = x;

                let for_agents = match status.agents.as_slice() {
                    [] => "Install what your AI agents need, or repair it.".to_string(),
                    agents => {
                        let ready = agents.iter().filter(|agent| agent.1).count();
                        let names: Vec<&str> = agents.iter().map(|agent| agent.0.as_str()).collect();
                        let tally = if ready == agents.len() { "all set up".to_string() } else { format!("{ready} of {} set up", agents.len()) };
                        format!("For {} ({tally}).", names.join(", "))
                    }
                };
                let start = if role == Role::Master { for_agents.as_str() } else { "Install the worker and start it with your session." };
                let pair = if role == Role::Master { "Show a code for your workers to type." } else { "Type the code the master shows." };
                self.card(canvas, self.rect(margin, 204, wide, 68), colour, Glyph::Number(1), "Start", TEXT, &[start], Action::Start);
                self.card(canvas, self.rect(margin, 282, wide, 68), colour, Glyph::Number(2), "Pair", TEXT, &[pair], Action::Pair);
                self.card(canvas, self.rect(margin, 360, wide, 68), RED, Glyph::Number(3), "Cleanup", TEXT, &["Remove everything Start and Pair put here."], Action::Cleanup);

                let mut top = 446;
                if role == Role::Worker && cfg!(windows) {
                    let rect = self.rect(margin, top, wide, 38);
                    let hovered = self.hovered(rect);
                    canvas.bordered(rect, self.u(11), self.u(1).max(1), LINE, if hovered { CARD_HOVER } else { BACKGROUND });
                    canvas.text(&self.regular, rect.x + self.u(16), rect.y + self.u(24), 13.0 * s, SOFT, "Reachable from public networks too");
                    // A switch: its knob at the right, and blue, when on.
                    let track = Rect { x: rect.x + rect.w - self.u(54), y: rect.y + self.u(9), w: self.u(38), h: self.u(20) };
                    canvas.rounded(track, self.u(10), if self.public_networks { BLUE } else { LINE });
                    canvas.circle(track.x + if self.public_networks { track.w - self.u(10) } else { self.u(10) }, track.y + self.u(10), self.u(7), 0xFFFFFF);
                    self.pressable(rect, Action::TogglePublic);
                    top += 50;
                }
                canvas.text(&self.bold, self.u(margin), self.u(top + 14), 10.5 * s, FAINT, "ACTIVITY");
                let panel = self.rect(margin, top + 24, wide, HEIGHT - top - 24 - 76);
                canvas.bordered(panel, self.u(14), self.u(1).max(1), LINE, CARD);
                let lines = self.log.lock().unwrap().clone();
                let room = ((panel.h - self.u(16)) / self.u(21)).max(1) as usize;
                if lines.is_empty() {
                    canvas.text(&self.regular, panel.x + self.u(16), panel.y + self.u(28), 13.0 * s, FAINT, "Nothing yet. Press Start to begin.");
                }
                let busy = self.busy.load(Ordering::SeqCst);
                let shown: Vec<&String> = lines.iter().rev().take(room).rev().collect();
                for (row, line) in shown.iter().enumerate() {
                    let y = panel.y + self.u(28 + 21 * row as i32);
                    let last = row + 1 == shown.len();
                    let bad = line.contains("NOT") || line.contains("didn't work") || line.starts_with("Not paired") || line.starts_with("No master");
                    let dot = if bad { ORANGE } else if busy && last { BLUE } else { GREEN };
                    canvas.circle(panel.x + self.u(18), y - self.u(4), self.u(3), dot);
                    let mut text: String = line.chars().take(66).collect();
                    if text.len() < line.len() {
                        text.push('…');
                    }
                    if busy && last {
                        // Three dots that take turns, to show it is still going.
                        text = text.trim_end_matches('…').to_string() + &".".repeat(1 + (self.ticks / 3 % 3) as usize);
                    }
                    canvas.text(&self.regular, panel.x + self.u(30), y, 13.0 * s, if bad { ORANGE } else { SOFT }, &text);
                }
                self.button(canvas, self.rect(margin, HEIGHT - 60, 110, 40), "Back", None, Action::Back);
            }
            Screen::ShowCode => {
                canvas.text(&self.bold, self.u(margin), self.u(144), 25.0 * s, TEXT, "Pair your workers");
                canvas.text(&self.regular, self.u(margin), self.u(170), 14.0 * s, FAINT, "On each worker choose Worker, then Pair, and type this code.");
                let (code, paired, left) = self.showing.as_ref().map(|showing| (showing.code(), showing.paired(), showing.seconds_left())).unwrap_or_default();
                let digits: String = code.chars().filter(char::is_ascii_digit).collect();
                self.code_tiles(canvas, self.u(204), &digits, false);
                canvas.centred(&self.regular, self.u(WIDTH / 2), self.u(298), 12.5 * s, FAINT, &format!("The code works for {}:{:02} more. Several workers can use it.", left / 60, left % 60));
                canvas.text(&self.bold, self.u(margin), self.u(344), 10.5 * s, FAINT, "PAIRED IN THIS SESSION");
                let panel = self.rect(margin, 354, wide, HEIGHT - 354 - 76);
                canvas.bordered(panel, self.u(14), self.u(1).max(1), LINE, CARD);
                if paired.is_empty() {
                    let waiting = format!("Waiting for workers{}", ".".repeat(1 + (self.ticks / 3 % 3) as usize));
                    canvas.text(&self.regular, panel.x + self.u(16), panel.y + self.u(28), 13.0 * s, FAINT, &waiting);
                }
                for (row, (name, address)) in paired.iter().take(9).enumerate() {
                    let y = panel.y + self.u(28 + 24 * row as i32);
                    canvas.circle(panel.x + self.u(20), y - self.u(4), self.u(4), GREEN);
                    let end = canvas.text(&self.bold, panel.x + self.u(34), y, 14.0 * s, TEXT, name);
                    canvas.text(&self.regular, end + self.u(10), y, 12.5 * s, FAINT, address);
                }
                self.button(canvas, self.rect(WIDTH - margin - 110, HEIGHT - 60, 110, 40), "Done", Some((BLUE, BLUE_HOVER)), Action::Back);
            }
            Screen::AskCode => {
                canvas.text(&self.bold, self.u(margin), self.u(144), 25.0 * s, TEXT, "Pair with your master");
                canvas.text(&self.regular, self.u(margin), self.u(170), 14.0 * s, FAINT, "Open Pair on the master and type the code it shows.");
                let digits = self.code.clone();
                self.code_tiles(canvas, self.u(204), &digits, true);
                let hint = if self.code.len() == 8 { "Press Pair, or Enter." } else { "Eight digits. The master is found on your network by itself." };
                canvas.centred(&self.regular, self.u(WIDTH / 2), self.u(298), 12.5 * s, FAINT, hint);
                self.button(canvas, self.rect(margin, HEIGHT - 60, 110, 40), "Back", None, Action::Back);
                if self.code.len() == 8 {
                    self.button(canvas, self.rect(WIDTH - margin - 110, HEIGHT - 60, 110, 40), "Pair", Some((BLUE, BLUE_HOVER)), Action::SubmitCode);
                }
            }
            Screen::ConfirmCleanup(role) => {
                self.badge(canvas, self.u(margin + 22), self.u(140), self.u(22), RED, Glyph::Warning);
                canvas.text(&self.bold, self.u(margin + 58), self.u(149), 23.0 * s, TEXT, "Remove everything?");
                let what = if role == Role::Master {
                    ["Every worker is told to forget this master.", "Your AI agents lose their access to Master Control.", "The master's folder and program are deleted."]
                } else {
                    ["The worker stops and no longer starts with your session.", "Its pairing, logs and recorded sessions are deleted.", "Its master can't reach it any more."]
                };
                let panel = self.rect(margin, 186, wide, 112);
                canvas.bordered(panel, self.u(14), self.u(1).max(1), LINE, CARD);
                for (row, line) in what.iter().enumerate() {
                    let y = panel.y + self.u(32 + 28 * row as i32);
                    canvas.circle(panel.x + self.u(20), y - self.u(4), self.u(3), RED);
                    canvas.text(&self.regular, panel.x + self.u(34), y, 13.5 * s, SOFT, line);
                }
                canvas.text(&self.regular, self.u(margin), self.u(326), 12.5 * s, FAINT, "This can't be undone. Start sets it up again from nothing.");
                // The safe choice first, and the one a stray Escape lands on.
                self.button(canvas, self.rect(margin, 350, wide, 48), "Cancel", Some((BLUE, BLUE_HOVER)), Action::Back);
                self.button(canvas, self.rect(margin, 408, wide, 48), "Remove everything", Some((RED, RED_HOVER)), Action::ReallyCleanup);
            }
            // Drawn above, on its own.
            Screen::About => {}
        }
    }
}

#[derive(Clone, Copy)]
enum Glyph {
    Master,
    Worker,
    Number(u8),
    Warning,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Noted(Mutex<Vec<String>>);

    struct Code;

    impl Showing for Code {
        fn code(&self) -> String {
            "1234 5678".into()
        }
        fn paired(&self) -> Vec<(String, String)> {
            vec![("linux".into(), "192.0.2.41:47901".into())]
        }
        fn seconds_left(&self) -> u64 {
            571
        }
    }

    impl Work for Noted {
        fn start(&self, role: Role, public_networks: bool) -> Vec<String> {
            self.0.lock().unwrap().push(format!("start {role:?} {public_networks}"));
            vec!["started".into()]
        }
        fn cleanup(&self, role: Role) -> Vec<String> {
            self.0.lock().unwrap().push(format!("cleanup {role:?}"));
            vec!["removed".into()]
        }
        fn show_code(&self) -> Result<Box<dyn Showing>, String> {
            Ok(Box::new(Code))
        }
        fn pair_with(&self, code: &str) -> Vec<String> {
            self.0.lock().unwrap().push(format!("pair {code}"));
            vec!["paired with master".into()]
        }
        fn status(&self, role: Role) -> Status {
            Status { installed: role == Role::Master, paired_with: vec!["linux".into()], agents: vec![("Claude Code".into(), true), ("Codex".into(), false)] }
        }
        fn identity(&self) -> (String, String) {
            ("test-machine".into(), "TestOS 1.0 · aa:bb:cc:dd:ee:ff".into())
        }
        fn open(&self, address: &str) {
            self.0.lock().unwrap().push(format!("open {address}"));
        }
        fn live(&self) -> Live {
            Live {
                lines: vec![
                    Line { well: Some(true), name: "This machine".into(), says: "is the master, for Claude Code, Codex".into() },
                    Line { well: Some(true), name: "linux".into(), says: "worker, answers".into() },
                    Line { well: Some(false), name: "windows".into(), says: "worker, doesn't answer".into() },
                ],
                calls: vec!["3 s ago   linux   screenshot   ok   212 ms".into()],
                busy: Vec::new(),
            }
        }
        fn busy(&self) -> Vec<String> {
            self.0.lock().unwrap().iter().filter_map(|said| said.strip_prefix("busy ").map(str::to_string)).collect()
        }
    }

    fn app() -> (App, Arc<Noted>, Canvas) {
        let noted = Arc::new(Noted::default());
        (App::new(noted.clone(), 1.0), noted, Canvas::new(WIDTH as usize, HEIGHT as usize))
    }

    /// Clicks the middle of the button that does `action`, as drawn.
    fn press(app: &mut App, canvas: &mut Canvas, action: Action) {
        app.draw(canvas);
        let rect = app.buttons.iter().find(|(_, drawn)| *drawn == action).unwrap_or_else(|| panic!("no {action:?} button on {:?}", app.screen)).0;
        app.click(rect.x + rect.w / 2, rect.y + rect.h / 2);
    }

    fn settle(app: &mut App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.busy.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn the_status_section_shows_what_is_going_on_and_redraws_when_it_changes() {
        let (mut app, _noted, mut canvas) = app();
        app.look_now();
        assert!(app.changed(), "something new to show");
        app.draw(&mut canvas);
        assert_eq!(app.drawn_live.lines.len(), 3);
        assert!(canvas.pixels.contains(&GREEN) && canvas.pixels.contains(&ORANGE), "a worker that answers and one that doesn't");
        let drawn = canvas.pixels.clone();
        // Nothing new: nothing to redraw (the thread that keeps it fresh may have looked again, and found the same).
        app.look_now();
        app.drawn_log = app.log.lock().unwrap().len();
        assert!(!app.changed());
        // A worker comes back: the next tick redraws, and the picture differs.
        let mut live = app.live.lock().unwrap().clone();
        live.lines[2] = Line { well: Some(true), name: "windows".into(), says: "answers".into() };
        live.calls.push("0 s ago   windows   click   ok   40 ms".into());
        app.set_live(live);
        // (set before the watching thread's next look, which would put the stand-in's own answer back)
        if app.changed() {
            app.draw(&mut canvas);
            assert_ne!(canvas.pixels, drawn);
        }
        // Traffic to a worker: its light flickers, lit at some ticks and nearly out at others, for as long as it
        // lasts, and is steady again afterwards.
        _noted.0.lock().unwrap().push("busy linux".into());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !app.live.lock().unwrap().busy.contains(&"linux".to_string()) {
            assert!(std::time::Instant::now() < deadline, "the watching thread takes up who is busy");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (mut lit, mut out) = (0, 0);
        for _ in 0..40 {
            assert!(app.changed(), "a flickering light is redrawn at every tick");
            app.draw(&mut canvas);
            lit += usize::from(canvas.pixels.contains(&LIGHT_ON));
            out += usize::from(canvas.pixels.contains(&LIGHT_OFF));
        }
        assert!(lit >= 5 && out >= 5, "on {lit} ticks and off {out} ticks of 40");
        assert!(canvas.pixels.contains(&GREEN), "the lines without traffic stay steady");
        _noted.0.lock().unwrap().clear();
        while !app.live.lock().unwrap().busy.is_empty() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        app.draw(&mut canvas);
        assert!(!canvas.pixels.contains(&LIGHT_ON) && !canvas.pixels.contains(&LIGHT_OFF));
        // On other screens the section isn't shown, and its changes ask for no redraw.
        press(&mut app, &mut canvas, Action::Choose(Role::Worker));
        app.set_live(Live::default());
        app.drawn_log = app.log.lock().unwrap().len();
        assert!(!app.changed());
    }

    #[test]
    fn about_shows_from_any_screen_and_goes_back_to_it() {
        let (mut app, noted, mut canvas) = app();
        press(&mut app, &mut canvas, Action::About);
        assert_eq!(app.screen, Screen::About);
        press(&mut app, &mut canvas, Action::OpenProject);
        assert_eq!(noted.0.lock().unwrap().last().unwrap(), "open https://github.com/dunkelherzmann/master-control");
        press(&mut app, &mut canvas, Action::Back);
        assert_eq!(app.screen, Screen::Choose);

        press(&mut app, &mut canvas, Action::Choose(Role::Worker));
        press(&mut app, &mut canvas, Action::About);
        app.act(Action::About);
        app.key(Key::Escape);
        assert_eq!(app.screen, Screen::Menu(Role::Worker), "back to where it was opened from, however often it was asked for");
        // The screen is drawn: the icon's colours and the text's ink are on it.
        app.act(Action::About);
        app.draw(&mut canvas);
        let distinct: std::collections::HashSet<u32> = canvas.pixels.iter().copied().collect();
        assert!(distinct.len() > 200 && canvas.pixels.contains(&TEXT), "{}", distinct.len());
    }

    #[test]
    fn the_master_starts_shows_its_code_and_cleans_up_only_when_sure() {
        let (mut app, noted, mut canvas) = app();
        press(&mut app, &mut canvas, Action::Choose(Role::Master));
        assert_eq!((app.screen, app.status.installed, app.status.paired_with.len()), (Screen::Menu(Role::Master), true, 1));
        press(&mut app, &mut canvas, Action::Start);
        settle(&mut app);
        assert_eq!(*app.log.lock().unwrap(), ["Starting…", "started"]);
        assert!(app.changed(), "the new lines are noticed");

        press(&mut app, &mut canvas, Action::Pair);
        assert_eq!(app.screen, Screen::ShowCode);
        app.draw(&mut canvas);
        assert!(app.buttons.iter().all(|(_, action)| matches!(action, Action::Back | Action::About)), "the code screen has Done, and About in the header as everywhere");
        // Looking at About keeps the pairing window open, and comes back to the code.
        press(&mut app, &mut canvas, Action::About);
        press(&mut app, &mut canvas, Action::Back);
        assert!(app.showing.is_some() && app.screen == Screen::ShowCode);
        press(&mut app, &mut canvas, Action::Back);
        assert!(app.showing.is_none() && app.screen == Screen::Menu(Role::Master), "Done closes the pairing window");

        press(&mut app, &mut canvas, Action::Cleanup);
        assert_eq!(app.screen, Screen::ConfirmCleanup(Role::Master));
        app.key(Key::Escape);
        assert_eq!(app.screen, Screen::Menu(Role::Master), "Escape is Cancel");
        assert!(!noted.0.lock().unwrap().iter().any(|line| line.starts_with("cleanup")));
        press(&mut app, &mut canvas, Action::Cleanup);
        press(&mut app, &mut canvas, Action::ReallyCleanup);
        settle(&mut app);
        assert_eq!(*noted.0.lock().unwrap(), ["start Master false", "cleanup Master"]);
        press(&mut app, &mut canvas, Action::Back);
        assert_eq!(app.screen, Screen::Choose);
    }

    #[test]
    fn the_worker_asks_for_eight_digits_and_pairs_with_them() {
        let (mut app, noted, mut canvas) = app();
        press(&mut app, &mut canvas, Action::Choose(Role::Worker));
        press(&mut app, &mut canvas, Action::Pair);
        assert_eq!(app.screen, Screen::AskCode);
        for key in "12a3 4".chars() {
            app.key(Key::Character(key));
        }
        assert_eq!(app.code, "1234", "only digits count");
        app.key(Key::Enter);
        assert_eq!(app.screen, Screen::AskCode, "not before all eight are there");
        app.draw(&mut canvas);
        assert!(!app.buttons.iter().any(|(_, action)| *action == Action::SubmitCode));
        app.key(Key::Backspace);
        for key in "456789".chars() {
            app.key(Key::Character(key));
        }
        assert_eq!(app.code, "12345678", "and no more than eight");
        press(&mut app, &mut canvas, Action::SubmitCode);
        settle(&mut app);
        assert_eq!(*noted.0.lock().unwrap(), ["pair 1234 5678"], "sent as the master shows it");
        assert_eq!(app.screen, Screen::Menu(Role::Worker));
        assert_eq!(app.log.lock().unwrap().last().map(String::as_str), Some("paired with master"));
    }

    #[test]
    fn a_button_does_nothing_while_something_is_running() {
        let (mut app, noted, mut canvas) = app();
        press(&mut app, &mut canvas, Action::Choose(Role::Worker));
        app.busy.store(true, Ordering::SeqCst);
        app.draw(&mut canvas);
        assert!(app.buttons.is_empty(), "nothing can be pressed");
        app.act(Action::Start);
        app.busy.store(false, Ordering::SeqCst);
        assert!(noted.0.lock().unwrap().is_empty());
    }

    #[test]
    fn every_screen_draws_within_the_window() {
        let (mut app, _, mut canvas) = app();
        for screen in [Screen::Choose, Screen::Menu(Role::Master), Screen::Menu(Role::Worker), Screen::ShowCode, Screen::AskCode, Screen::ConfirmCleanup(Role::Master), Screen::ConfirmCleanup(Role::Worker)] {
            app.screen = screen;
            app.showing = Some(Box::new(Code));
            app.draw(&mut canvas);
            assert!(canvas.pixels.contains(&TEXT), "{screen:?} has text");
            for (rect, _) in &app.buttons {
                assert!(rect.x >= 0 && rect.y >= 0 && rect.x + rect.w <= WIDTH && rect.y + rect.h <= HEIGHT, "{screen:?} {rect:?}");
            }
        }
    }

    #[test]
    fn a_denser_display_gets_a_larger_sharper_window() {
        let noted = Arc::new(Noted::default());
        let mut app = App::new(noted, 1.5);
        assert_eq!(app.size(), (840, 1050));
        let mut canvas = Canvas::new(840, 1050);
        app.draw(&mut canvas);
        let master = app.buttons.iter().find(|(_, action)| *action == Action::Choose(Role::Master)).unwrap().0;
        assert_eq!((master.x, master.w), (48, 744), "every length grows with the zoom");
        app.click(master.x + 10, master.y + 10);
        assert_eq!(app.screen, Screen::Menu(Role::Master));
    }
}

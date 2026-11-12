//! Seeing and driving the desktop (specs.md §3): one small interface, done by each platform's own means.

pub mod keys;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod atspi;
#[cfg(windows)]
mod uia;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
mod x11_clipboard;

use std::fmt;

pub use keys::Key;

#[derive(Debug)]
pub struct DesktopError {
    /// `not_supported`: this machine or session can't do it. `failed`: it can, and it went wrong.
    pub code: &'static str,
    pub message: String,
}

impl DesktopError {
    pub fn failed(message: impl fmt::Display) -> DesktopError {
        DesktopError { code: "failed", message: message.to_string() }
    }

    pub fn not_supported(message: impl fmt::Display) -> DesktopError {
        DesktopError { code: "not_supported", message: message.to_string() }
    }

    /// The element a client named isn't where it was when the tree was read.
    pub fn gone(message: impl fmt::Display) -> DesktopError {
        DesktopError { code: "element_gone", message: message.to_string() }
    }
}

pub type Result<T> = std::result::Result<T, DesktopError>;

/// A rectangle in desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && i64::from(x) < i64::from(self.x) + i64::from(self.width) && i64::from(y) < i64::from(self.y) + i64::from(self.height)
    }

    /// The part of this rectangle that lies inside the other.
    pub fn clip(&self, bounds: &Rect) -> Option<Rect> {
        let left = self.x.max(bounds.x);
        let top = self.y.max(bounds.y);
        let right = (i64::from(self.x) + i64::from(self.width)).min(i64::from(bounds.x) + i64::from(bounds.width));
        let bottom = (i64::from(self.y) + i64::from(self.height)).min(i64::from(bounds.y) + i64::from(bounds.height));
        (right > i64::from(left) && bottom > i64::from(top)).then(|| Rect {
            x: left,
            y: top,
            width: (right - i64::from(left)) as u32,
            height: (bottom - i64::from(top)) as u32,
        })
    }
}

/// A picture, three bytes a pixel: red, green, blue.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Image {
    /// A smaller copy, each pixel the average of the pixels it stands for.
    pub fn scaled_to_width(&self, width: u32) -> Image {
        if width == 0 || width >= self.width {
            return Image { width: self.width, height: self.height, rgb: self.rgb.clone() };
        }
        let height = ((u64::from(self.height) * u64::from(width)) / u64::from(self.width)).max(1) as u32;
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for row in 0..height {
            let top = (u64::from(row) * u64::from(self.height) / u64::from(height)) as u32;
            let bottom = ((u64::from(row + 1) * u64::from(self.height) / u64::from(height)) as u32).clamp(top + 1, self.height);
            for column in 0..width {
                let left = (u64::from(column) * u64::from(self.width) / u64::from(width)) as u32;
                let right = ((u64::from(column + 1) * u64::from(self.width) / u64::from(width)) as u32).clamp(left + 1, self.width);
                let mut sums = [0u32; 3];
                for y in top..bottom {
                    for x in left..right {
                        let at = ((y * self.width + x) * 3) as usize;
                        for (sum, value) in sums.iter_mut().zip(&self.rgb[at..at + 3]) {
                            *sum += u32::from(*value);
                        }
                    }
                }
                let count = (bottom - top) * (right - left);
                rgb.extend(sums.iter().map(|sum| (sum / count) as u8));
            }
        }
        Image { width, height, rgb }
    }

    pub fn to_png(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(DesktopError::failed)?;
        writer.write_image_data(&self.rgb).map_err(DesktopError::failed)?;
        writer.finish().map_err(DesktopError::failed)?;
        Ok(bytes)
    }
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    /// The app's name, as the platform has it: the program's file name on Windows, the window class on X11.
    pub app: String,
    pub pid: u32,
    pub frame: Rect,
    pub focused: bool,
    pub minimized: bool,
}

/// One thing in a window as assistive technology sees it: a button, a text field, a menu item.
#[derive(Debug, Clone, Default)]
pub struct Element {
    /// What it is, in the same words on every platform where there is one: `button`, `text`, `label`, `check_box`,
    /// `menu_item`, `list_item`, `tab`, `window`, `dialog`, `pane`... Otherwise the platform's own name for it.
    pub role: String,
    pub name: String,
    /// What it holds: a field's text, a slider's number. None when it holds nothing.
    pub value: Option<String>,
    /// Where it is on the desktop. None when it has no place (not shown, or the app doesn't say).
    pub frame: Option<Rect>,
    pub enabled: bool,
    pub focused: bool,
    pub children: Vec<Element>,
}

/// How much of a tree to read.
#[derive(Debug, Clone, Copy)]
pub struct TreeLimits {
    pub depth: u32,
    pub elements: usize,
}

/// The platform's role names in the words used on every platform.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn common_role(platform_name: &str) -> String {
    let name = platform_name.trim().to_lowercase().replace([' ', '-'], "_");
    match name.as_str() {
        "push_button" | "toggle_button" | "split_button" => "button",
        "edit" | "entry" | "password_text" | "text_box" => "text",
        "static" | "static_text" | "label" => "label",
        "checkbox" | "check_box" => "check_box",
        "radiobutton" | "radio_button" => "radio_button",
        "combobox" | "combo_box" => "combo_box",
        "menubar" | "menu_bar" => "menu_bar",
        "menuitem" | "menu_item" | "check_menu_item" | "radio_menu_item" => "menu_item",
        "listitem" | "list_item" => "list_item",
        "treeitem" | "tree_item" => "tree_item",
        "tabitem" | "tab_item" | "page_tab" => "tab",
        "page_tab_list" | "tab_list" => "tab_list",
        "toolbar" | "tool_bar" => "tool_bar",
        "statusbar" | "status_bar" => "status_bar",
        "scrollbar" | "scroll_bar" => "scroll_bar",
        "progressbar" | "progress_bar" => "progress_bar",
        "titlebar" | "title_bar" => "title_bar",
        "hyperlink" | "link" => "link",
        "frame" | "window" => "window",
        "panel" | "pane" | "filler" => "pane",
        "dataitem" | "table_cell" | "cell" => "cell",
        "datagrid" | "table" => "table",
        "spin_button" | "spinner" => "spinner",
        "icon" | "image" => "image",
        _ => return name,
    }
    .to_string()
}

/// Something done to an element itself, through assistive technology: no pointer, no keyboard, and the window
/// needn't be in front.
#[derive(Debug, Clone, PartialEq)]
pub enum ElementAction {
    /// What a click on it would do: a button pressed, a link followed, a menu item chosen.
    Press,
    SetValue(String),
    /// Ticked or not; None flips it.
    Toggle(Option<bool>),
    Expand(bool),
    Select,
    /// Give it the keyboard, so that what is typed next goes into it.
    Focus,
}

/// An element as a tree named it: the way down to it from the window, child by child, and what it was then.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub path: &'a [u32],
    pub role: &'a str,
    pub name: &'a str,
}

impl Target<'_> {
    /// Refuses when what is at the end of the path is no longer what the tree showed there.
    #[cfg_attr(not(any(windows, target_os = "linux", target_os = "macos")), allow(dead_code))]
    pub(crate) fn check(&self, role: &str, name: &str) -> Result<()> {
        if role == self.role && name == self.name {
            return Ok(());
        }
        Err(DesktopError::gone(format!("there is now a {role} \"{name}\" where the {} \"{}\" was", self.role, self.name)))
    }
}

/// How the element is after the action, as far as it tells.
#[derive(Debug, Default, PartialEq)]
pub struct Acted {
    pub value: Option<String>,
    pub on: Option<bool>,
    pub expanded: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// One machine's desktop. Positions are desktop pixels; windows come frontmost first.
pub trait Desktop {
    /// The whole desktop: every monitor.
    fn bounds(&self) -> Result<Rect>;
    fn capture(&self, area: Rect) -> Result<Image>;
    fn windows(&self) -> Result<Vec<WindowInfo>>;
    /// Asks for the window to come to the front and take the keyboard. Whether it did is for the caller to check.
    fn focus(&self, window: u64) -> Result<()>;
    fn pointer(&self) -> Result<(i32, i32)>;
    fn move_pointer(&self, x: i32, y: i32) -> Result<()>;
    fn button(&self, button: Button, down: bool) -> Result<()>;
    /// Turns the wheel by notches: `down` positive is towards the user, `right` positive is to the right.
    fn scroll(&self, right: i32, down: i32) -> Result<()>;
    fn key(&self, key: Key, down: bool) -> Result<()>;
    /// Types text into whatever has the keyboard, whatever the keyboard layout.
    fn text(&self, text: &str) -> Result<()>;
    /// What is in a window, as assistive technology sees it, and whether the limits cut it short.
    fn tree(&self, window: &WindowInfo, limits: TreeLimits) -> Result<(Element, bool)>;
    /// Does something to one element of a window. `not_supported` when that element can't do it.
    fn act(&self, window: &WindowInfo, target: Target, action: &ElementAction) -> Result<Acted>;
    /// Does what a click at a point of a window would, without the pointer and without bringing the window forward:
    /// presses what is there, or gives a text field the keyboard. Says what it did.
    fn quiet_click(&self, _window: &WindowInfo, _x: i32, _y: i32) -> Result<String> {
        Err(quiet_is_not_built())
    }
    /// What has the keyboard inside the window's app, as (role, name): where typed text and keys will go. None
    /// where the system can't tell.
    fn keyboard_owner(&self, _window: &WindowInfo) -> Option<(String, String)> {
        None
    }
    /// Types text into a window's app without the real keyboard and without bringing the window forward.
    fn quiet_text(&self, _window: &WindowInfo, _text: &str) -> Result<()> {
        Err(quiet_is_not_built())
    }
    /// Presses a key or a chord in a window's app the same way.
    fn quiet_chord(&self, _window: &WindowInfo, _chord: &[Key]) -> Result<()> {
        Err(quiet_is_not_built())
    }
    /// The text on the clipboard; None when it is empty or holds something that isn't text.
    fn clipboard(&self) -> Result<Option<String>>;
    fn set_clipboard(&self, text: &str) -> Result<()>;
}

fn quiet_is_not_built() -> DesktopError {
    DesktopError::not_supported("input that leaves the pointer, the keyboard and the window order alone is only built for macOS; leave `background` out")
}

/// This session's desktop, or why there is none to work with.
pub fn open() -> Result<Box<dyn Desktop>> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows::WindowsDesktop::open()?))
    }
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty()) {
            Ok(Box::new(x11::X11Desktop::open()?))
        } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            Err(DesktopError::not_supported("Wayland sessions without an X display aren't supported yet"))
        } else {
            Err(DesktopError::not_supported("the agent isn't running in a desktop session (DISPLAY is not set)"))
        }
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::MacDesktop::open()?))
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Err(DesktopError::not_supported("screen and input aren't built for this system yet"))
    }
}

/// Whether [`open`] can work on this kind of system at all.
pub fn built_in() -> bool {
    cfg!(any(windows, target_os = "linux", target_os = "macos"))
}

/// Whether pictures and input work right now, and what the user has yet to allow. Only macOS has permissions to
/// wait for; elsewhere both work as soon as there is a desktop.
pub fn ready() -> (bool, bool, Vec<&'static str>) {
    #[cfg(target_os = "macos")]
    {
        let (screen, input) = macos::permissions();
        let missing = [(!screen).then_some("Screen Recording"), (!input).then_some("Accessibility")].into_iter().flatten().collect();
        (screen, input, missing)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let works = built_in() && open().is_ok();
        (works, works, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_get_the_same_words_on_every_platform() {
        for (platform, common) in [
            ("push button", "button"), ("button", "button"), ("edit", "text"), ("entry", "text"), ("static", "label"), ("label", "label"),
            ("check box", "check_box"), ("check_box", "check_box"), ("page tab", "tab"), ("tab_item", "tab"), ("page tab list", "tab_list"),
            ("tab_list", "tab_list"), ("frame", "window"), ("window", "window"), ("panel", "pane"), ("filler", "pane"), ("menu item", "menu_item"),
            ("list item", "list_item"), ("Tree Item", "tree_item"), ("something-new", "something_new"),
        ] {
            assert_eq!(common_role(platform), common, "{platform}");
        }
    }

    #[test]
    fn rectangles_clip_and_contain() {
        let screen = Rect { x: 0, y: 0, width: 100, height: 50 };
        assert!(screen.contains(0, 0) && screen.contains(99, 49));
        assert!(!screen.contains(100, 0) && !screen.contains(0, 50) && !screen.contains(-1, 0));
        let window = Rect { x: -20, y: 30, width: 50, height: 50 };
        assert_eq!(window.clip(&screen), Some(Rect { x: 0, y: 30, width: 30, height: 20 }));
        assert_eq!(Rect { x: 200, y: 0, width: 5, height: 5 }.clip(&screen), None);
    }

    #[test]
    fn a_picture_shrinks_by_averaging() {
        // Four pixels wide, two high: a black half and a white half.
        let mut rgb = Vec::new();
        for _ in 0..2 {
            rgb.extend([0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255]);
        }
        let image = Image { width: 4, height: 2, rgb };
        let half = image.scaled_to_width(2);
        assert_eq!((half.width, half.height), (2, 1));
        assert_eq!(half.rgb, [0, 0, 0, 255, 255, 255]);
        let one = image.scaled_to_width(1);
        assert_eq!(one.rgb, [127, 127, 127]);
        assert_eq!(image.scaled_to_width(9).width, 4, "never larger than it was");
    }

    #[test]
    fn a_picture_becomes_a_png_that_reads_back() {
        let image = Image { width: 2, height: 1, rgb: vec![255, 0, 0, 0, 0, 255] };
        let bytes = image.to_png().unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut pixels = vec![0; 6];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(pixels, [255, 0, 0, 0, 0, 255]);
    }
}

//! Linux on GNOME Shell under Wayland. Wayland gives other programs no window list, no picture of the screen and no
//! way to send input, and GNOME Shell keeps its own D-Bus services for these to a few programs of its own; so a small
//! extension of ours (`linux/gnome-extension/`) runs inside the shell and serves them on the session bus. Input goes
//! through a virtual device of the shell's own seat: to apps it is the real pointer and keyboard. What is in a window
//! is read through AT-SPI, as on X11.

use std::path::PathBuf;

use serde::Deserialize;
use zbus::blocking::Connection;

use super::{Button, Desktop, DesktopError, Image, Key, Rect, Result, WindowInfo};

pub const UUID: &str = "master-control@dunkelherzmann.github.io";
pub const METADATA: &str = include_str!("../../../linux/gnome-extension/metadata.json");
pub const EXTENSION: &str = include_str!("../../../linux/gnome-extension/extension.js");

const NAME: &str = "io.github.dunkelherzmann.MasterControl";
const PATH: &str = "/io/github/dunkelherzmann/MasterControl";
/// The extension's `API`: raised with every change to its methods.
const API: u32 = 2;

fn failed<E: std::fmt::Display>(error: E) -> DesktopError {
    DesktopError::failed(format!("GNOME Shell: {error}"))
}

/// Where the extension is installed for this user.
pub fn folder() -> PathBuf {
    crate::state::user_home().join(".local/share/gnome-shell/extensions").join(UUID)
}

fn name_has_owner(connection: &Connection, name: &str) -> bool {
    connection
        .call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "NameHasOwner", &(name,))
        .ok()
        .and_then(|reply| reply.body().deserialize::<bool>().ok())
        .unwrap_or(false)
}

/// The API of the extension the running shell has loaded; None when it has none.
pub fn loaded_api() -> Option<u32> {
    let connection = Connection::session().ok()?;
    connection.call_method(Some(NAME), PATH, Some(NAME), "Api", &()).ok()?.body().deserialize().ok()
}

/// Whether this session's desktop is GNOME Shell (on either display system).
pub fn shell_runs() -> bool {
    Connection::session().is_ok_and(|connection| name_has_owner(&connection, "org.gnome.Shell"))
}

pub struct GnomeDesktop {
    connection: Connection,
}

#[derive(Deserialize)]
struct Screen {
    width: u32,
    height: u32,
    locked: bool,
}

#[derive(Deserialize)]
struct Window {
    id: String,
    title: String,
    app: String,
    pid: i64,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    focused: bool,
    minimized: bool,
    /// Where the app's own part of the window starts: below the title bar GNOME Shell draws for an X11 app.
    client_x: i32,
    client_y: i32,
}

impl GnomeDesktop {
    pub fn open() -> Result<GnomeDesktop> {
        let connection = Connection::session()
            .map_err(|error| DesktopError::not_supported(format!("the agent can't reach the session bus, so it isn't in a desktop session: {error}")))?;
        if !name_has_owner(&connection, NAME) {
            return Err(DesktopError::not_supported(if name_has_owner(&connection, "org.gnome.Shell") {
                format!(
                    "GNOME Shell runs on Wayland here, and its Master Control extension ({UUID}) isn't running: `mc-agent start` installs and enables it, and GNOME Shell loads new extensions when the user next signs in"
                )
            } else {
                "this Wayland desktop isn't GNOME Shell; only GNOME is supported on Wayland so far".to_string()
            }));
        }
        let desktop = GnomeDesktop { connection };
        let api: u32 = desktop.call("Api", &())?;
        if api != API {
            return Err(DesktopError::not_supported(format!(
                "GNOME Shell still runs an older Master Control extension (API {api}, this agent needs {API}): the new one, already installed, loads when the user next signs in"
            )));
        }
        Ok(desktop)
    }

    fn call<B, R>(&self, method: &str, body: &B) -> Result<R>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        self.connection.call_method(Some(NAME), PATH, Some(NAME), method, body).map_err(failed)?.body().deserialize().map_err(failed)
    }

    fn act(&self, method: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> Result<()> {
        self.connection.call_method(Some(NAME), PATH, Some(NAME), method, body).map_err(failed)?;
        Ok(())
    }

    fn listed(&self) -> Result<Vec<Window>> {
        let json: String = self.call("Windows", &())?;
        serde_json::from_str(&json).map_err(failed)
    }

    fn screen(&self) -> Result<Screen> {
        let json: String = self.call("Screen", &())?;
        serde_json::from_str(&json).map_err(failed)
    }

    /// Input and pictures mean nothing on the lock screen: say so instead of acting on it.
    fn unlocked(&self) -> Result<()> {
        if self.screen()?.locked {
            return Err(DesktopError::failed("the screen is locked: unlock the session first (as root: loginctl unlock-session)"));
        }
        Ok(())
    }
}

/// A PNG as three bytes a pixel.
fn decode(png: &[u8]) -> Result<Image> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(failed)?;
    let mut pixels = vec![0; reader.output_buffer_size().ok_or_else(|| failed("the picture is too large"))?];
    let frame = reader.next_frame(&mut pixels).map_err(failed)?;
    let channels = frame.color_type.samples();
    let rgb = match frame.color_type {
        png::ColorType::Rgb => pixels[..frame.buffer_size()].to_vec(),
        png::ColorType::Rgba => pixels[..frame.buffer_size()].as_chunks::<4>().0.iter().flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect(),
        png::ColorType::Grayscale | png::ColorType::GrayscaleAlpha => {
            pixels[..frame.buffer_size()].chunks(channels).flat_map(|pixel| [pixel[0]; 3]).collect()
        }
        png::ColorType::Indexed => return Err(failed("the picture came back with a palette")),
    };
    Ok(Image { width: frame.width, height: frame.height, rgb })
}

impl Desktop for GnomeDesktop {
    fn bounds(&self) -> Result<Rect> {
        let screen = self.screen()?;
        Ok(Rect { x: 0, y: 0, width: screen.width, height: screen.height })
    }

    fn capture(&self, area: Rect) -> Result<Image> {
        let png: Vec<u8> = self.call("Capture", &(area.x, area.y, area.width as i32, area.height as i32))?;
        let image = decode(&png)?;
        // On a scaled monitor the shell gives device pixels; positions stay desktop pixels.
        Ok(if image.width > area.width { image.scaled_to_width(area.width) } else { image })
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        Ok(self
            .listed()?
            .into_iter()
            .map(|window| WindowInfo {
                id: window.id.parse().unwrap_or(0),
                title: window.title,
                app: window.app,
                pid: u32::try_from(window.pid).unwrap_or(0),
                frame: Rect { x: window.x, y: window.y, width: window.width, height: window.height },
                focused: window.focused,
                minimized: window.minimized,
            })
            .collect())
    }

    fn focus(&self, window: u64) -> Result<()> {
        self.unlocked()?;
        let found: bool = self.call("Activate", &(window,))?;
        if !found {
            return Err(DesktopError::gone("there is no such window now"));
        }
        Ok(())
    }

    fn pointer(&self) -> Result<(i32, i32)> {
        self.call("Pointer", &())
    }

    fn move_pointer(&self, x: i32, y: i32) -> Result<()> {
        self.unlocked()?;
        self.act("MovePointer", &(f64::from(x), f64::from(y)))
    }

    fn button(&self, button: Button, down: bool) -> Result<()> {
        // Clutter numbers them as X11 does.
        let number: u32 = match button {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 3,
        };
        self.act("Button", &(number, down))
    }

    fn scroll(&self, right: i32, down: i32) -> Result<()> {
        self.act("Scroll", &(right, down))
    }

    fn key(&self, key: Key, down: bool) -> Result<()> {
        self.act("Key", &(super::x11::keysym(key), down))
    }

    fn text(&self, text: &str) -> Result<()> {
        self.unlocked()?;
        let _how: String = self.call("Text", &(text,))?;
        Ok(())
    }

    fn tree(&self, window: &WindowInfo, limits: super::TreeLimits) -> Result<(super::Element, bool)> {
        // Apps count positions from their own part of the window, which starts below a title bar the shell draws.
        let listed = self.listed()?.into_iter().find(|listed| listed.id == window.id.to_string());
        let mut own = window.clone();
        if let Some(listed) = listed {
            own.frame.x = listed.client_x;
            own.frame.y = listed.client_y;
        }
        super::atspi::tree(&own, limits)
    }

    fn act(&self, window: &WindowInfo, target: super::Target, action: &super::ElementAction) -> Result<super::Acted> {
        super::atspi::act(window, target, action)
    }

    fn clipboard(&self) -> Result<Option<String>> {
        let text: String = self.call("GetClipboard", &())?;
        Ok((!text.is_empty()).then_some(text))
    }

    fn set_clipboard(&self, text: &str) -> Result<()> {
        self.act("SetClipboard", &(text,))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_built_in_is_the_one_the_agent_speaks_to() {
        let metadata: serde_json::Value = serde_json::from_str(METADATA).unwrap();
        assert_eq!(metadata["uuid"], UUID);
        assert!(EXTENSION.contains(&format!("const API = {API};")), "raise API in extension.js and gnome.rs together");
        assert!(EXTENSION.contains(&format!("'{NAME}'")) && EXTENSION.contains(&format!("'{PATH}'")));
    }

    #[test]
    fn pictures_come_back_as_three_bytes_a_pixel() {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        writer.finish().unwrap();
        let image = decode(&bytes).unwrap();
        assert_eq!((image.width, image.height, image.rgb), (2, 1, vec![255, 0, 0, 0, 0, 255]));
    }
}

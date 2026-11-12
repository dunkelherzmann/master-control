//! Windows: pictures with GDI, input with SendInput, windows with the window manager's own list.

use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use std::sync::Once;

use windows_sys::Win32::Foundation::{CloseHandle, GlobalFree, HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop};
use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, VkKeyScanW, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, SetCursorPos, SetForegroundWindow, ShowWindow, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_RESTORE,
};

use super::{Button, Desktop, DesktopError, Image, Key, Rect, Result, WindowInfo};

pub struct WindowsDesktop;

static DPI: Once = Once::new();

fn last_error(what: &str) -> DesktopError {
    DesktopError::failed(format!("Windows: {what}: {}", std::io::Error::last_os_error()))
}

impl WindowsDesktop {
    pub fn open() -> Result<WindowsDesktop> {
        // Without this, positions and pictures are scaled by the display's zoom and no longer match the pixels.
        DPI.call_once(|| unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        });
        // SAFETY: plain calls; the handle is closed before returning.
        let desktop = unsafe { OpenInputDesktop(0, 0, 0x0001) };
        if desktop.is_null() {
            return Err(DesktopError::not_supported(
                "the agent isn't running in the signed-in desktop session (a service or an SSH session has no screen)",
            ));
        }
        unsafe { CloseDesktop(desktop) };
        Ok(WindowsDesktop)
    }

    fn send(&self, inputs: &[INPUT]) -> Result<()> {
        // SAFETY: the slice is valid for its length and INPUT's size is passed.
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(last_error("the input was refused (a window with higher rights in front, or a locked screen)"))
        }
    }
}

fn mouse(flags: u32, data: i32) -> INPUT {
    INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
}

fn keyboard(virtual_key: u16, scan: u16, flags: u32) -> INPUT {
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: virtual_key, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
}

/// The virtual-key code for a key, and whether it is one of the "extended" keys.
fn virtual_key(key: Key) -> Result<(u16, bool)> {
    Ok(match key {
        Key::Char(character) => {
            let mut units = [0u16; 2];
            let units = character.encode_utf16(&mut units);
            // SAFETY: a plain call.
            let scanned = if units.len() == 1 { unsafe { VkKeyScanW(units[0]) } } else { -1 };
            if scanned == -1 {
                return Err(DesktopError::failed(format!("Windows: this keyboard layout has no key for {character:?}; use type_text for the character")));
            }
            ((scanned & 0xff) as u16, false)
        }
        Key::Enter => (0x0d, false),
        Key::Tab => (0x09, false),
        Key::Escape => (0x1b, false),
        Key::Backspace => (0x08, false),
        Key::Delete => (0x2e, true),
        Key::Insert => (0x2d, true),
        Key::Home => (0x24, true),
        Key::End => (0x23, true),
        Key::PageUp => (0x21, true),
        Key::PageDown => (0x22, true),
        Key::Left => (0x25, true),
        Key::Up => (0x26, true),
        Key::Right => (0x27, true),
        Key::Down => (0x28, true),
        Key::Space => (0x20, false),
        Key::F(number) => (0x70 + u16::from(number) - 1, false),
        Key::Ctrl => (0x11, false),
        Key::Shift => (0x10, false),
        Key::Alt => (0x12, false),
        Key::Meta => (0x5b, true),
        Key::CapsLock => (0x14, false),
        Key::PrintScreen => (0x2c, true),
        Key::Menu => (0x5d, true),
    })
}

/// The clipboard format for text as UTF-16 (CF_UNICODETEXT).
const UNICODE_TEXT: u32 = 13;

/// Opens the clipboard, waiting a moment if another program has it open.
fn open_clipboard() -> Result<()> {
    for _ in 0..40 {
        // SAFETY: a plain call.
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Err(last_error("the clipboard is held by another program"))
}

struct Listing {
    windows: Vec<WindowInfo>,
    foreground: HWND,
}

unsafe extern "system" fn each_window(window: HWND, parameter: LPARAM) -> i32 {
    // SAFETY: the parameter is the Listing that `windows` passed, alive for the whole enumeration.
    let listing = unsafe { &mut *(parameter as *mut Listing) };
    unsafe {
        let length = GetWindowTextLengthW(window);
        if IsWindowVisible(window) == 0 || length <= 0 {
            return 1;
        }
        // Windows that are "shown" but kept off screen by the system: apps on other virtual desktops, suspended apps.
        let mut cloaked = 0u32;
        DwmGetWindowAttribute(window, DWMWA_CLOAKED as u32, (&mut cloaked as *mut u32).cast(), size_of::<u32>() as u32);
        if cloaked != 0 {
            return 1;
        }
        let mut title = vec![0u16; length as usize + 1];
        let copied = GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32);
        title.truncate(copied.max(0) as usize);
        // The frame as drawn; GetWindowRect includes an invisible border around it.
        let mut frame: RECT = zeroed();
        if DwmGetWindowAttribute(window, DWMWA_EXTENDED_FRAME_BOUNDS as u32, (&mut frame as *mut RECT).cast(), size_of::<RECT>() as u32) != 0 {
            GetWindowRect(window, &mut frame);
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        let minimized = IsIconic(window) != 0;
        if !minimized && (frame.right <= frame.left || frame.bottom <= frame.top) {
            return 1;
        }
        listing.windows.push(WindowInfo {
            id: window as usize as u64,
            title: String::from_utf16_lossy(&title),
            app: program_name(pid),
            pid,
            frame: Rect { x: frame.left, y: frame.top, width: (frame.right - frame.left).max(0) as u32, height: (frame.bottom - frame.top).max(0) as u32 },
            focused: window == listing.foreground,
            minimized,
        });
    }
    1
}

/// The file name of a process's program, without the folder; empty when it can't be asked.
fn program_name(pid: u32) -> String {
    // SAFETY: the handle is checked and closed; the buffer's real size is passed.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return String::new();
        }
        let mut path = [0u16; 1024];
        let mut length = path.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0;
        CloseHandle(process);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&path[..length as usize]);
        path.rsplit('\\').next().unwrap_or_default().to_string()
    }
}

impl Desktop for WindowsDesktop {
    fn bounds(&self) -> Result<Rect> {
        // SAFETY: plain calls.
        let (x, y, width, height) = unsafe {
            (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN))
        };
        if width <= 0 || height <= 0 {
            return Err(DesktopError::not_supported("Windows: there is no screen in this session"));
        }
        Ok(Rect { x, y, width: width as u32, height: height as u32 })
    }

    fn capture(&self, area: Rect) -> Result<Image> {
        let (width, height) = (area.width as i32, area.height as i32);
        let mut pixels = vec![0u8; area.width as usize * area.height as usize * 4];
        // SAFETY: every object made here is released here; the buffer is as large as the header says.
        let copied = unsafe {
            let screen = GetDC(null_mut());
            let memory = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, width, height);
            let before = SelectObject(memory, bitmap);
            let blitted = BitBlt(memory, 0, 0, width, height, screen, area.x, area.y, SRCCOPY | CAPTUREBLT);
            // A bitmap must not be selected into a device context while its bits are read.
            SelectObject(memory, before);
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative: rows from the top down.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..zeroed()
            };
            let rows = if blitted != 0 { GetDIBits(screen, bitmap, 0, area.height, pixels.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS) } else { 0 };
            DeleteObject(bitmap);
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
            rows
        };
        if copied != height {
            return Err(last_error("the screen couldn't be copied"));
        }
        let mut rgb = Vec::with_capacity(area.width as usize * area.height as usize * 3);
        for pixel in pixels.as_chunks::<4>().0 {
            rgb.extend([pixel[2], pixel[1], pixel[0]]);
        }
        Ok(Image { width: area.width, height: area.height, rgb })
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        // SAFETY: the Listing outlives the call, and EnumWindows calls back only during it.
        unsafe {
            let mut listing = Listing { windows: Vec::new(), foreground: GetForegroundWindow() };
            if EnumWindows(Some(each_window), &mut listing as *mut Listing as LPARAM) == 0 {
                return Err(last_error("the windows couldn't be listed"));
            }
            Ok(listing.windows)
        }
    }

    fn focus(&self, window: u64) -> Result<()> {
        let window = window as usize as HWND;
        // SAFETY: plain calls on a handle the system checks.
        unsafe {
            if IsWindow(window) == 0 {
                return Err(DesktopError::failed("Windows: the window is gone"));
            }
            if IsIconic(window) != 0 {
                ShowWindow(window, SW_RESTORE);
            }
            // Windows lets a process take the foreground only if it made the last input. A mouse move of no distance
            // counts, and changes nothing on screen.
            let _ = self.send(&[mouse(MOUSEEVENTF_MOVE, 0)]);
            SetForegroundWindow(window);
            BringWindowToTop(window);
        }
        Ok(())
    }

    fn pointer(&self) -> Result<(i32, i32)> {
        let mut point = POINT { x: 0, y: 0 };
        // SAFETY: the pointer is to a local.
        if unsafe { GetCursorPos(&mut point) } == 0 {
            return Err(last_error("the pointer's place couldn't be read"));
        }
        Ok((point.x, point.y))
    }

    fn move_pointer(&self, x: i32, y: i32) -> Result<()> {
        // SAFETY: a plain call.
        if unsafe { SetCursorPos(x, y) } == 0 {
            return Err(last_error("the pointer couldn't be moved"));
        }
        // SetCursorPos places the pointer; this makes it a real mouse event for the window under it.
        self.send(&[mouse(MOUSEEVENTF_MOVE, 0)])
    }

    fn button(&self, button: Button, down: bool) -> Result<()> {
        let flags = match (button, down) {
            (Button::Left, true) => MOUSEEVENTF_LEFTDOWN,
            (Button::Left, false) => MOUSEEVENTF_LEFTUP,
            (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
            (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
            (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
            (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
        };
        self.send(&[mouse(flags, 0)])
    }

    fn scroll(&self, right: i32, down: i32) -> Result<()> {
        // One notch is 120; for the vertical wheel, positive is away from the user.
        if down != 0 {
            self.send(&[mouse(MOUSEEVENTF_WHEEL, -down * 120)])?;
        }
        if right != 0 {
            self.send(&[mouse(MOUSEEVENTF_HWHEEL, right * 120)])?;
        }
        Ok(())
    }

    fn key(&self, key: Key, down: bool) -> Result<()> {
        let (code, extended) = virtual_key(key)?;
        // SAFETY: a plain call.
        let scan = unsafe { MapVirtualKeyW(u32::from(code), 0) } as u16;
        let flags = if extended { KEYEVENTF_EXTENDEDKEY } else { 0 } | if down { 0 } else { KEYEVENTF_KEYUP };
        self.send(&[keyboard(code, scan, flags)])
    }

    fn tree(&self, window: &WindowInfo, limits: super::TreeLimits) -> Result<(super::Element, bool)> {
        super::uia::tree(window.id, limits)
    }

    fn act(&self, window: &WindowInfo, target: super::Target, action: &super::ElementAction) -> Result<super::Acted> {
        super::uia::act(window.id, target, action)
    }

    fn keyboard_owner(&self, window: &WindowInfo) -> Option<(String, String)> {
        // Only when that window is the one in front: the keyboard has one owner for the whole desktop.
        self.windows().ok()?.into_iter().find(|candidate| candidate.id == window.id).filter(|now| now.focused)?;
        super::uia::keyboard_owner()
    }

    fn clipboard(&self) -> Result<Option<String>> {
        open_clipboard()?;
        // SAFETY: the clipboard is open, so the handle stays valid until it is closed; the memory is read only up to
        // its own size and unlocked before closing.
        let text = unsafe {
            let handle = GetClipboardData(UNICODE_TEXT);
            let text = if handle.is_null() {
                None
            } else {
                let units = GlobalLock(handle) as *const u16;
                if units.is_null() {
                    None
                } else {
                    let most = GlobalSize(handle) / 2;
                    let length = (0..most).find(|index| *units.add(*index) == 0).unwrap_or(most);
                    let text = String::from_utf16_lossy(std::slice::from_raw_parts(units, length));
                    GlobalUnlock(handle);
                    Some(text)
                }
            };
            CloseClipboard();
            text
        };
        Ok(text)
    }

    fn set_clipboard(&self, text: &str) -> Result<()> {
        let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
        open_clipboard()?;
        // SAFETY: the block is as large as what is copied into it. Once SetClipboardData succeeds the system owns it;
        // until then it is ours to free.
        let stored = unsafe {
            EmptyClipboard();
            let block = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
            let stored = if block.is_null() {
                false
            } else {
                let target = GlobalLock(block) as *mut u16;
                if target.is_null() {
                    GlobalFree(block);
                    false
                } else {
                    std::ptr::copy_nonoverlapping(units.as_ptr(), target, units.len());
                    GlobalUnlock(block);
                    let taken = !SetClipboardData(UNICODE_TEXT, block).is_null();
                    if !taken {
                        GlobalFree(block);
                    }
                    taken
                }
            };
            CloseClipboard();
            stored
        };
        if stored {
            Ok(())
        } else {
            Err(last_error("the text couldn't be put on the clipboard"))
        }
    }

    fn text(&self, text: &str) -> Result<()> {
        for character in text.chars() {
            match character {
                '\r' => {}
                '\n' => self.send(&[keyboard(0x0d, 0, 0), keyboard(0x0d, 0, KEYEVENTF_KEYUP)])?,
                '\t' => self.send(&[keyboard(0x09, 0, 0), keyboard(0x09, 0, KEYEVENTF_KEYUP)])?,
                _ => {
                    // The character itself, not a key: the same whatever the keyboard layout.
                    let mut units = [0u16; 2];
                    for unit in character.encode_utf16(&mut units) {
                        self.send(&[keyboard(0, *unit, KEYEVENTF_UNICODE), keyboard(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)])?;
                    }
                }
            }
        }
        Ok(())
    }
}

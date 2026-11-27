//! Windows: pictures with GDI, input with SendInput, windows with the window manager's own list.

use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Once, OnceLock};

use windows_sys::Win32::Foundation::{LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, EnumThreadWindows, GetClassNameW, GetMessageW, MessageBoxW, PostMessageW, WM_CLOSE, SetWindowsHookExW, IDOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LLMHF_INJECTED,
    LLMHF_LOWER_IL_INJECTED, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_OK, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL,
    WH_MOUSE_LL,
};

use windows_sys::Win32::Foundation::{CloseHandle, GlobalFree, HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, VkKeyScanW, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed, SetCursorPos, SetForegroundWindow, SetWindowPos, ShowWindow,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SW_MAXIMIZE, SW_MINIMIZE,
    SW_RESTORE,
};

use super::{Button, Desktop, DesktopError, Image, Key, Rect, Result, VirtualDesktop, WindowInfo, WindowState};

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

impl WindowsDesktop {
    /// Presses a chord and lets every key go, last first.
    fn shortcut(&self, chord: &[Key]) -> Result<()> {
        let mut down = Vec::new();
        let pressed = chord.iter().try_for_each(|key| {
            self.key(*key, true)?;
            down.push(*key);
            std::thread::sleep(std::time::Duration::from_millis(15));
            Ok::<(), DesktopError>(())
        });
        let released: Vec<_> = down.iter().rev().map(|key| self.key(*key, false)).collect();
        pressed?;
        released.into_iter().collect()
    }
}

/// Where Explorer keeps the virtual desktops: their ids in order, the one shown, and the names given them.
const DESKTOPS_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops";

/// A virtual desktop's id: a GUID's bytes as the registry stores them.
type DesktopId = [u8; 16];

/// The desktops' ids in order, and the one shown.
fn virtual_desktops() -> Result<(Vec<DesktopId>, Option<DesktopId>)> {
    let ids: Vec<[u8; 16]> = registry_bytes(DESKTOPS_KEY, "VirtualDesktopIDs").unwrap_or_default().as_chunks::<16>().0.to_vec();
    let current = registry_bytes(DESKTOPS_KEY, "CurrentVirtualDesktop").and_then(|bytes| <[u8; 16]>::try_from(bytes.as_slice()).ok());
    // A session that never made a second desktop lists none: it has the one.
    if ids.is_empty() {
        return Ok((vec![current.unwrap_or([0; 16])], Some(current.unwrap_or([0; 16]))));
    }
    Ok((ids, current))
}

/// A GUID's bytes as the registry names its key: {XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}, the first three parts
/// stored little-endian.
fn guid_text(bytes: &[u8; 16]) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_le_bytes([bytes[4], bytes[5]]),
        u16::from_le_bytes([bytes[6], bytes[7]]),
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// A value of the user's registry, as its bytes; None when there is none.
fn registry_bytes(key: &str, value: &str) -> Option<Vec<u8>> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_ANY};
    let (key, value) = (wide(key), wide(value));
    let mut size = 0u32;
    // SAFETY: the names are zero-ended; the first call asks the size, the second fills a buffer of that size.
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_ANY, null_mut(), null_mut(), &mut size) != 0 {
            return None;
        }
        let mut bytes = vec![0u8; size as usize];
        if RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_ANY, null_mut(), bytes.as_mut_ptr().cast(), &mut size) != 0 {
            return None;
        }
        bytes.truncate(size as usize);
        Some(bytes)
    }
}

fn registry_string(key: &str, value: &str) -> Option<String> {
    let bytes = registry_bytes(key, value)?;
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).take_while(|unit| *unit != 0).collect();
    Some(String::from_utf16_lossy(&units))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// The window's handle, if it still exists.
fn live(window: &WindowInfo) -> Result<HWND> {
    let handle = window.id as usize as HWND;
    // SAFETY: a plain call; the system checks the handle.
    if unsafe { IsWindow(handle) } == 0 {
        return Err(DesktopError::failed("Windows: the window is gone"));
    }
    Ok(handle)
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
            // Refused all the same (another app has just taken the foreground, or holds the keyboard): joined to the
            // input of the thread in front for a moment, this thread counts as that one, which may give it away.
            let front = GetForegroundWindow();
            if front != window && !front.is_null() {
                let ours = GetCurrentThreadId();
                let theirs = GetWindowThreadProcessId(front, null_mut());
                if theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, 1) != 0 {
                    SetForegroundWindow(window);
                    BringWindowToTop(window);
                    AttachThreadInput(ours, theirs, 0);
                }
            }
        }
        Ok(())
    }

    fn place_window(&self, window: &WindowInfo, frame: Rect) -> Result<()> {
        let handle = live(window)?;
        // SAFETY: plain calls on a handle the system checks; the rectangles are locals.
        unsafe {
            if IsIconic(handle) != 0 || IsZoomed(handle) != 0 {
                ShowWindow(handle, SW_RESTORE);
            }
            // `windows` gives the frame as drawn; SetWindowPos takes the rectangle with the invisible border around it.
            let (mut outer, mut drawn): (RECT, RECT) = (zeroed(), zeroed());
            GetWindowRect(handle, &mut outer);
            if DwmGetWindowAttribute(handle, DWMWA_EXTENDED_FRAME_BOUNDS as u32, (&mut drawn as *mut RECT).cast(), size_of::<RECT>() as u32) != 0 {
                drawn = outer;
            }
            let (left, top) = (drawn.left - outer.left, drawn.top - outer.top);
            let (right, bottom) = (outer.right - drawn.right, outer.bottom - drawn.bottom);
            let width = frame.width as i32 + left + right;
            let height = frame.height as i32 + top + bottom;
            if SetWindowPos(handle, null_mut(), frame.x - left, frame.y - top, width, height, SWP_NOZORDER | SWP_NOACTIVATE) == 0 {
                return Err(last_error("the window couldn't be moved"));
            }
        }
        Ok(())
    }

    fn set_window_state(&self, window: &WindowInfo, state: WindowState) -> Result<()> {
        let handle = live(window)?;
        let command = match state {
            WindowState::Normal => SW_RESTORE,
            WindowState::Minimized => SW_MINIMIZE,
            WindowState::Maximized => SW_MAXIMIZE,
        };
        // SAFETY: a plain call on a handle the system checks. Its answer is whether the window was visible before.
        unsafe { ShowWindow(handle, command) };
        Ok(())
    }

    fn desktops(&self) -> Result<Vec<VirtualDesktop>> {
        let (ids, current) = virtual_desktops()?;
        Ok(ids
            .iter()
            .map(|id| VirtualDesktop { name: registry_string(&format!("{DESKTOPS_KEY}\\Desktops\\{}", guid_text(id)), "Name").unwrap_or_default(), current: Some(*id) == current })
            .collect())
    }

    // Windows offers no published way to change desktops but its own shortcuts, which it documents: Win+Ctrl+Left
    // and Right switch, Win+Ctrl+D adds one, Win+Ctrl+F4 closes the one shown.
    fn switch_desktop(&self, index: usize) -> Result<()> {
        let (ids, current) = virtual_desktops()?;
        let from = current.and_then(|current| ids.iter().position(|id| *id == current)).ok_or_else(|| DesktopError::failed("Windows: which desktop is shown can't be read"))?;
        let toward = if index > from { Key::Right } else { Key::Left };
        for _ in 0..from.abs_diff(index) {
            self.shortcut(&[Key::Meta, Key::Ctrl, toward])?;
            // Each switch slides; one sent during the slide is lost.
            std::thread::sleep(std::time::Duration::from_millis(350));
        }
        Ok(())
    }

    fn create_desktop(&self) -> Result<()> {
        self.shortcut(&[Key::Meta, Key::Ctrl, Key::Char('d')])
    }

    fn remove_desktop(&self, index: usize) -> Result<()> {
        let (ids, current) = virtual_desktops()?;
        if current != ids.get(index).copied() {
            self.switch_desktop(index)?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while virtual_desktops()?.1 != ids.get(index).copied() {
                if std::time::Instant::now() >= deadline {
                    return Err(DesktopError::failed("Windows: the desktop to remove couldn't be shown first"));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        self.shortcut(&[Key::Meta, Key::Ctrl, Key::F(4)])
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

    fn idle(&self) -> Option<std::time::Duration> {
        // What the watcher saw; GetLastInputInfo would count the agent's and tests' own SendInput as someone's.
        let last = LAST_REAL_INPUT_MS.load(Ordering::Relaxed);
        (last > 0).then(|| std::time::Duration::from_millis(since_start_ms().saturating_sub(last)))
    }

    fn ask(&self, message: &str) -> Result<bool> {
        // A "done" pop-up still open would sit where this one opens, and a click meant for it would answer this.
        close_told();
        ASKING.store(true, Ordering::Relaxed);
        let (text, caption) = (wide(message), wide("Master Control"));
        // Cancel is the default button, so a person still typing can't answer OK by accident with Enter.
        // SAFETY: a plain call.
        bring_up_box_of(unsafe { GetCurrentThreadId() });
        // SAFETY: both strings end in a nul and outlive the call.
        let answer = unsafe {
            MessageBoxW(null_mut(), text.as_ptr(), caption.as_ptr(), MB_OKCANCEL | MB_ICONINFORMATION | MB_DEFBUTTON2 | MB_TOPMOST | MB_SETFOREGROUND)
        };
        ASKING.store(false, Ordering::Relaxed);
        match answer {
            0 => Err(last_error("the pop-up couldn't be shown")),
            IDOK => Ok(true),
            _ => Ok(false),
        }
    }

    fn tell(&self, message: &str) {
        let (text, caption) = (wide(message), wide("Master Control"));
        // On a thread of its own, so the agent goes on; the box stays until it is clicked.
        // Never over a question still waiting for its answer: it would hide it.
        if ASKING.load(Ordering::Relaxed) {
            return;
        }
        close_told();
        std::thread::spawn(move || {
            // SAFETY: plain calls; both strings end in a nul and are owned by this thread for the whole call.
            let thread = unsafe { GetCurrentThreadId() };
            TOLD_THREAD.store(thread, Ordering::Relaxed);
            bring_up_box_of(thread);
            unsafe { MessageBoxW(null_mut(), text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND) };
        });
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

/// A question is open (every pop-up of the machine comes from the one worker program).
static ASKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The thread showing the "done" pop-up, while it may still be open; 0 for none.
static TOLD_THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

unsafe extern "system" fn close_window(window: HWND, _: LPARAM) -> windows_sys::core::BOOL {
    // SAFETY: a plain call on a window the system just listed.
    unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    1
}

/// The message box among a thread's windows (class `#32770`; the thread may also own hidden helper windows).
unsafe extern "system" fn first_window(window: HWND, found: LPARAM) -> windows_sys::core::BOOL {
    let mut class = [0u16; 16];
    // SAFETY: the buffer's length is passed; `found` is the address of the caller's handle, live for the listing.
    unsafe {
        let length = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
        if String::from_utf16_lossy(&class[..length.max(0) as usize]) != "#32770" || IsWindowVisible(window) == 0 {
            return 1;
        }
        *(found as *mut HWND) = window;
    }
    0
}

/// Brings the pop-up a thread is about to show to the front once it is there: the topmost flag alone leaves it behind
/// a full-screen app (a browser playing a video), and Windows keeps the foreground from a program the person isn't
/// using. The way `focus` does it gets it there.
fn bring_up_box_of(thread: u32) {
    std::thread::spawn(move || {
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let mut found: HWND = null_mut();
            // SAFETY: the procedure writes only to `found`, which outlives the listing.
            unsafe { EnumThreadWindows(thread, Some(first_window), &mut found as *mut HWND as LPARAM) };
            if !found.is_null() {
                let _ = WindowsDesktop.focus(found as usize as u64);
                return;
            }
        }
    });
}

fn close_told() {
    let thread = TOLD_THREAD.swap(0, Ordering::Relaxed);
    if thread != 0 {
        // SAFETY: the system lists that thread's windows; the procedure only posts to them.
        unsafe { EnumThreadWindows(thread, Some(close_window), 0) };
    }
}

// ------------------------------------------------------------------------------------------------ real input

/// When a person last used the real mouse or keyboard, in milliseconds since the watcher started; 0 for not yet.
static LAST_REAL_INPUT_MS: AtomicU64 = AtomicU64::new(0);
static WATCH_START: OnceLock<std::time::Instant> = OnceLock::new();

fn since_start_ms() -> u64 {
    WATCH_START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

unsafe extern "system" fn mouse_seen(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: for a code of 0 or more the system passes a valid MSLLHOOKSTRUCT.
    unsafe {
        if code >= 0 && ((*(lparam as *const MSLLHOOKSTRUCT)).flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED)) == 0 {
            LAST_REAL_INPUT_MS.store(since_start_ms(), Ordering::Relaxed);
        }
        CallNextHookEx(null_mut(), code, wparam, lparam)
    }
}

unsafe extern "system" fn key_seen(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: for a code of 0 or more the system passes a valid KBDLLHOOKSTRUCT.
    unsafe {
        if code >= 0 && ((*(lparam as *const KBDLLHOOKSTRUCT)).flags & (LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED)) == 0 {
            LAST_REAL_INPUT_MS.store(since_start_ms(), Ordering::Relaxed);
        }
        CallNextHookEx(null_mut(), code, wparam, lparam)
    }
}

/// Watches the desktop's mouse and keyboard on a thread of its own, noting input that no program sent: only a
/// low-level hook is told which input was injected.
pub fn watch_input() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        since_start_ms();
        std::thread::spawn(|| unsafe {
            // SAFETY: the procedures live as long as the program; the thread keeps pumping messages, which the
            // hooks need, until the program ends.
            let module = GetModuleHandleW(std::ptr::null());
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_seen), module, 0);
            let keys = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_seen), module, 0);
            if mouse.is_null() && keys.is_null() {
                return;
            }
            let mut message: MSG = zeroed();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {}
        });
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_desktop_id_reads_as_the_registry_names_its_key() {
        // The current desktop's bytes on the test machine, and the key Explorer made for it.
        let bytes = [0x78, 0x3a, 0x07, 0x98, 0xd1, 0x55, 0xa5, 0x46, 0x89, 0x30, 0x93, 0xa7, 0x3f, 0xcc, 0x0b, 0x3b];
        assert_eq!(super::guid_text(&bytes), "{98073A78-55D1-46A5-8930-93A73FCC0B3B}");
    }
}

//! The window on Windows: one Win32 window whose whole face is the picture the program draws.

use std::cell::RefCell;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, SetDIBitsToDevice, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, PAINTSTRUCT};
use windows_sys::Win32::System::Console::FreeConsole;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateIconFromResourceEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, LoadCursorW, PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage, CW_USEDEFAULT,
    IDC_ARROW, MSG, SW_SHOWNORMAL, WM_CHAR, WM_DESTROY, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

use crate::canvas::Canvas;
use crate::ui::{App, Key, Work};

thread_local! {
    /// The window procedure is a plain function; this is how it reaches the app. Only the window's thread uses it.
    static STATE: RefCell<Option<(App, Canvas)>> = const { RefCell::new(None) };
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn with<T>(act: impl FnOnce(&mut App, &mut Canvas) -> T) -> Option<T> {
    STATE.with(|state| state.borrow_mut().as_mut().map(|(app, canvas)| act(app, canvas)))
}

unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let point = || (i32::from(lparam as u16 as i16), i32::from((lparam >> 16) as u16 as i16));
    let redraw = || unsafe { InvalidateRect(window, std::ptr::null(), 0) };
    match message {
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = unsafe { std::mem::zeroed() };
            let device = unsafe { BeginPaint(window, &mut paint) };
            with(|app, canvas| {
                app.draw(canvas);
                let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
                // Negative height: rows from the top, as the canvas keeps them.
                let (width, height) = (canvas.width as i32, canvas.height as i32);
                info.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: width, biHeight: -height, biPlanes: 1, biBitCount: 32, ..unsafe { std::mem::zeroed() } };
                // SAFETY: the pixel buffer is exactly width × height 32-bit values, as the header says.
                unsafe { SetDIBitsToDevice(device, 0, 0, width as u32, height as u32, 0, 0, 0, height as u32, canvas.pixels.as_ptr().cast(), &info, DIB_RGB_COLORS) };
            });
            unsafe { EndPaint(window, &paint) };
            return 0;
        }
        WM_MOUSEMOVE => {
            let (x, y) = point();
            with(|app, _| app.pointer(x, y));
            redraw();
        }
        WM_LBUTTONUP => {
            let (x, y) = point();
            with(|app, _| app.click(x, y));
            redraw();
        }
        WM_CHAR => {
            let key = match wparam as u32 {
                8 => Some(Key::Backspace),
                13 => Some(Key::Enter),
                27 => Some(Key::Escape),
                other => char::from_u32(other).filter(|character| !character.is_control()).map(Key::Character),
            };
            if let Some(key) = key {
                with(|app, _| app.key(key));
                redraw();
            }
        }
        WM_TIMER => {
            if with(|app, _| app.changed()) == Some(true) {
                redraw();
            }
        }
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        _ => {}
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

pub fn run(work: std::sync::Arc<dyn Work>) -> Result<(), String> {
    // Starting a worker needs admin rights: ask for them once, by starting again through the system's own prompt.
    if !mc_agent::elevated() {
        let program = std::env::current_exe().map_err(|error| error.to_string())?;
        // SAFETY: both strings are zero-ended and outlive the call.
        let started = unsafe { ShellExecuteW(null_mut(), wide("runas").as_ptr(), wide(&program.to_string_lossy()).as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
        // Over 32 means it started; otherwise the prompt was refused, and the window opens without the rights.
        if started as usize > 32 {
            return Ok(());
        }
    }
    // SAFETY: plain Win32 calls with values that outlive them; the window's messages are handled on this thread.
    unsafe {
        // The program is a console program, so its command lines print; the window has no use for the console.
        FreeConsole();
        // Real pixels, not ones Windows stretches: the screens are drawn at the display's own zoom.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let app = App::new(work, GetDpiForSystem() as f32 / 96.0);
        let (width, height) = app.size();
        let instance = GetModuleHandleW(null_mut());
        // Windows makes an icon straight from a PNG's bytes.
        let png = include_bytes!("../assets/icon.png");
        let icon = CreateIconFromResourceEx(png.as_ptr(), png.len() as u32, 1, 0x0003_0000, 0, 0, 0);
        let class = wide("MasterControlWindow");
        let mut description: WNDCLASSW = std::mem::zeroed();
        description.lpfnWndProc = Some(procedure);
        description.hInstance = instance;
        description.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        description.hIcon = icon;
        description.lpszClassName = class.as_ptr();
        RegisterClassW(&description);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        // The size asked for is the picture's; the frame and title bar come on top of it.
        let mut frame = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
        AdjustWindowRect(&mut frame, style, 0);
        STATE.with(|state| *state.borrow_mut() = Some((app, Canvas::new(width, height))));
        let window = CreateWindowExW(0, class.as_ptr(), wide("Master Control").as_ptr(), style | WS_VISIBLE, CW_USEDEFAULT, CW_USEDEFAULT, frame.right - frame.left, frame.bottom - frame.top, null_mut(), null_mut(), instance, null_mut());
        if window.is_null() {
            return Err(format!("the window couldn't be made: {}", std::io::Error::last_os_error()));
        }
        SetTimer(window, 1, 150, None);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

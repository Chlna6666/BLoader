use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use arcui_core::Rect;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, ClientToScreen, GetStockObject, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GWL_STYLE, GetClientRect, GetParent,
    GetWindowLongW, HTCLIENT, HWND_TOP, HWND_TOPMOST, IDC_ARROW, IsWindowVisible, LWA_ALPHA,
    LoadCursorW, MA_ACTIVATE, MSG, PM_REMOVE, PeekMessageW, PostQuitMessage, RegisterClassExW,
    SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetCursor, SetForegroundWindow,
    SetLayeredWindowAttributes, SetParent, SetWindowLongW, SetWindowPos, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_DESTROY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SETCURSOR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDBLCLK, WM_XBUTTONDOWN, WM_XBUTTONUP,
    WNDCLASSEXW, WS_CHILD, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{BOOL, w};

static INSTALLED: AtomicBool = AtomicBool::new(false);
static OVERLAY_HWND_RAW: AtomicIsize = AtomicIsize::new(0);
static TARGET_HWND_RAW: AtomicIsize = AtomicIsize::new(0);
static CAPTURE_ENABLED: AtomicBool = AtomicBool::new(false);
static CAPTURE_REGION: OnceLock<Mutex<Option<Rect>>> = OnceLock::new();

#[link(name = "user32")]
unsafe extern "system" {
    fn SetCapture(hWnd: HWND) -> HWND;
    fn ReleaseCapture() -> BOOL;
    fn AttachThreadInput(idAttach: u32, idAttachTo: u32, fAttach: BOOL) -> BOOL;
}

pub fn install() -> Result<(), String> {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    thread::Builder::new()
        .name("arcui-input-capture".to_string())
        .spawn(capture_thread)
        .map_err(|e| e.to_string())?;

    Ok(())
}

pub fn set_capture_target(target: Option<HWND>, enabled: bool, region: Option<Rect>) {
    TARGET_HWND_RAW.store(
        target.map(|hwnd| hwnd.0 as isize).unwrap_or_default(),
        Ordering::Release,
    );
    CAPTURE_ENABLED.store(enabled, Ordering::Release);
    let mut guard = capture_region().lock().unwrap_or_else(|e| e.into_inner());
    *guard = region.filter(|rect| rect.width() > 1.0 && rect.height() > 1.0);
}

fn capture_thread() {
    let hwnd = match create_capture_window() {
        Ok(hwnd) => hwnd,
        Err(_) => return,
    };
    OVERLAY_HWND_RAW.store(hwnd.0 as isize, Ordering::SeqCst);

    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
    }

    loop {
        pump_messages();
        sync_capture_window(hwnd);
        thread::sleep(Duration::from_millis(8));
    }
}

fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn sync_capture_window(hwnd: HWND) {
    if !CAPTURE_ENABLED.load(Ordering::Acquire) {
        unsafe {
            let _ = ReleaseCapture();
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_HIDEWINDOW,
            );
            if let Some(target) = target_hwnd() {
                force_foreground(target);
            }
        }
        return;
    }

    let target = target_hwnd();
    let Some(target) = target else {
        return;
    };

    // Set parent dynamically to hook onto the game window
    unsafe {
        if GetParent(hwnd).unwrap_or_default() != target {
            let _ = SetParent(hwnd, Some(target));
            // Change style to WS_CHILD
            let mut style = GetWindowLongW(hwnd, GWL_STYLE);
            style = (style & !WS_POPUP.0 as i32) | WS_CHILD.0 as i32;
            let _ = SetWindowLongW(hwnd, GWL_STYLE, style);
        }
    }

    let capture_region = {
        let guard = capture_region().lock().unwrap_or_else(|e| e.into_inner());
        *guard
    };
    let Some(_) = capture_region else {
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_HIDEWINDOW,
            );
        }
        return;
    };

    let (width, height) = unsafe {
        let mut rect = RECT::default();
        if GetClientRect(target, &mut rect).is_ok() {
            (rect.right - rect.left, rect.bottom - rect.top)
        } else {
            (0, 0)
        }
    };
    if width <= 1 || height <= 1 {
        return;
    }

    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOP), 0, 0, width, height, SWP_SHOWWINDOW);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
    }
}

fn create_capture_window() -> Result<HWND, String> {
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_SETCURSOR => {
                let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
                let _ = unsafe { SetCursor(Some(cursor)) };
                LRESULT(1)
            }
            WM_NCHITTEST => LRESULT(HTCLIENT as isize),
            WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN => {
                let _ = unsafe { SetCapture(hwnd) };
                crate::dx12::dispatch_platform_event(hwnd, msg, wparam, lparam);
                LRESULT(0)
            }
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP => {
                let _ = unsafe { ReleaseCapture() };
                crate::dx12::dispatch_platform_event(hwnd, msg, wparam, lparam);
                LRESULT(0)
            }
            WM_MOUSEMOVE | WM_MOUSEWHEEL | WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK
            | WM_MBUTTONDBLCLK | WM_XBUTTONDBLCLK | WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN
            | WM_SYSKEYUP | WM_CHAR => {
                crate::dx12::dispatch_platform_event(hwnd, msg, wparam, lparam);
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    let class_name = w!("ARCUI_CAPTURE_WINDOW");
    let hinstance = unsafe { GetModuleHandleW(None).map_err(|e| format!("{e:?}"))? };
    let wndclass = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance.into(),
        lpszClassName: class_name,
        hbrBackground: HBRUSH(unsafe { GetStockObject(BLACK_BRUSH).0 }),
        ..Default::default()
    };

    unsafe {
        let _ = RegisterClassExW(&wndclass);
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0),
            class_name,
            w!("arcui-capture"),
            WINDOW_STYLE(WS_POPUP.0),
            0,
            0,
            1,
            1,
            None,
            None,
            Some(wndclass.hInstance),
            None,
        )
        .map_err(|e| format!("{e:?}"))
    }
}

pub fn overlay_hwnd() -> isize {
    OVERLAY_HWND_RAW.load(Ordering::Acquire)
}

pub fn target_hwnd() -> Option<HWND> {
    let raw = TARGET_HWND_RAW.load(Ordering::Acquire);
    if raw == 0 {
        None
    } else {
        Some(HWND(raw as *mut _))
    }
}

fn query_panel_anchor(hwnd: HWND, capture_region: Rect) -> Option<(i32, i32, i32, i32)> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }

        let mut rect = RECT::default();
        if GetClientRect(hwnd, &mut rect).is_err() {
            return None;
        }

        let client_width = rect.right - rect.left;
        let client_height = rect.bottom - rect.top;
        if client_width <= 1 || client_height <= 1 {
            return None;
        }

        let mut origin = POINT { x: 0, y: 0 };
        if !ClientToScreen(hwnd, &mut origin).as_bool() {
            return None;
        }

        let left = capture_region
            .min
            .x
            .floor()
            .max(0.0)
            .min(client_width as f32) as i32;
        let top = capture_region
            .min
            .y
            .floor()
            .max(0.0)
            .min(client_height as f32) as i32;
        let right = capture_region
            .max
            .x
            .ceil()
            .max(left as f32 + 1.0)
            .min(client_width as f32) as i32;
        let bottom = capture_region
            .max
            .y
            .ceil()
            .max(top as f32 + 1.0)
            .min(client_height as f32) as i32;
        let width = right - left;
        let height = bottom - top;
        if width <= 1 || height <= 1 {
            return None;
        }

        Some((left, top, width, height))
    }
}

fn capture_region() -> &'static Mutex<Option<Rect>> {
    CAPTURE_REGION.get_or_init(|| Mutex::new(None))
}

fn force_foreground(hwnd: HWND) {
    unsafe {
        let foreground = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if foreground == hwnd {
            return;
        }
        let foreground_thread =
            windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(foreground, None);
        let our_thread = windows::Win32::System::Threading::GetCurrentThreadId();

        if foreground_thread != our_thread {
            AttachThreadInput(foreground_thread, our_thread, BOOL(1));
            windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
            windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
            AttachThreadInput(foreground_thread, our_thread, BOOL(0));
        } else {
            windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
            windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
        }
    }
}

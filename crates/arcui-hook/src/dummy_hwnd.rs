use std::mem;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, RegisterClassExW, WNDCLASSEXW,
    WS_EX_OVERLAPPEDWINDOW, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

pub(crate) fn create() -> Result<HWND, String> {
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    let wndclass = WNDCLASSEXW {
        cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: unsafe { GetModuleHandleW(None).map_err(|e| format!("{e:?}"))?.into() },
        lpszClassName: w!("ARCUI_DX_DUMMY"),
        ..Default::default()
    };

    unsafe {
        let _ = RegisterClassExW(&wndclass);
        CreateWindowExW(
            WS_EX_OVERLAPPEDWINDOW,
            wndclass.lpszClassName,
            w!("ARCUI_DX_DUMMY"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            100,
            100,
            None,
            None,
            Some(wndclass.hInstance),
            None,
        )
        .map_err(|e| format!("{e:?}"))
    }
}

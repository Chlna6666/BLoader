use std::mem;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassExW, CS_HREDRAW, CS_VREDRAW, WNDCLASSEXW,
    WS_EX_OVERLAPPEDWINDOW, WS_OVERLAPPEDWINDOW,
};

pub fn dummy_hwnd() -> HWND {
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    let wndclass = WNDCLASSEXW {
        cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: unsafe { GetModuleHandleW(None).unwrap().into() },
        lpszClassName: w!("BLOADER_RENDER_SIGNAL_DUMMY"),
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassExW(&wndclass);
        CreateWindowExW(
            WS_EX_OVERLAPPEDWINDOW,
            wndclass.lpszClassName,
            w!("BLOADER_RENDER_SIGNAL_DUMMY"),
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
        .expect("CreateWindowExW dummy")
    }
}

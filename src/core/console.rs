// src/core/console.rs
#![allow(dead_code)]
#![allow(non_snake_case)]

use std::ffi::{CString, c_void};
use std::ptr::null_mut;
use std::thread;
use std::time::Duration;

use crate::runtime::foundation::logging;
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AllocConsole, CONSOLE_MODE, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT,
    ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WRAP_AT_EOL_OUTPUT, GetConsoleWindow,
    STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode, SetConsoleTitleW,
    SetStdHandle,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::{SW_SHOW, ShowWindow};

pub unsafe fn init_console() {
    // 1. 检查是否由 CreateProcess (CREATE_NEW_CONSOLE) 创建了窗口
    let mut window = GetConsoleWindow();
    let mut is_existing = false;

    if window.0 != null_mut() {
        // 存在窗口 (可能是 Terminal 也可能是 Legacy)，直接使用
        let _ = ShowWindow(window, SW_SHOW);
        is_existing = true;
    } else {
        // 2. 如果没有窗口 (说明 Launcher 设置 enable_console=false 或启动失败)
        // 手动申请一个 (AllocConsole 通常只会产生 Legacy 窗口)
        let _ = AllocConsole();
        window = GetConsoleWindow();
    }

    // 设置标题
    let title = windows::core::w!("Minecraft Debug Console");
    let _ = SetConsoleTitleW(title);

    if window.0 != null_mut() {
        // 移除关闭按钮 (防止误关导致游戏退出)
        let menu = windows::Win32::UI::WindowsAndMessaging::GetSystemMenu(window, false);
        if !menu.is_invalid() {
            let _ = windows::Win32::UI::WindowsAndMessaging::DeleteMenu(
                menu,
                windows::Win32::UI::WindowsAndMessaging::SC_CLOSE,
                windows::Win32::UI::WindowsAndMessaging::MF_BYCOMMAND,
            );
        }
        let _ = ShowWindow(window, SW_SHOW);
    }

    // 重定向 IO 句柄 (必须执行，否则 Rust 的 println! 可能无法输出)
    let h_conout = CreateFileW(
        windows::core::w!("CONOUT$"),
        GENERIC_READ.0 | GENERIC_WRITE.0,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        None,
    )
    .unwrap_or_default();

    let h_conin = CreateFileW(
        windows::core::w!("CONIN$"),
        GENERIC_READ.0 | GENERIC_WRITE.0,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        None,
    )
    .unwrap_or_default();

    if !h_conout.is_invalid() {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, h_conout);
        let _ = SetStdHandle(STD_ERROR_HANDLE, h_conout);
        logging::set_console_handle(h_conout);

        // 启用 ANSI 颜色
        let mut mode = CONSOLE_MODE(0);
        if windows::Win32::System::Console::GetConsoleMode(h_conout, &mut mode).is_ok() {
            let new_mode = mode
                | ENABLE_PROCESSED_OUTPUT
                | ENABLE_WRAP_AT_EOL_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
            let _ = SetConsoleMode(h_conout, new_mode);
        }
    }

    if !h_conin.is_invalid() {
        let _ = SetStdHandle(STD_INPUT_HANDLE, h_conin);
        // 启用 QuickEdit
        let mut mode = CONSOLE_MODE(0);
        if windows::Win32::System::Console::GetConsoleMode(h_conin, &mut mode).is_ok() {
            let new_mode = (mode
                | ENABLE_EXTENDED_FLAGS
                | ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_PROCESSED_INPUT)
                | ENABLE_QUICK_EDIT_MODE;
            let _ = SetConsoleMode(h_conin, new_mode);
        }
    }

    // 将进程 stdout/stderr 重定向到 BLoader 的持久捕获文件。
    // 控制台渲染由 logging 直接写入 CONOUT$，因此捕获线程回放日志时不会递归。
    if !h_conout.is_invalid() {
        crate::runtime::foundation::native_stdio::install_process_capture();
        crate::runtime::foundation::native_stdio::flush_pending();
    }

    if is_existing {
        logging::info_message("Attached to existing console.");
    } else {
        logging::warn_message("Allocated new console (Legacy mode).");
    }
}

// ... fix_crt_streams 和 start_input_listener 与之前一致 ...
unsafe fn fix_crt_streams(h_out: HANDLE, h_in: HANDLE) {
    let mut crt_handle = GetModuleHandleW(windows::core::w!("ucrtbase.dll"));
    if crt_handle.is_err() || crt_handle.clone().unwrap().is_invalid() {
        crt_handle = GetModuleHandleW(windows::core::w!("msvcrt.dll"));
    }
    if let Ok(h_crt) = crt_handle {
        if h_crt.is_invalid() {
            return;
        }
        let func_open = GetProcAddress(h_crt, windows::core::s!("_open_osfhandle"));
        let func_dup2 = GetProcAddress(h_crt, windows::core::s!("_dup2"));
        let func_freopen_s = GetProcAddress(h_crt, windows::core::s!("freopen_s"));
        let func_iob = GetProcAddress(h_crt, windows::core::s!("__acrt_iob_func"));
        let func_setvbuf = GetProcAddress(h_crt, windows::core::s!("setvbuf"));
        if let (
            Some(open_ptr),
            Some(dup2_ptr),
            Some(freopen_ptr),
            Some(iob_ptr),
            Some(setvbuf_ptr),
        ) = (func_open, func_dup2, func_freopen_s, func_iob, func_setvbuf)
        {
            type OpenFn = unsafe extern "C" fn(windows::Win32::Foundation::HANDLE, i32) -> i32;
            type Dup2Fn = unsafe extern "C" fn(i32, i32) -> i32;
            type FreopenSFn =
                unsafe extern "C" fn(*mut *mut c_void, *const i8, *const i8, *mut c_void) -> i32;
            type GetFileFn = unsafe extern "C" fn(u32) -> *mut c_void;
            type SetVBufFn = unsafe extern "C" fn(*mut c_void, *mut i8, i32, usize) -> i32;
            let open_osfhandle: OpenFn = std::mem::transmute(open_ptr);
            let dup2: Dup2Fn = std::mem::transmute(dup2_ptr);
            let reopen: FreopenSFn = std::mem::transmute(freopen_ptr);
            let get_file: GetFileFn = std::mem::transmute(iob_ptr);
            let setvbuf: SetVBufFn = std::mem::transmute(setvbuf_ptr);
            const _O_TEXT: i32 = 0x4000;
            const STDIN_FILENO: i32 = 0;
            const STDOUT_FILENO: i32 = 1;
            const STDERR_FILENO: i32 = 2;
            const _IONBF: i32 = 0x0004;
            let fd_out = open_osfhandle(h_out, _O_TEXT);
            if fd_out != -1 {
                dup2(fd_out, STDOUT_FILENO);
                dup2(fd_out, STDERR_FILENO);
            }
            let fd_in = open_osfhandle(h_in, _O_TEXT);
            if fd_in != -1 {
                dup2(fd_in, STDIN_FILENO);
            }
            let path_in = CString::new("CONIN$").unwrap();
            let path_out = CString::new("CONOUT$").unwrap();
            let mode_r = CString::new("r").unwrap();
            let mode_w = CString::new("w").unwrap();
            let mut dummy: *mut c_void = std::ptr::null_mut();
            let stdin_ptr = get_file(0);
            let stdout_ptr = get_file(1);
            let stderr_ptr = get_file(2);
            if !stdin_ptr.is_null() {
                reopen(&mut dummy, path_in.as_ptr(), mode_r.as_ptr(), stdin_ptr);
                setvbuf(stdin_ptr, null_mut(), _IONBF, 0);
            }
            if !stdout_ptr.is_null() {
                reopen(&mut dummy, path_out.as_ptr(), mode_w.as_ptr(), stdout_ptr);
                setvbuf(stdout_ptr, null_mut(), _IONBF, 0);
            }
            if !stderr_ptr.is_null() {
                reopen(&mut dummy, path_out.as_ptr(), mode_w.as_ptr(), stderr_ptr);
                setvbuf(stderr_ptr, null_mut(), _IONBF, 0);
            }
        }
    }
}

pub fn start_input_listener() {
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(100));
    });
}

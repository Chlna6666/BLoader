// src/core/console.rs
#![allow(dead_code)]
#![allow(non_snake_case)]

use std::ptr::null_mut;
use std::thread;
use std::time::Duration;

use crate::runtime::foundation::{build_info, file_io_policy, logging};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AllocConsole, CONSOLE_MODE, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT,
    ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WRAP_AT_EOL_OUTPUT, GetConsoleMode,
    GetConsoleWindow, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    SetConsoleTitleW, SetStdHandle,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DeleteMenu, GetSystemMenu, MF_BYCOMMAND, SC_CLOSE, SW_SHOW, ShowWindow,
};
use windows::core::PCWSTR;

pub unsafe fn init_console() {
    let mut window = GetConsoleWindow();
    let is_existing = window.0 != null_mut();

    if is_existing {
        let _ = ShowWindow(window, SW_SHOW);
    } else {
        let _ = AllocConsole();
        window = GetConsoleWindow();
    }

    set_runtime_console_title();

    if window.0 != null_mut() {
        // Closing the console window can terminate a console-attached Minecraft
        // process. Keep the runtime console intentionally non-destructive.
        let menu = GetSystemMenu(window, false);
        if !menu.is_invalid() {
            let _ = DeleteMenu(menu, SC_CLOSE, MF_BYCOMMAND);
        }
        let _ = ShowWindow(window, SW_SHOW);
    }

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

        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(h_conout, &mut mode).is_ok() {
            let new_mode = CONSOLE_MODE(
                mode.0
                    | ENABLE_PROCESSED_OUTPUT.0
                    | ENABLE_WRAP_AT_EOL_OUTPUT.0
                    | ENABLE_VIRTUAL_TERMINAL_PROCESSING.0,
            );
            let _ = SetConsoleMode(h_conout, new_mode);
        }

        // set_console_handle emits the structured BLoader/Minecraft banner after
        // ANSI capability is known.
        logging::set_console_handle(h_conout);
    }

    if !h_conin.is_invalid() {
        let _ = SetStdHandle(STD_INPUT_HANDLE, h_conin);
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(h_conin, &mut mode).is_ok() {
            // QuickEdit pauses a console process while the user is selecting text.
            // That behavior is unacceptable for a real-time game/mod runtime.
            let new_mode = CONSOLE_MODE(
                (mode.0
                    | ENABLE_EXTENDED_FLAGS.0
                    | ENABLE_LINE_INPUT.0
                    | ENABLE_ECHO_INPUT.0
                    | ENABLE_PROCESSED_INPUT.0)
                    & !ENABLE_QUICK_EDIT_MODE.0,
            );
            let _ = SetConsoleMode(h_conin, new_mode);
        }
    }

    if !h_conout.is_invalid() {
        crate::runtime::foundation::native_stdio::install_process_capture();
        crate::runtime::foundation::native_stdio::flush_pending();
    }

    logging::scoped_info_message(
        "console",
        &format!(
            "runtime console ready | source={} | quick_edit=false | ansi=true-if-supported | layout=structured-v2 | file_io={}",
            if is_existing { "existing" } else { "allocated" },
            file_io_policy::mode_label(),
        ),
    );
}

fn set_runtime_console_title() {
    let host_version = file_io_policy::host_version().unwrap_or("unknown");
    let title = format!(
        "BLoader {} | Minecraft {} | Runtime Console",
        build_info::VERSION,
        host_version
    );
    let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _ = SetConsoleTitleW(PCWSTR(wide.as_ptr()));
    }
}

pub fn start_input_listener() {
    let _ = thread::Builder::new()
        .name("bloader-console-input".to_string())
        .spawn(|| {
            // Reserved for interactive diagnostics. Keeping the thread named makes
            // crash/log attribution explicit without adding a blocking stdin loop.
            thread::sleep(Duration::from_millis(100));
        });
}

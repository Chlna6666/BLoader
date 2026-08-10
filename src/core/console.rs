// src/core/console.rs
#![allow(dead_code)]
#![allow(non_snake_case)]

use std::ptr::null_mut;
use std::thread;
use std::time::Duration;

use crate::runtime::foundation::{build_info, console_branding, file_io_policy, i18n, logging};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, WriteFile,
};
use windows::Win32::System::Console::{
    AllocConsole, CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS,
    ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WRAP_AT_EOL_OUTPUT, GetConsoleMode,
    GetConsoleScreenBufferInfo, GetConsoleWindow, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetConsoleMode, SetConsoleTitleW, SetStdHandle,
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

        // set_console_handle may flush bootstrap diagnostics, then the branding
        // renderer clears the viewport and establishes the final console surface.
        logging::set_console_handle(h_conout);
        render_adaptive_branding(h_conout);
        emit_runtime_identity();
        replay_pre_main_load_state();
        crate::core::xuser_bridge::publish_pending_logs();
        schedule_mod_inventory_replay();
    }

    if !h_conin.is_invalid() {
        let _ = SetStdHandle(STD_INPUT_HANDLE, h_conin);
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(h_conin, &mut mode).is_ok() {
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

    logging::scoped_debug_message(
        "console",
        &format!(
            "runtime console ready | source={} | quick_edit=false | layout=adaptive-java-server | columns={} | file_io={}",
            if is_existing { "existing" } else { "allocated" },
            visible_columns(h_conout),
            file_io_policy::mode_label(),
        ),
    );
}

fn render_adaptive_branding(handle: HANDLE) {
    let ansi = console_has_vt(handle);
    let columns = visible_columns(handle);

    if ansi {
        write_console(handle, "\x1b[0m\x1b[2J\x1b[H");
    }

    for line in console_branding::render_banner(columns, ansi) {
        write_console(handle, &line);
        write_console(handle, "\r\n");
    }
}

fn emit_runtime_identity() {
    let host_version = file_io_policy::host_version().unwrap_or("unknown");
    let debug_destination = if file_io_policy::writes_allowed() {
        "logs/latest.log"
    } else {
        "OutputDebugString"
    };

    logging::scoped_info_message(
        "loader",
        &format!("{}: {}", i18n::tr("console.info.version"), build_info::VERSION),
    );
    logging::scoped_info_message(
        "loader",
        &format!("{}: {}", i18n::tr("console.info.license"), build_info::LICENSE),
    );
    logging::scoped_info_message(
        "loader",
        &format!("{}: {}", i18n::tr("console.info.repository"), build_info::REPOSITORY),
    );
    logging::scoped_info_message(
        "minecraft",
        &format!("{}: {host_version}", i18n::tr("console.info.version")),
    );
    logging::scoped_info_message(
        "loader",
        &format!(
            "{}: {} | {}: {} | {}: {}",
            i18n::tr("console.info.locale"),
            i18n::current_locale(),
            i18n::tr("console.banner.file_io"),
            file_io_policy::mode_label(),
            i18n::tr("console.banner.full_debug"),
            debug_destination,
        ),
    );
}

fn replay_pre_main_load_state() {
    let mods = crate::runtime::foundation::mod_diagnostics::all_mods();
    let preload_count = mods.iter().filter(|m| m.kind == "preload").count();
    if preload_count > 0 {
        logging::scoped_info_message(
            "mod:PreLoader",
            &i18n::tr("console.preloader.detected")
                .replace("{count}", &preload_count.to_string()),
        );
        let loaded = mods
            .iter()
            .filter(|m| m.kind == "preload" && m.state == "loaded")
            .count();
        if loaded > 0 {
            logging::scoped_info_message(
                "mod:PreLoader",
                &i18n::tr("console.preloader.active")
                    .replace("{count}", &loaded.to_string()),
            );
        }
    } else {
        logging::scoped_info_message("mod:PreLoader", &i18n::tr("console.preloader.none"));
    }

    logging::scoped_info_message(
        "mod:Proxy",
        &i18n::tr("console.proxy.route"),
    );
}

fn schedule_mod_inventory_replay() {
    let _ = thread::Builder::new()
        .name("bloader-console-mod-inventory".to_string())
        .spawn(|| {
            // Console creation is intentionally delayed after OEP on the normal
            // path. Replay the authoritative registry after regular Mod loading
            // has had time to complete, so pre-console lifecycle events are visible.
            thread::sleep(Duration::from_millis(2_000));
            let mut mods = crate::runtime::foundation::mod_diagnostics::all_mods();
            mods.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));

            for identity in &mods {
                let scope = format!("mod:{}", identity.name);
                let version = identity.version.as_deref().unwrap_or("unknown");
                match identity.state.as_str() {
                    "loaded" => logging::scoped_info_message(
                        &scope,
                        &format!("Loaded {version} ({})", identity.kind),
                    ),
                    "failed" => logging::scoped_error_message(
                        &scope,
                        &i18n::tr("console.mod.failed"),
                    ),
                    "crashed" => logging::scoped_error_message(
                        &scope,
                        &i18n::tr("console.mod.crashed"),
                    ),
                    "loading" => logging::scoped_info_message(
                        &scope,
                        &i18n::tr("console.mod.loading"),
                    ),
                    _ => logging::scoped_info_message(
                        &scope,
                        &format!("{} {version} ({})", i18n::tr("console.mod.discovered"), identity.kind),
                    ),
                }
            }

            let discovered = mods.len();
            let loaded = mods.iter().filter(|m| m.state == "loaded").count();
            let failed = mods
                .iter()
                .filter(|m| matches!(m.state.as_str(), "failed" | "crashed"))
                .count();
            let text = i18n::tr("console.mods.summary")
                .replace("{discovered}", &discovered.to_string())
                .replace("{loaded}", &loaded.to_string())
                .replace("{failed}", &failed.to_string());
            logging::info_message(&text);
        });
}

fn visible_columns(handle: HANDLE) -> usize {
    if handle.is_invalid() {
        return 80;
    }
    unsafe {
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        if GetConsoleScreenBufferInfo(handle, &mut info).is_ok() {
            let width = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
            if width > 0 {
                return width as usize;
            }
        }
    }
    80
}

fn console_has_vt(handle: HANDLE) -> bool {
    unsafe {
        let mut mode = CONSOLE_MODE(0);
        GetConsoleMode(handle, &mut mode).is_ok()
            && (mode.0 & ENABLE_VIRTUAL_TERMINAL_PROCESSING.0) != 0
    }
}

fn write_console(handle: HANDLE, text: &str) {
    if handle.is_invalid() || text.is_empty() {
        return;
    }
    unsafe {
        let mut written = 0;
        let _ = WriteFile(handle, Some(text.as_bytes()), Some(&mut written), None);
    }
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
            thread::sleep(Duration::from_millis(100));
        });
}

// src/core/console.rs
#![allow(dead_code)]
#![allow(non_snake_case)]

use std::fs;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
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

const CLASSIC_SCROLLBACK_LINES: i16 = 12_000;
static CONSOLE_WINDOW_READY: AtomicBool = AtomicBool::new(false);
static CONSOLE_LOGGING_READY: AtomicBool = AtomicBool::new(false);
static CONSOLE_STDIO_READY: AtomicBool = AtomicBool::new(false);

#[repr(C)]
#[derive(Clone, Copy)]
struct RawCoord {
    x: i16,
    y: i16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "SetConsoleScreenBufferSize"]
    fn set_console_screen_buffer_size_raw(handle: *mut core::ffi::c_void, size: RawCoord) -> i32;
}

/// Lightweight pre-main probe. It only reads the existing JSON and never
/// starts the config watcher, rewrites the config, or initializes logging.
pub fn early_console_requested() -> bool {
    let loader_cfg = crate::utils::get_loader_directory().join("config.json");
    let exe_cfg = crate::utils::get_exe_directory().join("config.json");
    let path = if loader_cfg.exists() { loader_cfg } else { exe_cfg };
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|value| value.get("enable_debug_console").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

pub fn window_ready() -> bool {
    CONSOLE_WINDOW_READY.load(Ordering::Acquire)
}

/// Creates the one and only visible console window before the slow PreLoader
/// chain begins. This intentionally does NOT call SetStdHandle, touch stdin,
/// install CRT capture, or mutate Minecraft process stdio state while the
/// premain trampoline still owns the startup thread context.
pub unsafe fn init_console_window_early() {
    if CONSOLE_WINDOW_READY.swap(true, Ordering::AcqRel) {
        return;
    }

    let mut window = GetConsoleWindow();
    let reused = window.0 != null_mut();
    if !reused {
        if AllocConsole().is_err() {
            CONSOLE_WINDOW_READY.store(false, Ordering::Release);
            logging::write_bootstrap_marker("console.early.alloc.failed");
            return;
        }
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

    let h_conout = open_conout();
    if !h_conout.is_invalid() {
        configure_output(h_conout);
        configure_classic_scrollback(h_conout);
        render_branding(
            h_conout,
            visible_columns(h_conout),
            console_has_vt(h_conout),
            true,
        );
    }

    logging::write_bootstrap_marker(&format!(
        "console.early.window.ready source={} process_stdio=false crt_capture=false",
        if reused { "existing-associated" } else { "allocated-once" }
    ));
}

/// Connects BLoader's own structured logging sink to the already-created
/// console. Safe to call before Minecraft OEP because it does not change the
/// process-wide standard handles.
pub unsafe fn activate_console_logging() {
    if CONSOLE_LOGGING_READY.swap(true, Ordering::AcqRel) {
        return;
    }
    if !window_ready() {
        CONSOLE_LOGGING_READY.store(false, Ordering::Release);
        return;
    }

    let h_conout = open_conout();
    if h_conout.is_invalid() {
        CONSOLE_LOGGING_READY.store(false, Ordering::Release);
        return;
    }

    configure_output(h_conout);
    emit_runtime_identity();
    logging::set_console_handle(h_conout);
    logging::scoped_debug_message(
        "console",
        &format!(
            "runtime console logging ready | backend=classic-early-single-window | columns={} | file_io={} | process_stdio=false",
            visible_columns(h_conout),
            file_io_policy::mode_label(),
        ),
    );
}

/// Binds Minecraft's process standard handles only after OEP has been released.
/// The console window already exists, so this operation never creates another
/// window and never calls FreeConsole/AllocConsole.
pub unsafe fn bind_process_stdio() {
    if CONSOLE_STDIO_READY.swap(true, Ordering::AcqRel) {
        return;
    }
    if !window_ready() {
        CONSOLE_STDIO_READY.store(false, Ordering::Release);
        return;
    }

    let h_conout = open_conout();
    let h_conin = open_conin();

    if !h_conout.is_invalid() {
        configure_output(h_conout);
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, h_conout);
        let _ = SetStdHandle(STD_ERROR_HANDLE, h_conout);
    }

    if !h_conin.is_invalid() {
        let _ = SetStdHandle(STD_INPUT_HANDLE, h_conin);
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(h_conin, &mut mode).is_ok() {
            let _ = SetConsoleMode(
                h_conin,
                CONSOLE_MODE(
                    mode.0
                        | ENABLE_EXTENDED_FLAGS.0
                        | ENABLE_LINE_INPUT.0
                        | ENABLE_ECHO_INPUT.0
                        | ENABLE_PROCESSED_INPUT.0
                        | ENABLE_QUICK_EDIT_MODE.0,
                ),
            );
        }
    }

    logging::scoped_debug_message(
        "console",
        "runtime console stdio bound | phase=post-oep | allocation=none | realloc=false",
    );
}

/// Full initialization path used by late-attach scenarios where Minecraft OEP
/// is already running and there is no premain context to protect.
pub unsafe fn init_console() {
    init_console_window_early();
    activate_console_logging();
    bind_process_stdio();
}

fn open_conout() -> HANDLE {
    unsafe {
        CreateFileW(
            windows::core::w!("CONOUT$"),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .unwrap_or_default()
    }
}

fn open_conin() -> HANDLE {
    unsafe {
        CreateFileW(
            windows::core::w!("CONIN$"),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .unwrap_or_default()
    }
}

fn configure_output(handle: HANDLE) {
    if handle.is_invalid() {
        return;
    }
    unsafe {
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(handle, &mut mode).is_ok() {
            let _ = SetConsoleMode(
                handle,
                CONSOLE_MODE(
                    mode.0
                        | ENABLE_PROCESSED_OUTPUT.0
                        | ENABLE_WRAP_AT_EOL_OUTPUT.0
                        | ENABLE_VIRTUAL_TERMINAL_PROCESSING.0,
                ),
            );
        }
    }
}

fn configure_classic_scrollback(handle: HANDLE) {
    if handle.is_invalid() {
        return;
    }
    unsafe {
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        if GetConsoleScreenBufferInfo(handle, &mut info).is_ok() {
            let window_width = info.srWindow.Right.saturating_sub(info.srWindow.Left) + 1;
            let width = info.dwSize.X.max(window_width).max(1);
            let height = info.dwSize.Y.max(CLASSIC_SCROLLBACK_LINES);
            let _ = set_console_screen_buffer_size_raw(handle.0, RawCoord { x: width, y: height });
        }
    }
}

fn render_branding(handle: HANDLE, columns: usize, ansi: bool, clear: bool) {
    if clear && ansi {
        let _ = write_all_handle(handle, b"\x1b[0m\x1b[2J\x1b[H");
    }
    for line in console_branding::render_banner(columns, ansi) {
        let _ = write_all_handle(handle, line.as_bytes());
        let _ = write_all_handle(handle, b"\r\n");
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
        "game-stdio",
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

    logging::scoped_info_message("mod:Proxy", &i18n::tr("console.proxy.route"));
}

fn replay_mod_inventory() {
    let mut mods = crate::runtime::foundation::mod_diagnostics::all_mods();
    mods.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));

    for identity in &mods {
        let scope = format!("mod:{}", identity.name);
        let version = identity.version.as_deref().unwrap_or("unknown");
        logging::scoped_info_message(
            &scope,
            &format!(
                "{} {version} ({})",
                i18n::tr("console.mod.discovered"),
                identity.kind
            ),
        );
        match identity.state.as_str() {
            "loaded" => logging::scoped_info_message(
                &scope,
                &format!("Loaded {version} ({})", identity.kind),
            ),
            "failed" => logging::scoped_error_message(&scope, &i18n::tr("console.mod.failed")),
            "crashed" => logging::scoped_error_message(&scope, &i18n::tr("console.mod.crashed")),
            "loading" => logging::scoped_info_message(&scope, &i18n::tr("console.mod.loading")),
            _ => {}
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
}

pub fn publish_runtime_state() {
    replay_pre_main_load_state();
    replay_mod_inventory();
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

fn write_all_handle(handle: HANDLE, mut bytes: &[u8]) -> bool {
    if handle.is_invalid() {
        return false;
    }
    while !bytes.is_empty() {
        let mut written = 0u32;
        if unsafe { WriteFile(handle, Some(bytes), Some(&mut written), None) }.is_err()
            || written == 0
        {
            return false;
        }
        bytes = &bytes[written as usize..];
    }
    true
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

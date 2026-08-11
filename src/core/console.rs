// src/core/console.rs
#![allow(dead_code)]
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::process::{Command, Stdio};
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
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
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WRAP_AT_EOL_OUTPUT, FreeConsole, GetConsoleMode,
    GetConsoleScreenBufferInfo, GetConsoleWindow, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetConsoleMode, SetConsoleTitleW, SetStdHandle,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    DeleteMenu, GetSystemMenu, IsWindowVisible, MF_BYCOMMAND, SC_CLOSE, SW_SHOW, ShowWindow,
};
use windows::core::PCWSTR;

const CLASSIC_SCROLLBACK_LINES: i16 = 12_000;
const ERROR_PIPE_CONNECTED: u32 = 535;
const PIPE_ACCESS_OUTBOUND: u32 = 0x0000_0002;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
const WINDOWS_TERMINAL_HANDSHAKE_TIMEOUT_MS: u64 = 2_500;
const BACKEND_PENDING: u8 = 0;
const BACKEND_WINDOWS_TERMINAL: u8 = 1;
const BACKEND_CLASSIC: u8 = 2;
const CONSOLE_HOST_EXE: &str = "BLoaderConsoleHost.exe";

#[repr(C)]
#[derive(Clone, Copy)]
struct RawCoord {
    x: i16,
    y: i16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer_size: u32,
        in_buffer_size: u32,
        default_timeout: u32,
        security_attributes: *const c_void,
    ) -> *mut c_void;
    fn ConnectNamedPipe(pipe: *mut c_void, overlapped: *mut c_void) -> i32;
    #[link_name = "CloseHandle"]
    fn close_handle_raw(handle: *mut c_void) -> i32;
    #[link_name = "GetLastError"]
    fn get_last_error_raw() -> u32;
    #[link_name = "SetConsoleScreenBufferSize"]
    fn set_console_screen_buffer_size_raw(handle: *mut c_void, size: RawCoord) -> i32;
}

pub unsafe fn init_console() {
    let existing_window = GetConsoleWindow();
    let has_associated_console = existing_window.0 != null_mut();
    let has_visible_console = has_associated_console && IsWindowVisible(existing_window).as_bool();

    logging::write_bootstrap_marker(&format!(
        "console.visibility.probe associated={} visible={}",
        has_associated_console, has_visible_console
    ));

    if has_visible_console {
        init_classic_console(true);
        return;
    }

    if launch_windows_terminal_async() {
        return;
    }

    init_classic_console(false);
}

unsafe fn init_classic_console(is_existing_visible: bool) {
    let mut window = GetConsoleWindow();
    if is_existing_visible {
        let _ = ShowWindow(window, SW_SHOW);
    } else {
        if window.0 != null_mut() {
            let _ = FreeConsole();
        }
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
            let _ = SetConsoleMode(
                h_conout,
                CONSOLE_MODE(
                    mode.0
                        | ENABLE_PROCESSED_OUTPUT.0
                        | ENABLE_WRAP_AT_EOL_OUTPUT.0
                        | ENABLE_VIRTUAL_TERMINAL_PROCESSING.0,
                ),
            );
        }

        configure_classic_scrollback(h_conout);
        render_branding(
            h_conout,
            visible_columns(h_conout),
            console_has_vt(h_conout),
            true,
        );

        emit_runtime_identity();
        logging::set_console_handle(h_conout);
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
        &format!(
            "runtime console ready | backend=console-host-fallback | source={} | quick_edit=true | scrollback={} | columns={} | file_io={}",
            if is_existing_visible { "existing-visible" } else { "allocated-visible" },
            CLASSIC_SCROLLBACK_LINES,
            visible_columns(h_conout),
            file_io_policy::mode_label(),
        ),
    );
}

fn launch_windows_terminal_async() -> bool {
    let pid = unsafe { GetCurrentProcessId() };
    let pipe_leaf = format!("BLoader.Console.{pid}");
    let pipe_path = format!(r"\\.\pipe\{pipe_leaf}");
    let pipe_wide: Vec<u16> = pipe_path.encode_utf16().chain(Some(0)).collect();

    let raw_pipe = unsafe {
        CreateNamedPipeW(
            pipe_wide.as_ptr(),
            PIPE_ACCESS_OUTBOUND,
            PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            4 * 1024,
            0,
            std::ptr::null(),
        )
    };
    if raw_pipe.is_null() || raw_pipe as isize == -1 {
        logging::write_bootstrap_marker("console.windows_terminal.pipe.create.failed fallback=classic");
        return false;
    }

    let loader_path = crate::utils::get_module_path(crate::utils::loader_module_handle());
    let Some(loader_dir) = loader_path.parent() else {
        unsafe { close_handle_raw(raw_pipe); }
        logging::write_bootstrap_marker("console.windows_terminal.loader_dir.missing fallback=classic");
        return false;
    };

    let host_path = loader_dir.join(CONSOLE_HOST_EXE);
    if !host_path.is_file() {
        unsafe { close_handle_raw(raw_pipe); }
        logging::write_bootstrap_marker(&format!(
            "console.windows_terminal.host.missing path={} fallback=classic",
            host_path.display()
        ));
        return false;
    }

    let title = format!(
        "BLoader {} | Minecraft {} | Runtime Console",
        build_info::VERSION,
        file_io_policy::host_version().unwrap_or("unknown")
    );

    logging::write_bootstrap_marker(&format!(
        "console.windows_terminal.spawn.begin dir={} host={} pipe={} transport=named-pipe",
        loader_dir.display(), host_path.display(), pipe_leaf
    ));

    let mut wt = Command::new("wt.exe");
    wt.args(["-w", "-1", "new-tab", "--startingDirectory"]);
    wt.arg(loader_dir.as_os_str());
    wt.args(["--title", title.as_str(), "--suppressApplicationTitle"]);
    wt.arg(host_path.as_os_str());
    wt.args(["--pipe", pipe_path.as_str()]);

    if let Err(error) = wt
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        unsafe { close_handle_raw(raw_pipe); }
        logging::write_bootstrap_marker(&format!(
            "console.windows_terminal.spawn.failed error={} fallback=classic",
            error
        ));
        return false;
    }

    let backend = Arc::new(AtomicU8::new(BACKEND_PENDING));
    let connector_backend = Arc::clone(&backend);
    let raw_value = raw_pipe as usize;
    let connector = thread::Builder::new()
        .name("bloader-windows-terminal".to_string())
        .spawn(move || {
            let raw_pipe = raw_value as *mut c_void;
            let connected = unsafe {
                ConnectNamedPipe(raw_pipe, null_mut()) != 0
                    || get_last_error_raw() == ERROR_PIPE_CONNECTED
            };
            if !connected {
                unsafe { close_handle_raw(raw_pipe); }
                if connector_backend
                    .compare_exchange(
                        BACKEND_PENDING,
                        BACKEND_CLASSIC,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    logging::scoped_warn_message(
                        "console",
                        "Windows Terminal bridge connection failed; switching to a visible classic console.",
                    );
                    unsafe { init_classic_console(false); }
                }
                return;
            }

            if connector_backend
                .compare_exchange(
                    BACKEND_PENDING,
                    BACKEND_WINDOWS_TERMINAL,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                unsafe { close_handle_raw(raw_pipe); }
                return;
            }

            let handle = HANDLE(raw_pipe);
            emit_runtime_identity();
            logging::set_console_stream_handle(handle, true);
            logging::scoped_debug_message(
                "console",
                "runtime console ready | backend=windows-terminal | transport=named-pipe | reader=BLoaderConsoleHost.exe | banner=host-real-width | shell=false | rundll32=false | startup_blocking=false",
            );
        });

    if connector.is_err() {
        unsafe { close_handle_raw(raw_pipe); }
        logging::write_bootstrap_marker(
            "console.windows_terminal.connector.spawn.failed fallback=classic",
        );
        return false;
    }

    let watchdog_backend = Arc::clone(&backend);
    if thread::Builder::new()
        .name("bloader-console-watchdog".to_string())
        .spawn(move || {
            thread::sleep(Duration::from_millis(WINDOWS_TERMINAL_HANDSHAKE_TIMEOUT_MS));
            if watchdog_backend
                .compare_exchange(
                    BACKEND_PENDING,
                    BACKEND_CLASSIC,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                logging::scoped_warn_message(
                    "console",
                    "Windows Terminal bridge handshake timed out; switching to a visible classic console.",
                );
                unsafe { init_classic_console(false); }
            }
        })
        .is_err()
        && backend
            .compare_exchange(
                BACKEND_PENDING,
                BACKEND_CLASSIC,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    {
        logging::write_bootstrap_marker(
            "console.windows_terminal.watchdog.spawn.failed fallback=classic",
        );
        unsafe { init_classic_console(false); }
    }

    true
}

/// Legacy ABI compatibility only. Runtime Windows Terminal hosting no longer uses
/// rundll32 or a DLL export; BLoaderConsoleHost.exe owns the client side.
pub unsafe fn run_rundll32_console_bridge(_cmdline: *const u8) {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_host_name_is_stable() {
        assert_eq!(CONSOLE_HOST_EXE, "BLoaderConsoleHost.exe");
    }
}

// src/core/console.rs
#![allow(dead_code)]
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::process::{Command, Stdio};
use std::ptr::null_mut;
use std::sync::mpsc;
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
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    DeleteMenu, GetSystemMenu, MF_BYCOMMAND, SC_CLOSE, SW_SHOW, ShowWindow,
};
use windows::core::PCWSTR;

const WINDOWS_TERMINAL_COLUMNS: usize = 120;
const CLASSIC_SCROLLBACK_LINES: i16 = 12_000;
const ERROR_PIPE_CONNECTED: u32 = 535;
const PIPE_ACCESS_OUTBOUND: u32 = 0x0000_0002;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;

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
    let has_existing_console = existing_window.0 != null_mut();

    if !has_existing_console && launch_windows_terminal_async() {
        return;
    }

    init_classic_console(has_existing_console);
}

unsafe fn init_classic_console(is_existing: bool) {
    let mut window = GetConsoleWindow();
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

        configure_classic_scrollback(h_conout);
        logging::set_console_handle(h_conout);
        render_branding(h_conout, visible_columns(h_conout), console_has_vt(h_conout), true);
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
                mode.0
                    | ENABLE_EXTENDED_FLAGS.0
                    | ENABLE_LINE_INPUT.0
                    | ENABLE_ECHO_INPUT.0
                    | ENABLE_PROCESSED_INPUT.0
                    | ENABLE_QUICK_EDIT_MODE.0,
            );
            let _ = SetConsoleMode(h_conin, new_mode);
        }
    }

    logging::scoped_debug_message(
        "console",
        &format!(
            "runtime console ready | backend=console-host-fallback | source={} | quick_edit=true | scrollback={} | columns={} | file_io={}",
            if is_existing { "existing" } else { "allocated" },
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
        return false;
    }

    // Arm ConnectNamedPipe before wt.exe starts. The previous order launched
    // `type \\.\pipe\...` first, so the client could observe the only pipe
    // instance before it entered the listening state and fail with ERROR_PIPE_BUSY.
    let raw_value = raw_pipe as usize;
    let (armed_tx, armed_rx) = mpsc::sync_channel(0);
    let connector = thread::Builder::new()
        .name("bloader-windows-terminal".to_string())
        .spawn(move || {
            let raw_pipe = raw_value as *mut c_void;
            let _ = armed_tx.send(());
            let connected = unsafe {
                ConnectNamedPipe(raw_pipe, null_mut()) != 0
                    || get_last_error_raw() == ERROR_PIPE_CONNECTED
            };
            if !connected {
                unsafe {
                    close_handle_raw(raw_pipe);
                }
                logging::scoped_warn_message(
                    "console",
                    "Windows Terminal runtime log pipe connection failed; continuing without an interactive console.",
                );
                return;
            }

            let handle = HANDLE(raw_pipe);
            logging::set_console_stream_handle(handle, true);
            render_branding(handle, WINDOWS_TERMINAL_COLUMNS, true, false);
            emit_runtime_identity();
            replay_pre_main_load_state();
            crate::core::xuser_bridge::publish_pending_logs();
            schedule_mod_inventory_replay();
            logging::scoped_debug_message(
                "console",
                "runtime console ready | backend=windows-terminal | transport=named-pipe | server_armed_before_client=true | reader=cmd-type | powershell=false | startup_blocking=false | close_isolated=true",
            );
        });

    if connector.is_err() {
        unsafe {
            close_handle_raw(raw_pipe);
        }
        return false;
    }

    // Wait only for the connector thread to reach the point immediately before
    // ConnectNamedPipe, then yield a tiny scheduling window. This is not a game
    // startup delay; it only orders the two console-side actors.
    let _ = armed_rx.recv_timeout(Duration::from_millis(50));
    thread::sleep(Duration::from_millis(2));

    let title = format!(
        "BLoader {} | Minecraft {} | Runtime Console",
        build_info::VERSION,
        file_io_policy::host_version().unwrap_or("unknown")
    );
    let reader_args = windows_terminal_reader_args(&pipe_path);
    let mut command = Command::new("wt.exe");
    command.args([
        "-w",
        "new",
        "--size",
        "120,40",
        "new-tab",
        "--title",
        title.as_str(),
        "--suppressApplicationTitle",
    ]);
    command.args(&reader_args);

    // The reader also has a silent, bounded retry loop. It normally succeeds on
    // the first attempt because the server is armed above; the loop only covers
    // an extreme scheduler race and never prints ERROR_PIPE_BUSY to the terminal.
    if command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_err()
    {
        // wt.exe itself is unavailable. The blocked connector owns raw_pipe and
        // will be released when the process exits; return false so the caller can
        // provide the classic Console Host fallback for this session.
        return false;
    }

    true
}

fn windows_terminal_reader_args(pipe_path: &str) -> Vec<String> {
    let command = format!(
        "for /l %i in (1,1,512) do @(type \"{pipe_path}\" 2>nul && exit /b 0) & exit /b 1"
    );
    vec![
        "cmd.exe".to_string(),
        "/d".to_string(),
        "/q".to_string(),
        "/c".to_string(),
        command,
    ]
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

fn schedule_mod_inventory_replay() {
    let _ = thread::Builder::new()
        .name("bloader-console-mod-inventory".to_string())
        .spawn(|| {
            thread::sleep(Duration::from_millis(2_000));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_terminal_reader_is_shell_minimal_and_hides_pipe_busy_race() {
        let args = windows_terminal_reader_args(r"\\.\pipe\BLoader.Console.1234");
        assert_eq!(args[0], "cmd.exe");
        assert_eq!(&args[1..4], &["/d", "/q", "/c"]);
        assert!(args[4].contains("for /l"));
        assert!(args[4].contains("2>nul"));
        assert!(args[4].contains(r"\\.\pipe\BLoader.Console.1234"));
        assert!(args.iter().all(|arg| !arg.to_ascii_lowercase().contains("powershell")));
    }
}

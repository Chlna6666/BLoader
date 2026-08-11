#![allow(non_snake_case)]

use std::env;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::thread;
use std::time::Duration;

#[path = "../runtime/foundation/console_branding.rs"]
mod console_branding;

use windows::Win32::Foundation::{GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows::Win32::System::Console::{
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_PROCESSED_OUTPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WRAP_AT_EOL_OUTPUT, GetConsoleMode,
    GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleMode, SetConsoleOutputCP,
};
use windows::core::PCWSTR;

const PIPE_OPEN_RETRIES: usize = 1_000;
const PIPE_OPEN_RETRY_MS: u64 = 2;
const PIPE_PREFIX: &str = r"\\.\pipe\BLoader.Console.";

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "CloseHandle"]
    fn close_handle_raw(handle: *mut c_void) -> i32;
}

fn main() {
    let Some(pipe_path) = parse_pipe_arg() else {
        eprintln!("BLoaderConsoleHost: missing --pipe argument");
        return;
    };
    if !pipe_path.starts_with(PIPE_PREFIX) {
        eprintln!("BLoaderConsoleHost: invalid pipe path");
        return;
    }

    let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.unwrap_or_default();
    if stdout.is_invalid() {
        return;
    }

    enable_terminal_output(stdout);
    render_branding(stdout);

    let Some(pipe) = open_pipe(&pipe_path) else {
        let _ = write_all_handle(
            stdout,
            b"\r\nBLoader: runtime log bridge connection failed.\r\n",
        );
        return;
    };

    pump_pipe(pipe, stdout);
    unsafe {
        close_handle_raw(pipe.0);
    }
}

fn parse_pipe_arg() -> Option<String> {
    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg.to_string_lossy().eq_ignore_ascii_case("--pipe") {
            return args.next().map(|value| value.to_string_lossy().into_owned());
        }
    }
    None
}

fn open_pipe(pipe_path: &str) -> Option<HANDLE> {
    let wide: Vec<u16> = std::ffi::OsStr::new(pipe_path)
        .encode_wide()
        .chain(Some(0))
        .collect();

    for _ in 0..PIPE_OPEN_RETRIES {
        match unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        } {
            Ok(handle) if !handle.is_invalid() => return Some(handle),
            _ => thread::sleep(Duration::from_millis(PIPE_OPEN_RETRY_MS)),
        }
    }
    None
}

fn pump_pipe(pipe: HANDLE, stdout: HANDLE) {
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let mut read = 0u32;
        if unsafe { ReadFile(pipe, Some(&mut buffer), Some(&mut read), None) }.is_err() || read == 0 {
            break;
        }
        if !write_all_handle(stdout, &buffer[..read as usize]) {
            break;
        }
    }
}

fn enable_terminal_output(stdout: HANDLE) {
    unsafe {
        let _ = SetConsoleOutputCP(65001);
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(stdout, &mut mode).is_ok() {
            let _ = SetConsoleMode(
                stdout,
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

fn render_branding(stdout: HANDLE) {
    let _ = write_all_handle(stdout, b"\x1b[0m\x1b[2J\x1b[H");
    for line in console_branding::render_banner(visible_columns(stdout), true) {
        let _ = write_all_handle(stdout, line.as_bytes());
        let _ = write_all_handle(stdout, b"\r\n");
    }
}

fn visible_columns(handle: HANDLE) -> usize {
    unsafe {
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        if GetConsoleScreenBufferInfo(handle, &mut info).is_ok() {
            let width = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
            if width > 0 {
                return width as usize;
            }
        }
    }
    120
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

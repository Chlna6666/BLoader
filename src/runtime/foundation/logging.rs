use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Local;
use tracing::{Level, debug, error, info, trace, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::Storage::FileSystem::WriteFile;
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ENABLE_WRAP_AT_EOL_OUTPUT, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleMode,
};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OutputDebugStringW(output: *const u16);
}

#[derive(Clone, Copy)]
struct SendHandle(HANDLE);

unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

static CONSOLE_HANDLE: OnceLock<SendHandle> = OnceLock::new();
static LOGGING_READY: OnceLock<()> = OnceLock::new();
static LATEST_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static ARCHIVE_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LATEST_GUARD: OnceLock<WorkerGuard> = OnceLock::new();
static ARCHIVE_GUARD: OnceLock<WorkerGuard> = OnceLock::new();
static LOG_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub fn init(level: &str) {
    if LOGGING_READY.get().is_some() {
        return;
    }

    let latest_log_path = prepare_latest_log_path();
    let archive_log_path = prepare_archive_log_path();
    let _ = LATEST_LOG_PATH.set(latest_log_path.clone());
    let _ = ARCHIVE_LOG_PATH.set(archive_log_path.clone());
    let _ = fs::write(&latest_log_path, b"");
    write_bootstrap_marker("logging.init.start");

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    let latest_appender = tracing_appender::rolling::never("logs", "latest.log");
    let archive_dir = archive_log_path
        .parent()
        .map(|value| value.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("logs"));
    let archive_name = archive_log_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("session.log")
        .to_string();
    let archive_appender = tracing_appender::rolling::never(archive_dir, archive_name);
    let (latest_writer, latest_guard) = tracing_appender::non_blocking(latest_appender);
    let (archive_writer, archive_guard) = tracing_appender::non_blocking(archive_appender);
    let _ = LATEST_GUARD.set(latest_guard);
    let _ = ARCHIVE_GUARD.set(archive_guard);

    // Console output is rendered by mirror_message so it has one consistent,
    // compact style. Tracing is kept for the two plain-text file sinks only.
    let latest_file_layer = fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(latest_writer)
        .with_span_events(FmtSpan::NONE)
        .with_filter(filter.clone());
    let archive_file_layer = fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(archive_writer)
        .with_span_events(FmtSpan::NONE)
        .with_filter(filter);

    match tracing_subscriber::registry()
        .with(latest_file_layer)
        .with(archive_file_layer)
        .try_init()
    {
        Ok(()) => write_bootstrap_marker("logging.tracing.init.ok"),
        Err(error) => write_bootstrap_marker(&format!("logging.tracing.init.failed {error}")),
    }

    let _ = LOGGING_READY.set(());
    write_bootstrap_marker(&format!("logging.init.ready level={level}"));
}


pub fn is_ready() -> bool {
    LOGGING_READY.get().is_some()
}

pub fn captured_mod_output(mod_name: &str, mod_id: &str, stream: &str, message: &str) {
    let scope = format!("mod:{mod_name}");
    let text = format!(
        "MOD_OUTPUT | mod_name={} | mod_id={} | source=native-stdio | stream={} | {}",
        mod_name, mod_id, stream, message
    );
    log_message(Level::INFO, &scope, &text);
    append_mod_log(mod_name, mod_id, stream, message);
}

pub fn captured_process_output(stream: &str, message: &str) {
    log_message(
        Level::INFO,
        "process-stdio",
        &format!(
            "PROCESS_OUTPUT | owner=unresolved | source=native-stdio | stream={} | {}",
            stream, message
        ),
    );
}

fn append_mod_log(mod_name: &str, mod_id: &str, stream: &str, message: &str) {
    let dir = PathBuf::from("logs").join("mods");
    let _ = fs::create_dir_all(&dir);
    let file_name = format!("{}-{}.log", sanitize_file_component(mod_name), sanitize_file_component(mod_id));
    let path = dir.join(file_name);
    let line = format!(
        "[{}] [thread={}] [stream={}] {}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        unsafe { GetCurrentThreadId() },
        stream,
        message,
    );
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

fn sanitize_file_component(value: &str) -> String {
    let mut result = value
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.') {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    if result.is_empty() {
        result.push_str("unknown");
    }
    result
}

pub fn set_console_handle(handle: HANDLE) {
    let _ = CONSOLE_HANDLE.set(SendHandle(handle));
    enable_console_ansi(handle);
    write_bootstrap_marker(&format!("logging.console.handle=0x{:X}", handle.0 as usize));
}

pub fn startup_banner(
    loader_name: &str,
    loader_version: &str,
    application_name: &str,
    application_version: &str,
    locale: &str,
) {
    let plain = format!(
        "\r\n+------------------------------------------------------------------+\r\n\
         |  BLOADER // MOD LOAD + CRASH DIAGNOSTICS                         |\r\n\
         +------------------------------------------------------------------+\r\n\
         |  Loader : {:<17} v{:<23} |\r\n\
         |  Host   : {:<17} v{:<23} |\r\n\
         |  Locale : {:<17} profile: core-only              |\r\n\
         |  Native : SYNC  Crash: EARLY+VEH+SEH  Stdio: CAPTURED             |\r\n\
         +------------------------------------------------------------------+\r\n",
        truncate(loader_name, 17),
        truncate(loader_version, 23),
        truncate(application_name, 17),
        truncate(application_version, 23),
        truncate(locale, 17),
    );

    if console_supports_ansi() {
        let decorated = format!("\x1b[38;5;141m{}\x1b[0m", plain);
        write_bytes_to_console(decorated.as_bytes());
    } else {
        write_bytes_to_console(plain.as_bytes());
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let mut result = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    result.push('~');
    result
}

pub fn info_message(message: &str) {
    log_message(Level::INFO, "loader", message);
}

pub fn warn_message(message: &str) {
    log_message(Level::WARN, "loader", message);
}

pub fn error_message(message: &str) {
    log_message(Level::ERROR, "loader", message);
}

pub fn emergency_error_message(scope: &str, message: &str) {
    emergency_log_message(Level::ERROR, scope, message);
}

pub fn emergency_warn_message(scope: &str, message: &str) {
    emergency_log_message(Level::WARN, scope, message);
}

pub fn emergency_info_message(scope: &str, message: &str) {
    emergency_log_message(Level::INFO, scope, message);
}

pub fn debug_message(message: &str) {
    log_message(Level::DEBUG, "loader", message);
}

pub fn trace_message(message: &str) {
    log_message(Level::TRACE, "loader", message);
}

pub fn scoped_info_message(scope: &str, message: &str) {
    log_message(Level::INFO, scope, message);
}

pub fn scoped_warn_message(scope: &str, message: &str) {
    log_message(Level::WARN, scope, message);
}

pub fn scoped_error_message(scope: &str, message: &str) {
    log_message(Level::ERROR, scope, message);
}

pub fn scoped_debug_message(scope: &str, message: &str) {
    log_message(Level::DEBUG, scope, message);
}

pub fn scoped_trace_message(scope: &str, message: &str) {
    log_message(Level::TRACE, scope, message);
}

pub fn write_bootstrap_marker(message: &str) {
    let line = format!(
        "[{}] {}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        message
    );
    write_bytes_to_bootstrap_file(line.as_bytes());

    // DllMain 阶段控制台和 tracing 可能尚未初始化。同步输出到调试器，
    // 让 Visual Studio、WinDbg 或 DebugView 能立即观察加载链路。
    let debug_line: Vec<u16> = line.encode_utf16().chain(Some(0)).collect();
    unsafe {
        OutputDebugStringW(debug_line.as_ptr());
    }
    if CONSOLE_HANDLE.get().is_some() {
        write_bytes_to_console(line.as_bytes());
    }
}

fn log_message(level: Level, scope: &str, message: &str) {
    mirror_message(level, scope, message);
    emit_event(level, scope, message);
}

fn emergency_log_message(level: Level, scope: &str, message: &str) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let sequence = LOG_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let thread_id = unsafe { GetCurrentThreadId() };
    let line = format!(
        "[{}] [seq={}] [thread={}] [{}] [{}] {}\r\n",
        timestamp,
        sequence,
        thread_id,
        level.as_str(),
        scope,
        message
    );
    write_bytes_to_console(line.as_bytes());
    write_bytes_to_bootstrap_file(line.as_bytes());
    let debug_line: Vec<u16> = line.encode_utf16().chain(Some(0)).collect();
    unsafe { OutputDebugStringW(debug_line.as_ptr()); }
}

fn emit_event(level: Level, scope: &str, message: &str) {
    let scoped_message = format!("[{}] {}", scope, message);
    match level {
        Level::ERROR => error!(target: "runtime", "{scoped_message}"),
        Level::WARN => warn!(target: "runtime", "{scoped_message}"),
        Level::INFO => info!(target: "runtime", "{scoped_message}"),
        Level::DEBUG => debug!(target: "runtime", "{scoped_message}"),
        Level::TRACE => trace!(target: "runtime", "{scoped_message}"),
    }
}

fn mirror_message(level: Level, scope: &str, message: &str) {
    let timestamp = Local::now().format("%H:%M:%S%.3f");
    let sequence = LOG_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let thread_id = unsafe { GetCurrentThreadId() };
    let (label, marker, color) = match level {
        Level::ERROR => ("ERROR", "X", "\x1b[1;91m"),
        Level::WARN => ("WARN ", "!", "\x1b[1;93m"),
        Level::INFO => ("INFO ", ">", "\x1b[1;92m"),
        Level::DEBUG => ("DEBUG", "#", "\x1b[1;96m"),
        Level::TRACE => ("TRACE", ".", "\x1b[1;95m"),
    };
    let plain_line = format!(
        "[{}] [seq={}] [thread={}] [{}] [{}] {}\r\n",
        timestamp,
        sequence,
        thread_id,
        label.trim(),
        scope,
        message
    );

    if console_supports_ansi() {
        let colored_line = format!(
            "\x1b[90m{}\x1b[0m \x1b[90m#{:06}\x1b[0m \x1b[90mt{:05}\x1b[0m {}{} {}\x1b[0m \x1b[38;5;75m{:<20}\x1b[0m \x1b[90m|\x1b[0m {}\r\n",
            timestamp,
            sequence,
            thread_id,
            color,
            marker,
            label,
            scope,
            message
        );
        write_bytes_to_console(colored_line.as_bytes());
    } else {
        write_bytes_to_console(plain_line.as_bytes());
    }
}

fn prepare_latest_log_path() -> PathBuf {
    let log_dir = PathBuf::from("logs");
    let _ = fs::create_dir_all(&log_dir);
    log_dir.join("latest.log")
}

fn prepare_archive_log_path() -> PathBuf {
    let archive_dir = PathBuf::from("logs").join("archive");
    let _ = fs::create_dir_all(&archive_dir);
    archive_dir.join(format!(
        "bloader-{}.log",
        Local::now().format("%Y%m%d-%H%M%S")
    ))
}

fn write_bytes_to_bootstrap_file(bytes: &[u8]) {
    for path in bootstrap_log_paths() {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(bytes);
            let _ = file.flush();
        }
    }
}

fn bootstrap_log_paths() -> Vec<PathBuf> {
    let latest = LATEST_LOG_PATH
        .get()
        .cloned()
        .unwrap_or_else(prepare_latest_log_path);
    let archive = ARCHIVE_LOG_PATH
        .get()
        .cloned()
        .unwrap_or_else(prepare_archive_log_path);
    vec![latest, archive]
}

fn write_bytes_to_console(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }

    let handle = CONSOLE_HANDLE
        .get()
        .map(|value| value.0)
        .or_else(|| unsafe { GetStdHandle(STD_OUTPUT_HANDLE).ok() })
        .unwrap_or_default();

    if !handle.is_invalid() {
        unsafe {
            let mut written = 0;
            let _ = WriteFile(handle, Some(bytes), Some(&mut written), None);
        }
    } else {
        let _ = io::stdout().write_all(bytes);
        let _ = io::stdout().flush();
    }
}

fn console_supports_ansi() -> bool {
    let handle = CONSOLE_HANDLE
        .get()
        .map(|value| value.0)
        .or_else(|| unsafe { GetStdHandle(STD_OUTPUT_HANDLE).ok() })
        .unwrap_or_default();

    if handle.is_invalid() {
        return false;
    }

    unsafe {
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(handle, &mut mode).is_ok() {
            return (mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING)
                == ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        }
    }

    false
}

fn enable_console_ansi(handle: HANDLE) {
    if handle.is_invalid() {
        return;
    }

    unsafe {
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(handle, &mut mode).is_ok() {
            let desired = mode
                | ENABLE_PROCESSED_OUTPUT
                | ENABLE_WRAP_AT_EOL_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
            let _ = SetConsoleMode(handle, desired);
        }
    }
}

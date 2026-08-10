use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::Local;
use tracing::{Level, debug, error, info, trace, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::WriteFile;
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ENABLE_WRAP_AT_EOL_OUTPUT, GetConsoleMode, SetConsoleMode,
};
use windows::Win32::System::Threading::GetCurrentThreadId;

use crate::runtime::foundation::{build_info, file_io_policy};

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
static LATEST_PREPARED: OnceLock<()> = OnceLock::new();
static LATEST_GUARD: OnceLock<WorkerGuard> = OnceLock::new();
static ARCHIVE_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

pub fn init(level: &str) {
    if LOGGING_READY.get().is_some() {
        write_bootstrap_marker("logging.init.skip reason=already-initialized");
        return;
    }

    write_bootstrap_marker(&format!(
        "logging.init.start level={} mode={} host_version={}",
        level,
        file_io_policy::mode_label(),
        file_io_policy::host_version().unwrap_or("unknown")
    ));

    let filter = EnvFilter::try_new(level).unwrap_or_else(|error| {
        write_bootstrap_marker(&format!(
            "logging.filter.invalid requested={} fallback=debug error={error}",
            level
        ));
        EnvFilter::new("debug")
    });

    if file_io_policy::legacy_uwp_no_write() {
        match tracing_subscriber::registry().with(filter).try_init() {
            Ok(()) => write_bootstrap_marker("logging.tracing.init.ok sink=fileless"),
            Err(error) => write_bootstrap_marker(&format!(
                "logging.tracing.init.failed sink=fileless error={error}"
            )),
        }
        let _ = LOGGING_READY.set(());
        write_bootstrap_marker(&format!(
            "logging.init.ready level={} mode={} sinks=console+OutputDebugString file_writes=false",
            level,
            file_io_policy::mode_label()
        ));
        return;
    }

    let latest_log_path = latest_log_path();
    let archive_log_path = archive_log_path();
    let latest_dir = latest_log_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("logs"));
    let latest_name = latest_log_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("latest.log")
        .to_string();
    let archive_dir = archive_log_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("logs").join("archive"));
    let archive_name = archive_log_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("session.log")
        .to_string();

    let latest_appender = tracing_appender::rolling::never(latest_dir, latest_name);
    let archive_appender = tracing_appender::rolling::never(archive_dir, archive_name);
    let (latest_writer, latest_guard) = tracing_appender::non_blocking(latest_appender);
    let (archive_writer, archive_guard) = tracing_appender::non_blocking(archive_appender);
    let _ = LATEST_GUARD.set(latest_guard);
    let _ = ARCHIVE_GUARD.set(archive_guard);

    let latest_file_layer = fmt::layer()
        .with_target(true)
        .with_ansi(false)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_file(true)
        .with_line_number(true)
        .with_writer(latest_writer)
        .with_span_events(FmtSpan::NONE)
        .with_filter(filter.clone());
    let archive_file_layer = fmt::layer()
        .with_target(true)
        .with_ansi(false)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_file(true)
        .with_line_number(true)
        .with_writer(archive_writer)
        .with_span_events(FmtSpan::NONE)
        .with_filter(filter);

    match tracing_subscriber::registry()
        .with(latest_file_layer)
        .with(archive_file_layer)
        .try_init()
    {
        Ok(()) => write_bootstrap_marker(
            "logging.tracing.init.ok sink=files metadata=target+thread+file+line",
        ),
        Err(error) => write_bootstrap_marker(&format!(
            "logging.tracing.init.failed sink=files error={error}"
        )),
    }

    let _ = LOGGING_READY.set(());
    write_bootstrap_marker(&format!(
        "logging.init.ready level={} mode={} sinks=latest+archive+console+OutputDebugString latest={} archive={} file_writes=true",
        level,
        file_io_policy::mode_label(),
        latest_log_path.display(),
        archive_log_path.display(),
    ));
}

pub fn is_ready() -> bool {
    LOGGING_READY.get().is_some()
}

pub fn captured_mod_output(mod_name: &str, mod_id: &str, stream: &str, message: &str) {
    let scope = format!("mod:{mod_name}");
    log_message(Level::INFO, &scope, &format!("{stream}> {message}"));
    append_mod_log(mod_name, mod_id, stream, message);
}

pub fn captured_process_output(stream: &str, message: &str) {
    log_message(
        Level::INFO,
        "game-stdio",
        &format!("{stream}> {message}"),
    );
}

fn append_mod_log(mod_name: &str, mod_id: &str, stream: &str, message: &str) {
    if !file_io_policy::writes_allowed() {
        return;
    }

    let dir = PathBuf::from("logs").join("mods");
    let _ = fs::create_dir_all(&dir);
    let file_name = format!(
        "{}-{}.log",
        sanitize_file_component(mod_name),
        sanitize_file_component(mod_id)
    );
    let path = dir.join(file_name);
    let line = format!(
        "[{}] [tid={}] [thread={}] [stream={}] {}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        current_thread_id(),
        current_thread_name(),
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
    let first_attach = CONSOLE_HANDLE.set(SendHandle(handle)).is_ok();
    enable_console_ansi(handle);
    if first_attach {
        write_console_banner();
    }
    write_bootstrap_marker(&format!(
        "logging.console.ready handle=0x{:X} ansi={} layout=structured-v2",
        handle.0 as usize,
        console_supports_ansi(),
    ));
}

fn write_console_banner() {
    let host_version = file_io_policy::host_version().unwrap_or("unknown");
    let mode = file_io_policy::mode_label();
    let width = 78usize;
    let rule = "=".repeat(width);
    let lines = [
        rule.clone(),
        format!(
            " BLoader {:<10} Minecraft {:<16} {}",
            format!("v{}", build_info::VERSION),
            format!("v{host_version}"),
            build_info::LICENSE,
        ),
        format!(
            " Runtime Console | DEBUG diagnostics | file_io={} | secrets=redacted",
            mode
        ),
        format!(" Repository: {}", build_info::REPOSITORY),
        rule,
        " TIME         LEVEL  GROUP    SOURCE             THREAD     MESSAGE".to_string(),
        " -----------------------------------------------------------------------------".to_string(),
    ];
    for line in lines {
        if console_supports_ansi() {
            write_bytes_to_console(format!("\x1b[38;5;75m{line}\x1b[0m\r\n").as_bytes());
        } else {
            write_bytes_to_console(format!("{line}\r\n").as_bytes());
        }
    }
}

pub fn startup_banner(
    loader_name: &str,
    loader_version: &str,
    application_name: &str,
    application_version: &str,
    locale: &str,
) {
    log_message(
        Level::INFO,
        "bootstrap",
        &format!(
            "{} v{} | host={} v{} | locale={} | license={} | repository={} | crash=VEH+SEH | file_io={}",
            loader_name,
            loader_version,
            application_name,
            application_version,
            locale,
            build_info::LICENSE,
            build_info::REPOSITORY,
            file_io_policy::mode_label(),
        ),
    );
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
    let file_line = format!(
        "[{}] [BOOT] [tid={}] [thread={}] {}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        current_thread_id(),
        current_thread_name(),
        message
    );
    write_bytes_to_bootstrap_file(file_line.as_bytes());
    write_debug_string(&file_line);

    if CONSOLE_HANDLE.get().is_some() {
        let console_line = format_console_line(Level::DEBUG, "bootstrap", message);
        write_bytes_to_console(console_line.as_bytes());
    }
}

fn log_message(level: Level, scope: &str, message: &str) {
    mirror_message(level, scope, message);
    emit_event(level, scope, message);
}

fn emergency_log_message(level: Level, scope: &str, message: &str) {
    let line = format!(
        "[{}] [{}] [{}] [tid={}] [thread={}] {}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        level_label(level),
        scope,
        current_thread_id(),
        current_thread_name(),
        message
    );
    write_bytes_to_console(format_console_line(level, scope, message).as_bytes());
    write_bytes_to_bootstrap_file(line.as_bytes());
    write_debug_string(&line);
}

fn emit_event(level: Level, scope: &str, message: &str) {
    let scoped_message = format!("[{scope}] {message}");
    match level {
        Level::ERROR => error!(target: "runtime", "{scoped_message}"),
        Level::WARN => warn!(target: "runtime", "{scoped_message}"),
        Level::INFO => info!(target: "runtime", "{scoped_message}"),
        Level::DEBUG => debug!(target: "runtime", "{scoped_message}"),
        Level::TRACE => trace!(target: "runtime", "{scoped_message}"),
    }
}

fn mirror_message(level: Level, scope: &str, message: &str) {
    let console_line = format_console_line(level, scope, message);
    write_bytes_to_console(console_line.as_bytes());

    let debug_line = format!(
        "[{}] [{}] [{}] [tid={}] [thread={}] {}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        level_label(level),
        scope,
        current_thread_id(),
        current_thread_name(),
        message
    );
    write_debug_string(&debug_line);
}

fn format_console_line(level: Level, scope: &str, message: &str) -> String {
    let timestamp = Local::now().format("%H:%M:%S%.3f");
    let level_text = level_label(level);
    let (group, source) = console_route(scope);
    let source_text = truncate(&source, 18);
    let thread_id = current_thread_id();
    let message = message.replace('\r', "").replace('\n', " | ");
    let plain = format!(
        "{}  {:<5}  {:<7}  {:<18} t{:<7} {}\r\n",
        timestamp,
        level_text,
        group,
        source_text,
        thread_id,
        message,
    );

    if !console_supports_ansi() {
        return plain;
    }

    let level_color = match level {
        Level::ERROR => "\x1b[91m",
        Level::WARN => "\x1b[93m",
        Level::INFO => "\x1b[92m",
        Level::DEBUG => "\x1b[96m",
        Level::TRACE => "\x1b[90m",
    };
    let group_color = match group.as_str() {
        "MOD" => "\x1b[95m",
        "XUSER" => "\x1b[94m",
        "STDIO" => "\x1b[38;5;208m",
        "NET" => "\x1b[38;5;45m",
        "SYS" => "\x1b[38;5;75m",
        _ => "\x1b[97m",
    };
    format!(
        "\x1b[90m{}\x1b[0m  {}{:<5}\x1b[0m  {}{:<7}\x1b[0m  \x1b[97m{:<18}\x1b[0m \x1b[90mt{:<7}\x1b[0m {}\r\n",
        timestamp,
        level_color,
        level_text,
        group_color,
        group,
        source_text,
        thread_id,
        message,
    )
}

fn console_route(scope: &str) -> (String, String) {
    if let Some(name) = scope.strip_prefix("mod:") {
        return ("MOD".to_string(), name.to_string());
    }
    if scope == "xuser-bridge" || scope.starts_with("xuser-") {
        return (
            "XUSER".to_string(),
            scope.trim_start_matches("xuser-").to_string(),
        );
    }
    if matches!(scope, "native-stdio" | "game-stdio" | "stdio-capture") {
        return ("STDIO".to_string(), scope.to_string());
    }
    if scope.contains("network") || scope.starts_with("net-") {
        return ("NET".to_string(), scope.to_string());
    }
    if matches!(scope, "native-loader" | "preloader" | "runtime-ready") {
        return ("MOD".to_string(), scope.to_string());
    }
    if matches!(
        scope,
        "bootstrap"
            | "identity"
            | "build"
            | "host"
            | "process"
            | "paths"
            | "logging"
            | "capabilities"
            | "compat"
            | "file-redirection"
    ) {
        return ("SYS".to_string(), scope.to_string());
    }

    // BL Host log() uses the active Mod name as its logging scope. Resolve it
    // against the runtime Mod registry so SDK/native Mod logs render under MOD
    // even when they did not come through the stdout/stderr capture pipeline.
    if crate::runtime::foundation::mod_diagnostics::find_by_name(scope).is_some() {
        return ("MOD".to_string(), scope.to_string());
    }

    ("BLOADER".to_string(), scope.to_string())
}

fn current_thread_id() -> u32 {
    unsafe { GetCurrentThreadId() }
}

fn current_thread_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string()
}

fn level_label(level: Level) -> &'static str {
    match level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
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

fn latest_log_path() -> PathBuf {
    LATEST_LOG_PATH.get_or_init(prepare_latest_log_path).clone()
}

fn archive_log_path() -> PathBuf {
    ARCHIVE_LOG_PATH.get_or_init(prepare_archive_log_path).clone()
}

fn prepare_latest_log_path() -> PathBuf {
    let log_dir = PathBuf::from("logs");
    let _ = fs::create_dir_all(&log_dir);
    let path = log_dir.join("latest.log");
    if LATEST_PREPARED.set(()).is_ok() {
        let _ = fs::write(&path, b"");
    }
    path
}

fn prepare_archive_log_path() -> PathBuf {
    let archive_dir = PathBuf::from("logs").join("archive");
    let _ = fs::create_dir_all(&archive_dir);
    archive_dir.join(format!(
        "bloader-{}.log",
        Local::now().format("%Y%m%d-%H%M%S-%3f")
    ))
}

fn write_bytes_to_bootstrap_file(bytes: &[u8]) {
    if !file_io_policy::writes_allowed() {
        return;
    }

    for path in [latest_log_path(), archive_log_path()] {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(bytes);
            let _ = file.flush();
        }
    }
}

fn write_bytes_to_console(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }

    let Some(handle) = CONSOLE_HANDLE.get().map(|value| value.0) else {
        return;
    };
    if handle.is_invalid() {
        return;
    }

    unsafe {
        let mut written = 0;
        let _ = WriteFile(handle, Some(bytes), Some(&mut written), None);
    }
}

fn console_supports_ansi() -> bool {
    let Some(handle) = CONSOLE_HANDLE.get().map(|value| value.0) else {
        return false;
    };
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

fn write_debug_string(line: &str) {
    let wide: Vec<u16> = line.encode_utf16().chain(Some(0)).collect();
    unsafe {
        OutputDebugStringW(wide.as_ptr());
    }
}

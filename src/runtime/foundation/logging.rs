use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

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

// 1=ERROR, 2=WARN, 3=INFO, 4=DEBUG, 5=TRACE.
// Persistent logs are intentionally independent from this console threshold.
static CONSOLE_LEVEL: AtomicU8 = AtomicU8::new(3);

pub fn set_console_level(level: &str) {
    let value = match level.trim().to_ascii_lowercase().as_str() {
        "error" => 1,
        "warn" | "warning" => 2,
        "debug" => 4,
        "trace" => 5,
        _ => 3,
    };
    CONSOLE_LEVEL.store(value, Ordering::Release);
}

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

pub fn console_is_ready() -> bool {
    CONSOLE_HANDLE
        .get()
        .map(|value| !value.0.is_invalid())
        .unwrap_or(false)
}

pub fn replay_console_message(level: &str, scope: &str, message: &str) {
    if !console_is_ready() {
        return;
    }
    let level = match level.to_ascii_lowercase().as_str() {
        "error" => Level::ERROR,
        "warn" | "warning" => Level::WARN,
        "debug" => Level::DEBUG,
        "trace" => Level::TRACE,
        _ => Level::INFO,
    };
    if console_should_show(level, scope, message) {
        write_bytes_to_console(format_console_line(level, scope, message).as_bytes());
    }
}

pub fn captured_mod_output(mod_name: &str, mod_id: &str, stream: &str, message: &str) {
    let scope = format!("mod:{mod_name}");
    let console_message = if stream.eq_ignore_ascii_case("stderr") {
        format!("[stderr] {message}")
    } else {
        message.to_string()
    };
    log_message(Level::INFO, &scope, &console_message);
    append_mod_log(mod_name, mod_id, stream, message);
}

pub fn captured_process_output(stream: &str, message: &str) {
    let console_message = if stream.eq_ignore_ascii_case("stderr") {
        format!("[stderr] {message}")
    } else {
        message.to_string()
    };
    log_message(Level::INFO, "game-stdio", &console_message);
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
        "logging.console.ready handle=0x{:X} ansi={} layout=java-server-v1",
        handle.0 as usize,
        console_supports_ansi(),
    ));
}

fn write_console_banner() {
    let host_version = file_io_policy::host_version().unwrap_or("unknown");
    let mode = file_io_policy::mode_label();
    let debug_destination = if file_io_policy::writes_allowed() {
        "logs/latest.log"
    } else {
        "OutputDebugString (legacy UWP no-write)"
    };
    let art = [
        r" ____  _                    _           ",
        r"| __ )| |    ___   __ _  __| | ___ _ __",
        r"|  _ \| |   / _ \ / _` |/ _` |/ _ \ '__|",
        r"| |_) | |__| (_) | (_| | (_| |  __/ |   ",
        r"|____/|_____\___/ \__,_|\__,_|\___|_|   ",
    ];

    write_bytes_to_console(b"\r\n");
    for line in art {
        if console_supports_ansi() {
            write_bytes_to_console(format!("\x1b[96m{line}\x1b[0m\r\n").as_bytes());
        } else {
            write_bytes_to_console(format!("{line}\r\n").as_bytes());
        }
    }

    let metadata = [
        format!(
            "  BLoader v{}  |  Minecraft {}  |  {}",
            build_info::VERSION,
            host_version,
            build_info::LICENSE,
        ),
        format!("  Minecraft Bedrock Mod Loader  |  file_io={mode}"),
        format!("  Full debug: {debug_destination}"),
        String::new(),
    ];
    for line in metadata {
        write_bytes_to_console(format!("{line}\r\n").as_bytes());
    }
}

pub fn startup_banner(
    _loader_name: &str,
    _loader_version: &str,
    application_name: &str,
    application_version: &str,
    locale: &str,
) {
    log_message(
        Level::INFO,
        "loader",
        &format!(
            "Starting for {} {} (locale={}, file_io={})",
            application_name,
            application_version,
            locale,
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

    if console_is_ready() && console_should_show(Level::DEBUG, "bootstrap", message) {
        write_bytes_to_console(format_console_line(Level::DEBUG, "bootstrap", message).as_bytes());
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
    if console_should_show(level, scope, message) {
        write_bytes_to_console(format_console_line(level, scope, message).as_bytes());
    }

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

fn console_should_show(level: Level, scope: &str, message: &str) -> bool {
    let required = level_rank(level);
    if required > CONSOLE_LEVEL.load(Ordering::Acquire) {
        return false;
    }

    // Per-request XUser signing telemetry is intentionally kept in the complete
    // debug log at normal INFO console verbosity. The console only needs state
    // transitions and failures, matching the signal density of Java servers.
    if level == Level::INFO
        && scope == "xuser-bridge"
        && (message.starts_with("XUser token/signature request")
            || message.starts_with("BMCBL XUser pipe payload"))
    {
        return CONSOLE_LEVEL.load(Ordering::Acquire) >= 4;
    }
    true
}

fn level_rank(level: Level) -> u8 {
    match level {
        Level::ERROR => 1,
        Level::WARN => 2,
        Level::INFO => 3,
        Level::DEBUG => 4,
        Level::TRACE => 5,
    }
}

fn format_console_line(level: Level, scope: &str, message: &str) -> String {
    let timestamp = Local::now().format("%H:%M:%S");
    let level_text = level_label(level);
    let source = truncate(&console_source(scope), 28);
    let message = message.replace('\r', "").replace('\n', " | ");

    if !console_supports_ansi() {
        return format!("[{timestamp} {level_text}] [{source}]: {message}\r\n");
    }

    let level_color = match level {
        Level::ERROR => "\x1b[91m",
        Level::WARN => "\x1b[93m",
        Level::INFO => "\x1b[97m",
        Level::DEBUG => "\x1b[96m",
        Level::TRACE => "\x1b[90m",
    };
    let source_color = if scope.starts_with("mod:")
        || crate::runtime::foundation::mod_diagnostics::find_by_name(scope).is_some()
    {
        "\x1b[95m"
    } else if scope == "xuser-bridge" || scope.starts_with("xuser-") {
        "\x1b[94m"
    } else if scope.contains("network") || scope.starts_with("net-") {
        "\x1b[96m"
    } else {
        "\x1b[92m"
    };

    format!(
        "\x1b[90m[{timestamp} \x1b[0m{level_color}{level_text}\x1b[90m]\x1b[0m {source_color}[{source}]\x1b[0m: {message}\r\n"
    )
}

fn console_source(scope: &str) -> String {
    if let Some(name) = scope.strip_prefix("mod:") {
        return name.to_string();
    }
    if scope == "xuser-bridge" || scope.starts_with("xuser-") {
        return "XUser".to_string();
    }
    if scope == "game-stdio" {
        return "Minecraft".to_string();
    }
    if matches!(scope, "native-stdio" | "stdio-capture") {
        return "StdIO".to_string();
    }
    if scope.contains("network") || scope.starts_with("net-") {
        return "Network".to_string();
    }
    if matches!(scope, "native-loader" | "preloader" | "runtime-ready") {
        return "Loader".to_string();
    }
    if matches!(
        scope,
        "loader"
            | "bootstrap"
            | "identity"
            | "build"
            | "host"
            | "process"
            | "paths"
            | "logging"
            | "capabilities"
            | "compat"
            | "console"
            | "file-redirection"
    ) {
        return "BLoader".to_string();
    }
    if crate::runtime::foundation::mod_diagnostics::find_by_name(scope).is_some() {
        return scope.to_string();
    }
    format!("BLoader/{scope}")
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

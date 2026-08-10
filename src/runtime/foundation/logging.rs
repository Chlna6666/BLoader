use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread;
use std::time::Duration;

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

use crate::runtime::foundation::file_io_policy;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OutputDebugStringW(output: *const u16);
}

const CONSOLE_BACKLOG_LIMIT: usize = 4096;
const EXTERNAL_RELAY_POLL_MS: u64 = 50;
const EXTERNAL_RELAY_MAX_PENDING: usize = 128 * 1024;

#[derive(Clone, Copy)]
struct SendHandle(HANDLE);

unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

#[derive(Clone)]
struct ConsoleBacklogRecord {
    level: Level,
    scope: String,
    message: String,
}

struct ExternalStructuredLine {
    level: Level,
    source: String,
    message: String,
}

static CONSOLE_HANDLE: OnceLock<SendHandle> = OnceLock::new();
static CONSOLE_FORCE_ANSI: AtomicBool = AtomicBool::new(false);
static CONSOLE_BACKLOG: OnceLock<Mutex<VecDeque<ConsoleBacklogRecord>>> = OnceLock::new();
static EXTERNAL_LOG_RELAY_STARTED: OnceLock<()> = OnceLock::new();
static LOGGING_READY: OnceLock<()> = OnceLock::new();
static LATEST_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static ARCHIVE_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LATEST_PREPARED: OnceLock<()> = OnceLock::new();
static LATEST_GUARD: OnceLock<WorkerGuard> = OnceLock::new();
static ARCHIVE_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

// 1=ERROR, 2=WARN, 3=INFO, 4=DEBUG, 5=TRACE.
static CONSOLE_LEVEL: AtomicU8 = AtomicU8::new(3);

fn console_backlog() -> &'static Mutex<VecDeque<ConsoleBacklogRecord>> {
    CONSOLE_BACKLOG.get_or_init(|| Mutex::new(VecDeque::new()))
}

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
        "error" | "fatal" => Level::ERROR,
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
    if let Some(external) = parse_external_structured_line(message) {
        let scope = format!("mod:{}", external.source);
        log_message(external.level, &scope, &external.message);
        append_mod_log(mod_name, mod_id, stream, message);
        return;
    }

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
    if let Some(external) = parse_external_structured_line(message) {
        let scope = format!("mod:{}", external.source);
        log_message(external.level, &scope, &external.message);
        return;
    }

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
    let _ = CONSOLE_HANDLE.set(SendHandle(handle));
    CONSOLE_FORCE_ANSI.store(false, Ordering::Release);
    enable_console_ansi(handle);
    write_bootstrap_marker(&format!(
        "logging.console.ready handle=0x{:X} ansi={} banner_owner=console-branding",
        handle.0 as usize,
        console_supports_ansi(),
    ));
    flush_console_backlog();
    start_external_log_relay();
}

/// Installs a non-console stream (for example a Windows Terminal named-pipe
/// mirror) as the interactive output sink. ANSI is forced because GetConsoleMode
/// is not defined for pipe handles.
pub fn set_console_stream_handle(handle: HANDLE, ansi: bool) {
    let _ = CONSOLE_HANDLE.set(SendHandle(handle));
    CONSOLE_FORCE_ANSI.store(ansi, Ordering::Release);
    write_bootstrap_marker(&format!(
        "logging.console.stream.ready handle=0x{:X} ansi={} banner_owner=console-branding",
        handle.0 as usize,
        ansi,
    ));
    flush_console_backlog();
    start_external_log_relay();
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
    if console_should_show(level, scope, message) {
        if console_is_ready() {
            write_bytes_to_console(format_console_line(level, scope, message).as_bytes());
        } else {
            queue_console_backlog(level, scope, message);
        }
    }
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
        if console_is_ready() {
            write_bytes_to_console(format_console_line(level, scope, message).as_bytes());
        } else if !(scope.starts_with("mod:") && message.starts_with("Loaded ")) {
            // Final Mod inventory is already replayed by console.rs. Avoid one
            // duplicate status line while retaining raw Mod output and failures.
            queue_console_backlog(level, scope, message);
        }
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

fn queue_console_backlog(level: Level, scope: &str, message: &str) {
    let mut backlog = console_backlog()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    while backlog.len() >= CONSOLE_BACKLOG_LIMIT {
        backlog.pop_front();
    }
    backlog.push_back(ConsoleBacklogRecord {
        level,
        scope: scope.to_string(),
        message: message.to_string(),
    });
}

fn flush_console_backlog() {
    if !console_is_ready() {
        return;
    }
    let records = {
        let mut backlog = console_backlog()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        backlog.drain(..).collect::<Vec<_>>()
    };
    for record in records {
        if console_should_show(record.level, &record.scope, &record.message) {
            write_bytes_to_console(
                format_console_line(record.level, &record.scope, &record.message).as_bytes(),
            );
        }
    }
}

fn start_external_log_relay() {
    if !file_io_policy::writes_allowed() || EXTERNAL_LOG_RELAY_STARTED.set(()).is_err() {
        return;
    }
    let path = PathBuf::from("logs").join("latest.log");
    let _ = thread::Builder::new()
        .name("bloader-external-log-relay".to_string())
        .spawn(move || tail_external_structured_log(&path));
}

fn tail_external_structured_log(path: &Path) {
    let mut offset = 0u64;
    let mut pending = Vec::<u8>::new();
    let mut buffer = [0u8; 16 * 1024];

    loop {
        if !console_is_ready() {
            thread::sleep(Duration::from_millis(EXTERNAL_RELAY_POLL_MS));
            continue;
        }

        if let Ok(metadata) = fs::metadata(path) {
            if metadata.len() < offset {
                offset = 0;
                pending.clear();
            }
        }

        if let Ok(mut file) = File::open(path) {
            if file.seek(SeekFrom::Start(offset)).is_ok() {
                loop {
                    match file.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            offset += read as u64;
                            pending.extend_from_slice(&buffer[..read]);
                            drain_external_structured_lines(&mut pending);
                        }
                        Err(_) => break,
                    }
                }
            }
        }

        if pending.len() > EXTERNAL_RELAY_MAX_PENDING {
            pending.clear();
        }
        thread::sleep(Duration::from_millis(EXTERNAL_RELAY_POLL_MS));
    }
}

fn drain_external_structured_lines(pending: &mut Vec<u8>) {
    let mut consumed = 0usize;
    for (index, byte) in pending.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let line = String::from_utf8_lossy(&pending[consumed..index])
            .trim_end_matches('\r')
            .to_string();
        consumed = index + 1;
        let Some(external) = parse_external_structured_line(&line) else {
            continue;
        };
        let scope = format!("mod:{}", external.source);
        replay_console_message(level_label(external.level), &scope, &external.message);
    }
    if consumed > 0 {
        pending.drain(..consumed);
    }
}

fn parse_external_structured_line(line: &str) -> Option<ExternalStructuredLine> {
    let first = line.strip_prefix('[')?;
    let first_end = first.find(']')?;
    let header = first[..first_end].trim();
    let level_token = header.split_whitespace().last()?;
    let level = external_level(level_token)?;

    let second = first[first_end + 1..].trim_start().strip_prefix('[')?;
    let second_end = second.find(']')?;
    let source = second[..second_end].trim();
    if source.is_empty()
        || source.chars().count() > 64
        || source.chars().any(|value| value.is_control())
    {
        return None;
    }

    let message = sanitize_terminal_text(second[second_end + 1..].trim_start());
    Some(ExternalStructuredLine {
        level,
        source: source.to_string(),
        message,
    })
}

fn external_level(value: &str) -> Option<Level> {
    match value.trim().to_ascii_uppercase().as_str() {
        "ERROR" | "FATAL" => Some(Level::ERROR),
        "WARN" | "WARNING" => Some(Level::WARN),
        "INFO" => Some(Level::INFO),
        "DEBUG" => Some(Level::DEBUG),
        "TRACE" => Some(Level::TRACE),
        _ => None,
    }
}

fn sanitize_terminal_text(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    let _ = chars.next();
                    for control in chars.by_ref() {
                        if ('@'..='~').contains(&control) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    let _ = chars.next();
                    let mut previous_escape = false;
                    for control in chars.by_ref() {
                        if control == '\x07' || (previous_escape && control == '\\') {
                            break;
                        }
                        previous_escape = control == '\x1b';
                    }
                }
                Some(_) => {
                    let _ = chars.next();
                }
                None => {}
            }
            continue;
        }
        if ch == '\t' || !ch.is_control() {
            result.push(ch);
        }
    }
    result
}

fn console_should_show(level: Level, scope: &str, message: &str) -> bool {
    let required = level_rank(level);
    if required > CONSOLE_LEVEL.load(Ordering::Acquire) {
        return false;
    }

    if level == Level::INFO && scope == "xuser-bridge" {
        if message.starts_with("XUser token/signature request")
            || message.starts_with("BMCBL XUser pipe payload")
            || message.starts_with("early XUser diagnostics replay complete")
        {
            return CONSOLE_LEVEL.load(Ordering::Acquire) >= 4;
        }
    }

    if level == Level::INFO
        && matches!(scope, "native-stdio" | "stdio-capture")
        && message.starts_with("process stdio capture installed")
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
        Level::ERROR => "\x1b[38;2;255;92;92m",
        Level::WARN => "\x1b[38;2;255;196;87m",
        Level::INFO => "\x1b[38;2;220;224;230m",
        Level::DEBUG => "\x1b[38;2;112;183;255m",
        Level::TRACE => "\x1b[38;2;128;134;145m",
    };
    let source_color = source_color(scope, &source);

    format!(
        "\x1b[38;2;120;126;136m[{timestamp} \x1b[0m{level_color}{level_text}\x1b[38;2;120;126;136m]\x1b[0m {source_color}[{source}]\x1b[0m: \x1b[38;2;220;224;230m{message}\x1b[0m\r\n"
    )
}

fn source_color(scope: &str, source: &str) -> &'static str {
    if source == "BLoader" {
        "\x1b[38;2;93;210;220m"
    } else if source == "Minecraft" {
        "\x1b[38;2;116;201;120m"
    } else if source == "XUser" {
        "\x1b[38;2;99;165;255m"
    } else if source == "PreLoader" {
        "\x1b[38;2;239;184;86m"
    } else if source == "Proxy" {
        "\x1b[38;2;190;135;255m"
    } else if source == "StdIO" {
        "\x1b[38;2;150;155;165m"
    } else if source == "Network" {
        "\x1b[38;2;86;205;210m"
    } else if scope.starts_with("mod:")
        || crate::runtime::foundation::mod_diagnostics::find_by_name(scope).is_some()
    {
        "\x1b[38;2;218;143;255m"
    } else {
        "\x1b[38;2;166;173;186m"
    }
}

fn console_source(scope: &str) -> String {
    if let Some(name) = scope.strip_prefix("mod:") {
        return name.to_string();
    }
    if scope == "xuser-bridge" || scope.starts_with("xuser-") {
        return "XUser".to_string();
    }
    if matches!(scope, "game-stdio" | "minecraft") {
        return "Minecraft".to_string();
    }
    if matches!(scope, "native-stdio" | "stdio-capture") {
        return "StdIO".to_string();
    }
    if scope == "preloader" {
        return "PreLoader".to_string();
    }
    if scope == "proxy" {
        return "Proxy".to_string();
    }
    if scope.contains("network") || scope.starts_with("net-") {
        return "Network".to_string();
    }
    if matches!(scope, "native-loader" | "runtime-ready") {
        return "BLoader".to_string();
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
            | "premain-gate"
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

    let mut offset = 0usize;
    while offset < bytes.len() {
        unsafe {
            let mut written = 0;
            if WriteFile(handle, Some(&bytes[offset..]), Some(&mut written), None).is_err()
                || written == 0
            {
                break;
            }
            offset += written as usize;
        }
    }
}

fn console_supports_ansi() -> bool {
    if CONSOLE_FORCE_ANSI.load(Ordering::Acquire) {
        return true;
    }
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

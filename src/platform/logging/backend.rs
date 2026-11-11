use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Local;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber, debug, error, info, trace, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;
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

use crate::platform::{file_io_policy, i18n};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OutputDebugStringW(output: *const u16);
}

const CONSOLE_BACKLOG_LIMIT: usize = 4096;
const EXTERNAL_RELAY_POLL_MS: u64 = 50;
const EXTERNAL_RELAY_MAX_PENDING: usize = 128 * 1024;
/// Session-wide journal of already-forwarded captured lines. Bounded by entry
/// count, not by time: the log relay can deliver a line long after the live
/// channels did, and a time window let those late copies through again.
const EXTERNAL_DEDUPE_LIMIT: usize = 16384;

#[derive(Clone, Copy)]
struct SendHandle(HANDLE);

unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

#[derive(Clone)]
struct ConsoleBacklogRecord {
    timestamp_full: String,
    timestamp_clock: String,
    sequence: u64,
    level: Level,
    scope: String,
    message: String,
}

struct ExternalStructuredLine {
    timestamp_full: String,
    timestamp_clock: String,
    level: Level,
    source: String,
    message: String,
}

static CONSOLE_HANDLE: OnceLock<SendHandle> = OnceLock::new();
static CONSOLE_FORCE_ANSI: AtomicBool = AtomicBool::new(false);
static CONSOLE_ANSI_ENABLED: AtomicBool = AtomicBool::new(false);
static CONSOLE_BACKLOG: OnceLock<Mutex<VecDeque<ConsoleBacklogRecord>>> = OnceLock::new();
static CONSOLE_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static CONSOLE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static EXTERNAL_LOG_RELAY_STARTED: OnceLock<()> = OnceLock::new();
/// The same logical record may be seen from the native console, CRT pipes,
/// OutputDebugString, logger callback and the shared file relay. Keep a
/// per-channel occurrence counter, rather than dropping *all* equal lines
/// in the same second (which suppresses genuine repeated errors).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureChannel {
    Console,
    Stdio,
    DebugString,
    Logger,
    FileRelay,
}
impl CaptureChannel {
    const fn index(self) -> usize {
        match self {
            Self::Console => 0,
            Self::Stdio => 1,
            Self::DebugString => 2,
            Self::Logger => 3,
            Self::FileRelay => 4,
        }
    }
}
#[derive(Default)]
struct DedupeCounts {
    by_channel: [u32; 5],
    emitted: u32,
}
#[derive(Default)]
struct DedupeJournal {
    order: VecDeque<String>,
    entries: HashMap<String, DedupeCounts>,
}
static EXTERNAL_DEDUPE: OnceLock<Mutex<DedupeJournal>> = OnceLock::new();
static PROCESS_START_TIMESTAMP: OnceLock<String> = OnceLock::new();
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

fn console_write_lock() -> &'static Mutex<()> {
    CONSOLE_WRITE_LOCK.get_or_init(|| Mutex::new(()))
}

fn current_console_timestamp() -> (String, String) {
    let now = Local::now();
    (
        now.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        now.format("%H:%M:%S%.3f").to_string(),
    )
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
    // The console sink is filtered by the configured level too; without this
    // `log_level=debug` only raised the file sinks and the console stayed at
    // INFO.
    set_console_level(level);

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
        flush_early_events();
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
        .event_format(ConsoleFileFormat)
        .with_writer(latest_writer)
        .with_filter(filter.clone());
    let archive_file_layer = fmt::layer()
        .event_format(ConsoleFileFormat)
        .with_writer(archive_writer)
        .with_filter(filter);

    match tracing_subscriber::registry()
        .with(latest_file_layer)
        .with(archive_file_layer)
        .try_init()
    {
        Ok(()) => write_bootstrap_marker(
            "logging.tracing.init.ok sink=files format=console-standard",
        ),
        Err(error) => write_bootstrap_marker(&format!(
            "logging.tracing.init.failed sink=files error={error}"
        )),
    }

    let _ = LOGGING_READY.set(());
    flush_early_events();
    spawn_relay_report();
    write_bootstrap_marker(&format!(
        "logging.init.ready level={} mode={} sinks=latest+archive+console+OutputDebugString latest={} archive={} file_writes=true",
        level,
        file_io_policy::mode_label(),
        latest_log_path.display(),
        archive_log_path.display(),
    ));
}

/// One-shot observability for the external log relay, mirroring the game-log
/// channel check: every counter at zero while LeviLamina is running means the
/// relay never saw the shared log's new content, so the console shows no
/// native Mod or game lines.
fn spawn_relay_report() {
    if !file_io_policy::writes_allowed() {
        return;
    }
    let _ = thread::Builder::new()
        .name("bloader-relay-report".to_string())
        .spawn(|| {
            let started = Instant::now();
            for total_secs in [30u64, 60u64, 300u64] {
                thread::sleep(Duration::from_secs(total_secs).saturating_sub(started.elapsed()));
                if total_secs == 30 {
                    report_external_capture_health();
                } else {
                    logging_scoped_debug_relay_report(total_secs);
                }
            }
        });
}

/// BLoader can load PreLoader while its own logger never reaches the Windows
/// console. Installing a detour does not guarantee that stdout is attached,
/// and a file sink may use a different location. Give one actionable warning
/// only when both independent input channels have received no actual records.
fn report_external_capture_health() {
    let preloader_ready = crate::platform::mod_diagnostics::find_by_name("PreLoader")
        .is_some_and(|identity| identity.state == "loaded");
    if !preloader_ready {
        return;
    }
    let path = external_log_path();
    let path_state = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => format!("file_size={}", metadata.len()),
        Ok(_) => "not_a_regular_file".to_string(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing".to_string(),
        Err(error) => format!("unavailable={error}"),
    };
    let (native_fragments, native_lines) = crate::platform::console_capture::capture_counts();
    let file_lines = RELAY_SEEN.load(Ordering::Relaxed);
    let file_parsed = file_lines.saturating_sub(RELAY_UNPARSED.load(Ordering::Relaxed));
    let file_forwarded = FILE_RELAY_FORWARDED.load(Ordering::Relaxed);
    let status = format!(
        "PreLoader log capture | win32_hook={} crt_fwrite_hook={} native_fragments={native_fragments} native_lines={native_lines} file_lines={file_lines} file_parsed={file_parsed} file_forwarded={file_forwarded} log_path={} log_status={path_state}",
        crate::platform::console_capture::is_active(),
        crate::platform::console_capture::crt_fwrite_capture_active(),
        path.display(),
    );
    if native_lines == 0 && file_forwarded == 0 {
        scoped_warn_message(
            "logging",
            &format!("{status} | no external output observed after 30s; hook-installed does not mean logs-received"),
        );
    } else {
        scoped_debug_message("logging", &status);
    }
}

fn logging_scoped_debug_relay_report(total_secs: u64) {
    scoped_debug_message(
        "logging",
        &format!(
            "capture channel check (after {total_secs}s): seen={} unparsed={} forwarded={} stale_skipped={} file_batches={} file_sync_fallbacks={} console_batches={} console_late={} console_omitted={}",
            RELAY_SEEN.load(Ordering::Relaxed),
            RELAY_UNPARSED.load(Ordering::Relaxed),
            RELAY_FORWARDED.load(Ordering::Relaxed),
            RELAY_STALE_SKIPPED.load(Ordering::Relaxed),
            FILE_BATCHES.load(Ordering::Relaxed),
            FILE_SYNC_FALLBACKS.load(Ordering::Relaxed),
            CONSOLE_BATCHES.load(Ordering::Relaxed),
            CONSOLE_LATE_TOTAL.load(Ordering::Relaxed),
            CONSOLE_DROPPED_TOTAL.load(Ordering::Relaxed),
        ),
    );
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
    let (timestamp_full, _) = current_console_timestamp();
    replay_console_message_at(level, scope, message, &timestamp_full);
}

fn replay_console_message_at(level: &str, scope: &str, message: &str, timestamp_full: &str) {
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
        write_console_record_at(timestamp_full, level, scope, message);
    }
}

pub fn captured_mod_output(mod_name: &str, mod_id: &str, stream: &str, message: &str) {
    let sanitized = sanitize_terminal_text(message);
    // PreLoader's active load scope is only a fallback for unstructured
    // output. A real LeviLamina console header must override that scope.
    if let Some(external) = parse_external_structured_line(&sanitized)
        .or_else(|| parse_levilamina_console_line(&sanitized))
    {
        // The same logical line usually also arrives through the console hook
        // or the latest.log relay; route it through the shared forwarded-record
        // path so the dedupe journal sees it and the console shows it once.
        if forward_captured_record(CapturedRecord {
            timestamp_full: external.timestamp_full,
            timestamp_clock: external.timestamp_clock,
            level: external_level_for(&external.source, external.level, &external.message),
            scope: external_scope(&external.source),
            message: external.message,
            channel: CaptureChannel::Stdio,
        }) {
            append_mod_log(mod_name, mod_id, stream, message);
        }
        return;
    }

    let scope = format!("mod:{mod_name}");
    let console_message = if stream.eq_ignore_ascii_case("stderr") {
        format!("[stderr] {sanitized}")
    } else {
        sanitized
    };
    let (timestamp_full, timestamp_clock) = current_console_timestamp();
    if forward_captured_record(CapturedRecord {
        timestamp_full,
        timestamp_clock,
        level: level_from_markers(&console_message),
        scope,
        message: console_message,
        channel: CaptureChannel::Stdio,
    }) {
        append_mod_log(mod_name, mod_id, stream, message);
    }
}

pub fn captured_process_output(stream: &str, message: &str) {
    let sanitized = sanitize_terminal_text(message);
    if let Some(external) = parse_external_structured_line(&sanitized)
        .or_else(|| parse_levilamina_console_line(&sanitized))
    {
        forward_captured_record(CapturedRecord {
            timestamp_full: external.timestamp_full,
            timestamp_clock: external.timestamp_clock,
            level: external_level_for(&external.source, external.level, &external.message),
            scope: external_scope(&external.source),
            message: external.message,
            channel: CaptureChannel::Stdio,
        });
        return;
    }

    // Native loaders such as LeviLamina own the process console and print
    // colored text that may arrive as a fragment or without a parsable
    // structured header. Never let their output fall through to the
    // game-stdio scope, which is rendered as [Minecraft].
    let scope = if let Some(scope) = known_external_scope(&sanitized) {
        scope
    } else if let Some(owner) = crate::platform::mod_diagnostics::resolve_output_owner(&sanitized) {
        format!("mod:{}", owner.name)
    } else {
        "native-stdio".to_string()
    };
    let console_message = if stream.eq_ignore_ascii_case("stderr") {
        format!("[stderr] {sanitized}")
    } else {
        sanitized
    };
    let (timestamp_full, timestamp_clock) = current_console_timestamp();
    forward_captured_record(CapturedRecord {
        timestamp_full,
        timestamp_clock,
        level: level_from_markers(&console_message),
        scope,
        message: console_message,
        channel: CaptureChannel::Stdio,
    });
}

/// Forwards one line captured directly from an in-process logger (the
/// LeviLamina `ll::io` bridge). The level is already known, so no marker
/// guessing is needed; the record goes through the same dedupe and sinks as
/// every other captured line.
pub fn captured_logger_line(scope: &str, level: Level, message: &str) {
    let sanitized = sanitize_terminal_text(message);
    if sanitized.trim().is_empty() {
        return;
    }
    let (timestamp_full, timestamp_clock) = current_console_timestamp();
    forward_captured_record(CapturedRecord {
        timestamp_full,
        timestamp_clock,
        level,
        scope: scope.to_string(),
        message: sanitized,
        channel: CaptureChannel::Logger,
    });
}

fn append_mod_log(mod_name: &str, mod_id: &str, stream: &str, message: &str) {
    if !file_io_policy::writes_allowed() {
        return;
    }
    let file_name = format!(
        "{}-{}.log",
        sanitize_file_component(mod_name),
        sanitize_file_component(mod_id)
    );
    let path = PathBuf::from("logs").join("mods").join(file_name);
    let line = format!(
        "[{}] [tid={}] [thread={}] [stream={}] {}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        current_thread_id(),
        current_thread_name(),
        stream,
        message,
    );
    enqueue_file_write(FileWrite::Mod(path, line.into_bytes()));
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
    let relay_offset = external_history_start_offset();
    let _ = CONSOLE_HANDLE.set(SendHandle(handle));
    CONSOLE_FORCE_ANSI.store(false, Ordering::Release);
    enable_console_ansi(handle);
    // A GetConsoleMode syscall for every formatted line was unnecessary.
    // Refresh only when the host console sink is initialized.
    CONSOLE_ANSI_ENABLED.store(query_console_ansi(handle), Ordering::Release);
    write_bootstrap_marker(&format!(
        "logging.console.ready handle=0x{:X} ansi={} banner_owner=console-branding",
        handle.0 as usize,
        console_supports_ansi(),
    ));
    flush_console_backlog();
    start_external_log_relay(relay_offset);
}

/// Installs a non-console stream (for example a Windows Terminal named-pipe
/// mirror) as the interactive output sink. ANSI is forced because GetConsoleMode
/// is not defined for pipe handles.
pub fn set_console_stream_handle(handle: HANDLE, ansi: bool) {
    let relay_offset = external_history_start_offset();
    let _ = CONSOLE_HANDLE.set(SendHandle(handle));
    CONSOLE_FORCE_ANSI.store(ansi, Ordering::Release);
    CONSOLE_ANSI_ENABLED.store(ansi, Ordering::Release);
    write_bootstrap_marker(&format!(
        "logging.console.stream.ready handle=0x{:X} ansi={} banner_owner=console-branding",
        handle.0 as usize,
        ansi,
    ));
    flush_console_backlog();
    start_external_log_relay(relay_offset);
}

pub fn startup_banner(
    _loader_name: &str,
    _loader_version: &str,
    application_name: &str,
    application_version: &str,
    _locale: &str,
) {
    let message = i18n::tr("console.starting")
        .replace("{app}", application_name)
        .replace("{version}", application_version);
    log_message(Level::INFO, "loader", &message);
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
        let (timestamp_full, _) = current_console_timestamp();
        write_console_record_at(&timestamp_full, Level::DEBUG, "bootstrap", message);
    }
}

// Log events generated before the tracing subscriber must be preserved with
// their actual event time, not silently lost or retimestamped upon replay.
#[derive(Clone)]
struct EarlyFileEvent {
    timestamp: String,
    level: Level,
    scope: String,
    message: String,
}
static EARLY_EVENTS: OnceLock<Mutex<VecDeque<EarlyFileEvent>>> = OnceLock::new();
const EARLY_EVENTS_LIMIT: usize = 4096;

fn log_message(level: Level, scope: &str, message: &str) {
    if LOGGING_READY.get().is_none() {
        let (timestamp, clock) = current_console_timestamp();
        let store = EARLY_EVENTS.get_or_init(|| Mutex::new(VecDeque::new()));
        let mut pending = store.lock().unwrap_or_else(|e| e.into_inner());
        if pending.len() == EARLY_EVENTS_LIMIT { pending.pop_front(); }
        pending.push_back(EarlyFileEvent {
            timestamp: timestamp.clone(), level, scope: scope.to_string(), message: message.to_string(),
        });
        drop(pending);
        mirror_message_at(level, scope, message, &timestamp, &clock);
    } else {
        mirror_message(level, scope, message);
        emit_event(level, scope, message);
    }
}

fn flush_early_events() {
    let Some(store) = EARLY_EVENTS.get() else { return; };
    let entries = {
        let mut lock = store.lock().unwrap_or_else(|e| e.into_inner());
        lock.drain(..).collect::<Vec<_>>()
    };
    if !file_io_policy::writes_allowed() { return; }
    for event in entries {
        // Avoid duplicate console output from the external log tailer.
        let text = format!(
            "EARLY [{}] [{}] [{}] {}\r\n",
            event.timestamp, level_label(event.level), event.scope, event.message
        );
        write_bytes_to_bootstrap_file(text.as_bytes());
    }
}

/// Console capture with the owning module already resolved from the write's
/// caller. The attribution wins over the line's own header, because a native
/// mod may relay another component's text and the caller is authoritative.
pub fn captured_console_output_from(attribution: Option<&str>, input: &str) {
    let line = sanitize_terminal_text(input);
    if line.trim().is_empty() {
        return;
    }
    // LeviLamina's Win32 console sink uses "HH:MM:SS.mmm LEVEL SOURCE text",
    // whereas its file sink uses "[date time LEVEL][SOURCE] text".
    // Capture both forms before fallback stack attribution; otherwise native
    // WARN/ERROR events appear as INFO with duplicated timestamps.
    if let Some(external) =
        parse_external_structured_line(&line).or_else(|| parse_levilamina_console_line(&line))
    {
        if external_line_ignored(&external.source) {
            return;
        }
        // Prefer the structured producer tag for LeviLamina's cross-DLL
        // forwarding. A shim's call stack often belongs to PreLoader.
        let scope = external_scope(&external.source);
        forward_captured_record(CapturedRecord {
            timestamp_full: external.timestamp_full,
            timestamp_clock: external.timestamp_clock,
            level: external_level_for(&external.source, external.level, &external.message),
            scope,
            message: external.message,
            channel: CaptureChannel::Console,
        });
        return;
    }
    if let Some(scope) = attribution {
        let level = level_from_markers(&line);
        forward_captured_now(scope, level, &line, CaptureChannel::Console);
        return;
    }
    // No caller attribution (the write came through a shim). Fall back to the
    // line's own prefix; every branch still goes through the shared record
    // path so a copy from another channel is deduplicated.
    if let Some(rest) = line.strip_prefix('[')
        && let Some((source, value)) = rest.split_once(']')
        && source.len() >= 3
        && source.len() <= 40
        && source
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        let scope = external_scope(source);
        let value = value.trim_start().to_string();
        forward_captured_now(&scope, Level::INFO, &value, CaptureChannel::Console);
    } else if let Some(owner) = crate::platform::mod_diagnostics::resolve_output_owner(&line) {
        forward_captured_now(&format!("mod:{}", owner.name), Level::INFO, &line, CaptureChannel::Console);
    } else if let Some(scope) = known_external_scope(&line) {
        forward_captured_now(&scope, Level::INFO, &line, CaptureChannel::Console);
    } else {
        forward_captured_now("native-stdio", Level::INFO, &line, CaptureChannel::Console);
    }
}

fn level_from_markers(text: &str) -> Level {
    if text.contains("[ERROR]") {
        Level::ERROR
    } else if text.contains("[WARN]") {
        Level::WARN
    } else if text.contains("[DEBUG]") {
        Level::DEBUG
    } else if text.contains("[TRACE]") {
        Level::TRACE
    } else {
        Level::INFO
    }
}

fn enqueue_early_replay(level: Level, scope: &str, message: &str, timestamp: &str) {
    let store = EARLY_EVENTS.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut pending = store.lock().unwrap_or_else(|e| e.into_inner());
    if pending.len() == EARLY_EVENTS_LIMIT { pending.pop_front(); }
    pending.push_back(EarlyFileEvent {
        timestamp: timestamp.to_string(), level, scope: scope.to_string(), message: message.to_string(),
    });
}

/// Replay a previously recorded event with its original timestamp.
pub fn replay_early_message_at(level: &str, scope: &str, message: &str, timestamp: &str) {
    let level = match level {
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };
    let clock = timestamp.split_whitespace().last().unwrap_or("00:00:00.000");
    if LOGGING_READY.get().is_none() {
        enqueue_early_replay(level, scope, message, timestamp);
    }
    mirror_message_at(level, scope, message, timestamp, clock);
    if LOGGING_READY.get().is_some() {
        emit_event(level, scope, &format!("replayed=true occurred_at={timestamp} {message}"));
    }
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
            let (timestamp_full, _) = current_console_timestamp();
            write_console_record_at(&timestamp_full, level, scope, message);
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
    let (timestamp_full, timestamp_clock) = current_console_timestamp();
    mirror_message_at(level, scope, message, &timestamp_full, &timestamp_clock);
}

fn mirror_message_at(
    level: Level,
    scope: &str,
    message: &str,
    timestamp_full: &str,
    timestamp_clock: &str,
) {
    if console_should_show(level, scope, message) {
        if console_is_ready() {
            write_console_record_at(timestamp_full, level, scope, message);
        } else {
            queue_console_backlog_at(level, scope, message, timestamp_full, timestamp_clock);
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
    let (timestamp_full, timestamp_clock) = current_console_timestamp();
    queue_console_backlog_at(level, scope, message, &timestamp_full, &timestamp_clock);
}

fn queue_console_backlog_at(
    level: Level,
    scope: &str,
    message: &str,
    timestamp_full: &str,
    timestamp_clock: &str,
) {
    let mut backlog = console_backlog()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    while backlog.len() >= CONSOLE_BACKLOG_LIMIT {
        backlog.pop_front();
    }
    backlog.push_back(ConsoleBacklogRecord {
        timestamp_full: timestamp_full.to_string(),
        timestamp_clock: timestamp_clock.to_string(),
        sequence: CONSOLE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        level,
        scope: scope.to_string(),
        message: message.to_string(),
    });
}

fn flush_console_backlog() {
    if !console_is_ready() {
        return;
    }
    let mut records = {
        let mut backlog = console_backlog()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        backlog.drain(..).collect::<Vec<_>>()
    };
    records.sort_by(|left, right| {
        left.timestamp_full
            .cmp(&right.timestamp_full)
            .then(left.sequence.cmp(&right.sequence))
    });
    for record in records {
        if console_should_show(record.level, &record.scope, &record.message) {
            write_console_record_at(
                &record.timestamp_full, record.level, &record.scope, &record.message,
            );
        }
    }
}

/// Discover the bounded history range without performing file I/O on the
/// Minecraft bootstrap thread. The relay worker parses this tail asynchronously.
/// Older entries are excluded by the process-start timestamp filter.
const EXTERNAL_HISTORY_TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// LeviLamina's default FileSink writes logs/latest.log relative to the game
/// directory. Resolve it once from the executable, not the process CWD:
/// native mods may change the CWD after our relay starts.
fn external_log_path() -> PathBuf {
    crate::platform::utils::get_exe_directory()
        .join("logs")
        .join("latest.log")
}

fn external_history_start_offset() -> u64 {
    if !file_io_policy::writes_allowed() {
        return 0;
    }
    let path = external_log_path();
    fs::metadata(path)
        .map(|metadata| metadata.len().saturating_sub(EXTERNAL_HISTORY_TAIL_BYTES))
        .unwrap_or(0)
}

fn start_external_log_relay(start_offset: u64) {
    if !file_io_policy::writes_allowed() || EXTERNAL_LOG_RELAY_STARTED.set(()).is_err() {
        return;
    }
    let path = external_log_path();
    let _ = thread::Builder::new()
        .name("bloader-external-log-relay".to_string())
        .spawn(move || tail_external_structured_log(&path, start_offset));
}

/// Cheap identity of the file currently living at `path`: creation time plus
/// size. LeviLamina rotates `latest.log` by renaming it and creating a fresh
/// one; a renamed file keeps its content, so a size-only check cannot tell
/// "the other writer truncated" apart from "the file was swapped" — and both
/// break the byte offset differently. Comparing creation time detects the
/// swap reliably.
#[cfg(windows)]
fn file_epoch(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::fs::MetadataExt;
    let metadata = fs::metadata(path).ok()?;
    // creation_time() is the Win32 FILETIME (100ns ticks since 1601).
    Some((metadata.creation_time(), metadata.len()))
}

#[cfg(not(windows))]
fn file_epoch(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(path).ok()?;
    Some((0, metadata.len()))
}

fn tail_external_structured_log(path: &Path, mut offset: u64) {
    let mut pending = Vec::<u8>::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut epoch = file_epoch(path);

    loop {
        if !console_is_ready() {
            thread::sleep(Duration::from_millis(EXTERNAL_RELAY_POLL_MS));
            continue;
        }

        let current = file_epoch(path);
        let rotated_or_truncated = match (epoch, current) {
            (Some((old_creation, _)), Some((new_creation, new_len))) => {
                old_creation != new_creation || new_len < offset
            }
            (None, None) => false,
            _ => true,
        };
        if rotated_or_truncated {
            // Rotation or truncation invalidates the byte offset; appends do not.
            offset = 0;
            pending.clear();
        }
        epoch = current;

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
        RELAY_SEEN.fetch_add(1, Ordering::Relaxed);
        let Some(external) = parse_external_structured_line(&line) else {
            RELAY_UNPARSED.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        forward_external_line(&external);
    }
    if consumed > 0 {
        pending.drain(..consumed);
    }
}

/// Relay observability: how many lines the latest.log tailer read, how many
/// carried a structured header, how many passed the current-run filter and
/// reached the sinks. All zero with LeviLamina running means the relay never
/// saw the file's new content (rotation detection or offset issue).
static RELAY_SEEN: AtomicU64 = AtomicU64::new(0);
static RELAY_UNPARSED: AtomicU64 = AtomicU64::new(0);
static RELAY_FORWARDED: AtomicU64 = AtomicU64::new(0);
static RELAY_STALE_SKIPPED: AtomicU64 = AtomicU64::new(0);
static FILE_RELAY_FORWARDED: AtomicU64 = AtomicU64::new(0);

/// Records the earliest BLoader execution point (DllMain entry). External log
/// lines keep their original timestamps, and the shared `logs/latest.log` may
/// still hold entire previous sessions when LeviLamina has not truncated it
/// yet, so anything older than this moment is history and must never be
/// re-printed on the console.
pub fn note_process_start() {
    let _ = PROCESS_START_TIMESTAMP.set(current_console_timestamp().0);
}

fn external_line_from_current_run(timestamp_full: &str) -> bool {
    PROCESS_START_TIMESTAMP
        .get()
        .is_none_or(|start| external_timestamp_at_or_after(timestamp_full, start))
}

fn external_timestamp_at_or_after(timestamp_full: &str, process_start: &str) -> bool {
    // Source loggers may omit milliseconds; include the shared first second.
    let event_second = timestamp_full.get(..19).unwrap_or(timestamp_full);
    let start_second = process_start.get(..19).unwrap_or(process_start);
    event_second >= start_second
}

/// Renders one line captured from the game's own logging channel
/// (OutputDebugStringA/W) through the unified console and file sinks. These
/// are internal logs Minecraft never writes to stdout, so they only surface
/// here. `scope` is the caller attribution; a line that carries its own
/// structured header keeps that header's level and message but never overrides
/// the module that produced it.
pub fn game_console_message(scope: &str, message: &str) {
    let mut text = message.trim_start();
    while let Some(rest) = text.strip_prefix("NO LOG FILE! - ") {
        text = rest.trim_start();
    }
    let sanitized = sanitize_terminal_text(text);
    if let Some(external) = parse_external_structured_line(&sanitized) {
        forward_captured_record(CapturedRecord {
            timestamp_full: external.timestamp_full,
            timestamp_clock: external.timestamp_clock,
            level: external_level_for(&external.source, external.level, &external.message),
            scope: scope.to_string(),
            message: external.message,
            channel: CaptureChannel::DebugString,
        });
        return;
    }
    let level = level_from_markers(&sanitized);
    let resolved_scope = known_external_scope(&sanitized).unwrap_or_else(|| scope.to_string());
    let level = if is_runtime_internal_noise(&sanitized) {
        Level::DEBUG
    } else {
        level
    };
    // Route through the shared record path: the same line may also arrive from
    // the console hook or the log relay, and the dedupe journal keeps one copy.
    forward_captured_now(&resolved_scope, level, &sanitized, CaptureChannel::DebugString);
}

/// One captured line from any shared channel, already attributed to the module
/// that produced it. Both the console sink and the file sinks receive the same
/// record so "captured" means recorded, not merely printed.
struct CapturedRecord {
    timestamp_full: String,
    timestamp_clock: String,
    level: Level,
    scope: String,
    message: String,
    channel: CaptureChannel,
}

/// Forwards one captured line that carries no timestamp of its own. Every
/// capture channel goes through the same record path so the dedupe journal sees
/// all copies and the console shows each line once.
fn forward_captured_now(
    scope: &str,
    level: Level,
    message: &str,
    channel: CaptureChannel,
) {
    let (timestamp_full, timestamp_clock) = current_console_timestamp();
    forward_captured_record(CapturedRecord {
        timestamp_full,
        timestamp_clock,
        level,
        scope: scope.to_string(),
        message: message.to_string(),
        channel,
    });
}

/// Forwards one parsed external line to the unified sinks. The live Win32
/// console capture and the shared `latest.log` tail relay both call this path,
/// so native mods (LeviLamina, the Bedrock server, ...) and the game itself
/// render exactly like BLoader's own log events.
fn forward_external_line(external: &ExternalStructuredLine) {
    if external_line_ignored(&external.source) {
        return;
    }
    if forward_captured_record(CapturedRecord {
        timestamp_full: external.timestamp_full.clone(),
        timestamp_clock: external.timestamp_clock.clone(),
        level: external_level_for(&external.source, external.level, &external.message),
        scope: external_scope(&external.source),
        message: external.message.clone(),
        channel: CaptureChannel::FileRelay,
    }) {
        FILE_RELAY_FORWARDED.fetch_add(1, Ordering::Relaxed);
    }
}

fn forward_captured_record(record: CapturedRecord) -> bool {
    if !external_line_from_current_run(&record.timestamp_full) {
        RELAY_STALE_SKIPPED.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    if !record_captured_line_once(&record) {
        return false;
    }
    RELAY_FORWARDED.fetch_add(1, Ordering::Relaxed);
    if console_should_show(record.level, &record.scope, &record.message) {
        if console_is_ready() {
            write_console_record_at(
                &record.timestamp_full,
                record.level,
                &record.scope,
                &record.message,
            );
        } else {
            queue_console_backlog_at(
                record.level,
                &record.scope,
                &record.message,
                &record.timestamp_full,
                &record.timestamp_clock,
            );
        }
    }
    write_captured_file_record(&record);
    true
}

/// External lines keep their original timestamp in the file sinks; without
/// this the captured output only ever existed on the console and the unified
/// log was missing every native Mod and game line.
fn write_captured_file_record(record: &CapturedRecord) {
    if !file_io_policy::writes_allowed() {
        return;
    }
    // The file sinks use the same configured level as the console, so a
    // captured DEBUG line must not leak into an `info` log.
    if level_rank(record.level) > CONSOLE_LEVEL.load(Ordering::Acquire) {
        return;
    }
    let line = format!(
        "[{}] [{}] [{}] {}\r\n",
        record.timestamp_full,
        level_label(record.level),
        record.scope,
        record.message
    );
    // The relay source is LeviLamina's logs/latest.log, while BLoader's
    // unified file is logs/bloader/latest.log; this cannot feed back.
    enqueue_file_write(FileWrite::Main(line.into_bytes()));
}

/// Maps a parsed external line's source tag to a console scope. The game's
/// own log arrives through its native OutputDebugString channel; other
/// sources keep their own names.
fn external_scope(source: &str) -> String {
    match source.trim().to_ascii_lowercase().as_str() {
        "minecraft" | "bedrock" => "game-log".to_string(),
        _ => format!("mod:{}", source.trim()),
    }
}

/// Sources whose content is not captured: the game's log is already covered
/// by its native OutputDebugString channel.
fn external_line_ignored(source: &str) -> bool {
    source.trim().eq_ignore_ascii_case("server")
}

/// Microsoft game-runtime DLLs emit their own startup diagnostics through the
/// shared debug channel, e.g. "Runtime Service struct SErrorImpl Created".
/// Those are component-internal traces, not the game's ContentLog, so they
/// must not render as [Minecraft] at INFO. They stay available at DEBUG for
/// capture-chain debugging.
fn is_runtime_internal_noise(message: &str) -> bool {
    message.starts_with("Runtime Service struct ")
        && (message.contains(" Created") || message.contains(" Destroyed"))
}

fn external_level_for(source: &str, level: Level, message: &str) -> Level {
    if is_runtime_internal_noise(message) {
        return Level::DEBUG;
    }
    level
}

/// Fallback attribution for native console/stdio lines that are not in the
/// timestamped [date time LEVEL][source] format: the writer's own leading
/// `[Name]` tag becomes the source. Nothing is hardcoded per Mod — whatever
/// name a loader or Mod prints is what the console shows, so new Mods are
/// attributed without a code change. A tag that is a level word or a
/// timestamp is not a source name.
fn known_external_scope(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix('[')?;
    let (tag, _) = rest.split_once(']')?;
    let tag = tag.trim();
    if tag.is_empty()
        || tag.chars().count() > 64
        || !tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ' '))
    {
        return None;
    }
    let upper = tag.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "INFO" | "WARN" | "ERROR" | "DEBUG" | "TRACE" | "FATAL" | "WARNING"
    ) {
        return None;
    }
    // A timestamp tag contains ':' or a date separator; a source name does not.
    if tag.contains(':') || tag.chars().any(|ch| ch.is_ascii_digit()) && tag.contains('-') {
        return None;
    }
    Some(format!("mod:{tag}"))
}

/// Returns `false` when the same captured line (timestamp + text) was already
/// forwarded inside the dedupe window. A native mod may print a line to the
/// console and append the identical line to `latest.log`; without this journal
/// the unified sinks would record it twice.
/// Normalizes one captured line into the dedupe key. The same logical line
/// arrives from different channels with different decorations: the console
/// copy may carry a leading `[Logger]` tag that the parsed copy already
/// stripped, and writers pad lines differently. Both differences are removed
/// so every channel maps to the same key.
fn captured_line_key(message: &str) -> String {
    let mut text = message.trim();
    if let Some(rest) = text.strip_prefix('[')
        && let Some((tag, tail)) = rest.split_once(']')
    {
        // A leading tag that is not a level word is the writer's own name; the
        // parsed copy of the same line has it stripped already.
        let upper = tag.trim().to_ascii_uppercase();
        if !matches!(upper.as_str(), "INFO" | "WARN" | "ERROR" | "DEBUG" | "TRACE") {
            text = tail.trim();
        }
    }
    text.to_string()
}

fn record_captured_line_once(record: &CapturedRecord) -> bool {
    let clock = record.timestamp_clock.get(..8).unwrap_or(&record.timestamp_clock);
    let key = format!(
        "{}|{}|{}",
        clock,
        record.scope.to_ascii_lowercase(),
        captured_line_key(&record.message)
    );
    let mut journal = EXTERNAL_DEDUPE
        .get_or_init(|| Mutex::new(DedupeJournal::default()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    if !journal.entries.contains_key(&key) {
        while journal.order.len() >= EXTERNAL_DEDUPE_LIMIT {
            if let Some(expired) = journal.order.pop_front() {
                journal.entries.remove(&expired);
            }
        }
        journal.order.push_back(key.clone());
    }
    let counts = journal.entries.entry(key).or_default();
    let index = record.channel.index();
    counts.by_channel[index] = counts.by_channel[index].saturating_add(1);
    if counts.by_channel[index] <= counts.emitted {
        return false;
    }
    counts.emitted = counts.by_channel[index];
    true
}

fn parse_external_structured_line(line: &str) -> Option<ExternalStructuredLine> {
    let first = line.strip_prefix('[')?;
    let first_end = first.find(']')?;
    let header = first[..first_end].trim();
    let level_token = header.split_whitespace().last()?;
    let level = external_level(level_token)?;

    let raw_timestamp = header.strip_suffix(level_token)?.trim();
    // Win32 console loggers commonly omit the date, while the file relay
    // includes it. Normalize both before the current-session cutoff check.
    let timestamp_full = if raw_timestamp.len() >= 8
        && raw_timestamp.as_bytes().get(2) == Some(&b':')
        && raw_timestamp.as_bytes().get(5) == Some(&b':')
    {
        format!("{} {raw_timestamp}", Local::now().format("%Y-%m-%d"))
    } else {
        raw_timestamp.to_string()
    };
    let timestamp_clock = timestamp_full
        .split_whitespace()
        .last()
        .map(str::to_string)
        .filter(|value| value.len() >= 8)
        .unwrap_or_else(|| current_console_timestamp().1);

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
        timestamp_full: timestamp_full.to_string(),
        timestamp_clock,
        level,
        source: source.to_string(),
        message,
    })
}

/// LeviLamina's native ConsoleSink defaults to
/// "{3:.3%T.} {2} {1} {0}" (local clock, level, logger, text).
/// Its file sink uses "[date time LEVEL][source] text", parsed separately.
/// Recognizing the console format allows preloaded stdout to be attributed
/// without native hooks, and preserves the source's original event time.
fn parse_levilamina_console_line(line: &str) -> Option<ExternalStructuredLine> {
    let (time, remainder) = line.trim_start().split_once(' ')?;
    let bytes = time.as_bytes();
    let valid_time = bytes.len() >= 8
        && bytes.get(2) == Some(&b':')
        && bytes.get(5) == Some(&b':')
        && bytes[0..2].iter().all(u8::is_ascii_digit)
        && bytes[3..5].iter().all(u8::is_ascii_digit)
        && bytes[6..8].iter().all(u8::is_ascii_digit)
        && (bytes.len() == 8
            || (bytes.get(8) == Some(&b'.')
                && bytes.len() > 9
                && bytes[9..].iter().all(u8::is_ascii_digit)));
    if !valid_time {
        return None;
    }
    let (level_label, remainder) = remainder.trim_start().split_once(' ')?;
    let level = external_level(level_label)?;
    let (source, message) = remainder.trim_start().split_once(' ')?;
    if source.is_empty()
        || source.len() > 64
        || !source.bytes().all(|c|
            c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')
        )
    {
        return None;
    }
    let timestamp_full = format!("{} {time}", Local::now().format("%Y-%m-%d"));
    Some(ExternalStructuredLine {
        timestamp_clock: time.to_string(),
        timestamp_full,
        level,
        source: source.to_string(),
        message: message.trim_start().to_string(),
    })
}

fn external_level(value: &str) -> Option<Level> {
    match value.trim().to_ascii_uppercase().as_str() {
        // Standard level names used by BLoader and most loggers.
        "ERROR" | "FATAL" | "ERR" => Some(Level::ERROR),
        "WARN" | "WARNING" | "WRN" => Some(Level::WARN),
        "INFO" | "INF" | "IFO" => Some(Level::INFO),
        "DEBUG" | "DBG" => Some(Level::DEBUG),
        "TRACE" | "TRC" => Some(Level::TRACE),
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

    // Windows components share the debug channel but are neither the game nor
    // a Mod; they only matter while debugging the capture chain itself.
    if scope == "system" {
        return CONSOLE_LEVEL.load(Ordering::Acquire) >= 4;
    }

    if level == Level::INFO && scope.starts_with("mod:") && message.trim().is_empty() {
        return false;
    }

    if level == Level::INFO && scope == "xuser-bridge" {
        if message.starts_with("XUser token/signature request")
            || message.starts_with("BMCBL XUser pipe payload")
            || message.starts_with("early XUser diagnostics replay complete")
            || message.starts_with("XUser Bridge 入口已执行 | protocol=")
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

    if level == Level::INFO {
        let loader_noise = scope == "loader"
            && (message.starts_with("[config] Loading configuration file from:")
                || message.starts_with("[config] Hot-reload file watcher active")
                || message.starts_with("Config applied |")
                || message.starts_with("i18n ready:")
                || message.starts_with("Minecraft symbol subsystem:")
                || message.starts_with("Runtime environment:")
                || message.starts_with("Host application:")
                || message.starts_with("Runtime profile:")
                || message.starts_with("Locale=")
                || message.starts_with("[file-redirection]")
                || message.starts_with("[net-hook]")
                || message.starts_with("Loading Mod:"));
        let bootstrap_noise = scope == "bootstrap"
            && message.starts_with("Immediate startup execution mode:");
        let premain_noise = scope == "premain-gate"
            && message.starts_with("Critical preload phase completed");
        let native_noise = scope == "native-loader"
            && (message.starts_with("LOAD_SUCCESS |") || message.starts_with("SUMMARY |"));
        let readiness_noise = scope == "runtime-ready"
            && message.starts_with("Delayed Mod readiness uses");

        if loader_noise || bootstrap_noise || premain_noise || native_noise || readiness_noise {
            return CONSOLE_LEVEL.load(Ordering::Acquire) >= 4;
        }
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

// Preserve millisecond timestamps in events/files but keep the interactive
// console compact. External log history may also supply fractional seconds.
fn compact_console_clock(timestamp: &str) -> &str {
    let clock = timestamp.split_whitespace().last().unwrap_or(timestamp);
    clock.get(..8).unwrap_or(clock)
}

fn format_console_line_at(timestamp: &str, level: Level, scope: &str, message: &str) -> String {
    let timestamp = compact_console_clock(timestamp);
    let level_text = level_label(level);
    let (primary, detail) = console_source_parts(scope);
    let primary = truncate(&primary, 28);
    let detail = detail.map(|value| truncate(&value, 28));
    let message = localized_console_message(scope, message)
        .replace('\r', "")
        .replace('\n', " | ");

    if !console_supports_ansi() {
        let mut source = format!("[{primary}]");
        if let Some(detail) = detail.as_deref() {
            source.push_str(&format!(" [{detail}]"));
        }
        return format!("[{timestamp} {level_text}] {source} {message}\r\n");
    }

    let level_color = match level {
        Level::ERROR => "\x1b[38;2;255;92;92m",
        Level::WARN => "\x1b[38;2;255;196;87m",
        Level::INFO => "\x1b[38;2;220;224;230m",
        Level::DEBUG => "\x1b[38;2;112;183;255m",
        Level::TRACE => "\x1b[38;2;128;134;145m",
    };
    let primary_color = source_color(scope, &primary);
    let mut source = format!("{primary_color}[{primary}]\x1b[0m");
    if let Some(detail) = detail.as_deref() {
        let detail_color = if primary == "BLoader" {
            primary_color
        } else {
            "\x1b[38;2;166;173;186m"
        };
        source.push_str(&format!(" {detail_color}[{detail}]\x1b[0m"));
    }

    format!(
        "\x1b[38;2;120;126;136m[{timestamp} \x1b[0m{level_color}{level_text}\x1b[38;2;120;126;136m]\x1b[0m {source} \x1b[38;2;220;224;230m{message}\x1b[0m\r\n"
    )
}

fn localized_console_message(scope: &str, message: &str) -> String {
    if message.starts_with("Priority preload detected | ") {
        let name = message
            .split('|')
            .nth(1)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("PreLoader");
        return i18n::tr("console.preloader.priority_detected").replace("{name}", name);
    }

    if message.starts_with("Priority preload active | ") {
        let elapsed_ms = message
            .split('|')
            .find_map(|part| part.trim().strip_prefix("elapsed="))
            .and_then(|value| value.strip_suffix("ms"))
            .map(str::trim)
            .unwrap_or("?");
        return i18n::tr("console.preloader.priority_active")
            .replace("{elapsed_ms}", elapsed_ms);
    }

    if message
        == "PreLoader and its delegated preload chain completed synchronously before Minecraft OEP execution."
    {
        return i18n::tr("console.preloader.chain_complete");
    }

    if message
        == "Windows Terminal bridge connection failed; switching to a visible classic console."
    {
        return i18n::tr("console.wt.connect_failed");
    }

    if message
        == "Windows Terminal bridge handshake timed out; switching to a visible classic console."
    {
        return i18n::tr("console.wt.timeout");
    }

    if scope == "xuser-bridge"
        && message.contains("未检测到 BMCBL 安全一次性管道")
        && message.contains("官方 XUser")
    {
        return i18n::tr("console.xuser.official");
    }

    if scope.starts_with("mod:") {
        if let Some(rest) = message.strip_prefix("Loaded ") {
            let version = rest.split(" (").next().unwrap_or(rest).trim();
            if i18n::current_locale().starts_with("zh") {
                return format!("已加载 {version}");
            }
            return format!("Loaded {version}");
        }
    }

    if message.starts_with("BLoader v") && message.contains(" initialized. ") {
        if let Some(version) = message
            .strip_prefix("BLoader v")
            .and_then(|value| value.split_whitespace().next())
        {
            if i18n::current_locale().starts_with("zh") {
                return format!("BLoader v{version} 已启动");
            }
            return format!("BLoader v{version} started");
        }
    }

    message.to_string()
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
        || crate::platform::mod_diagnostics::find_by_name(scope).is_some()
    {
        "\x1b[38;2;218;143;255m"
    } else {
        "\x1b[38;2;166;173;186m"
    }
}

fn console_source_parts(scope: &str) -> (String, Option<String>) {
    if let Some(name) = scope.strip_prefix("mod:") {
        return (name.to_string(), None);
    }
    if scope == "xuser-bridge" || scope.starts_with("xuser-") {
        return ("XUser".to_string(), None);
    }
    if matches!(scope, "game-stdio" | "game-log" | "minecraft") {
        return ("Minecraft".to_string(), None);
    }
    if scope == "system" {
        return ("System".to_string(), None);
    }
    if matches!(scope, "native-stdio" | "stdio-capture") {
        return ("StdIO".to_string(), None);
    }
    if scope == "preloader" {
        return ("PreLoader".to_string(), None);
    }
    if scope == "proxy" {
        return ("Proxy".to_string(), None);
    }
    if scope.contains("network") || scope.starts_with("net-") {
        return ("Network".to_string(), None);
    }
    if matches!(
        scope,
        "native-loader" | "runtime-ready" | "launch-info" | "console-capture" | "startup" | "xgameruntime"
    ) {
        return ("BLoader".to_string(), None);
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
        return ("BLoader".to_string(), None);
    }
    if crate::platform::mod_diagnostics::find_by_name(scope).is_some() {
        return (scope.to_string(), None);
    }
    let detail = scope.strip_prefix("BLoader/").unwrap_or(scope).trim();
    if detail.is_empty() {
        ("BLoader".to_string(), None)
    } else {
        ("BLoader".to_string(), Some(titlecase_module(detail)))
    }
}

fn titlecase_module(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut result = first.to_uppercase().collect::<String>();
    result.push_str(chars.as_str());
    result
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

/// Extracts the `message` field of a tracing event; every BLoader event logs
/// exactly one pre-formatted message that already embeds its scope tag.
struct EventMessageVisitor(String);

impl Visit for EventMessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

/// File-side formatter that renders tracing events in the same shape as the
/// unified console, but keeps the full local timestamp. This replaces the
/// default fmt output (UTC clock + ThreadId + file:line) that previously
/// leaked raw tracing lines into latest.log.
struct ConsoleFileFormat;

impl<S, N> FormatEvent<S, N> for ConsoleFileFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let mut visitor = EventMessageVisitor(String::new());
        event.record(&mut visitor);
        write!(
            writer,
            "[{}] [{}] {}\r\n",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            level_label(*event.metadata().level()),
            visitor.0
        )
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
    // Do not share LeviLamina's rotating logs/latest.log. Its FileSink may
    // replace the file while BLoader's tracing appender still has it open.
    let log_dir = PathBuf::from("logs").join("bloader");
    let _ = fs::create_dir_all(&log_dir);
    let path = log_dir.join("latest.log");
    if LATEST_PREPARED.set(()).is_ok() {
        let _ = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path);
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

// Captured stdio and per-Mod logs must not open and flush a file on every
// intercepted native print. Batch the two existing log destinations and Mod
// files through one bounded writer. Critical bootstrap/panic markers continue
// to use the synchronous emergency file path.
enum FileWrite {
    Main(Vec<u8>),
    Mod(PathBuf, Vec<u8>),
}

static FILE_SINK: OnceLock<Option<std::sync::mpsc::SyncSender<FileWrite>>> = OnceLock::new();
static FILE_SYNC_FALLBACKS: AtomicU64 = AtomicU64::new(0);
static FILE_BATCHES: AtomicU64 = AtomicU64::new(0);
const FILE_QUEUE_LIMIT: usize = 8192;
const FILE_BATCH_LIMIT: usize = 256;
const FILE_BATCH_WINDOW: Duration = Duration::from_millis(25);

fn write_mod_file(path: &Path, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(bytes);
    }
}

fn write_file_batch(commands: Vec<FileWrite>) {
    let mut main = Vec::<u8>::new();
    let mut mod_files = HashMap::<PathBuf, Vec<u8>>::new();
    for command in commands {
        match command {
            FileWrite::Main(bytes) => main.extend_from_slice(&bytes),
            FileWrite::Mod(path, bytes) => mod_files.entry(path).or_default().extend_from_slice(&bytes),
        }
    }
    if !main.is_empty() {
        write_bytes_to_bootstrap_file(&main);
    }
    for (path, bytes) in mod_files {
        write_mod_file(&path, &bytes);
    }
    FILE_BATCHES.fetch_add(1, Ordering::Relaxed);
}

fn file_sink() -> &'static Option<std::sync::mpsc::SyncSender<FileWrite>> {
    FILE_SINK.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<FileWrite>(FILE_QUEUE_LIMIT);
        match thread::Builder::new()
            .name("bloader-file-writer".to_string())
            .spawn(move || {
                while let Ok(first) = receiver.recv() {
                    let mut batch = Vec::with_capacity(FILE_BATCH_LIMIT);
                    batch.push(first);
                    let start = Instant::now();
                    while batch.len() < FILE_BATCH_LIMIT {
                        let Some(remaining) = FILE_BATCH_WINDOW.checked_sub(start.elapsed()) else {
                            break;
                        };
                        match receiver.recv_timeout(remaining) {
                            Ok(command) => batch.push(command),
                            Err(_) => break,
                        }
                    }
                    write_file_batch(batch);
                }
            }) {
            Ok(_) => Some(sender),
            Err(_) => None,
        }
    })
}

fn enqueue_file_write(command: FileWrite) {
    if !file_io_policy::writes_allowed() {
        return;
    }
    if let Some(sender) = file_sink() {
        match sender.try_send(command) {
            Ok(()) => return,
            Err(std::sync::mpsc::TrySendError::Full(command))
            | Err(std::sync::mpsc::TrySendError::Disconnected(command)) => {
                // File records are lossless even under severe overload. This
                // rare fallback may be synchronous; the ordinary hot path isn't.
                FILE_SYNC_FALLBACKS.fetch_add(1, Ordering::Relaxed);
                write_file_batch(vec![command]);
                return;
            }
        }
    }
    FILE_SYNC_FALLBACKS.fetch_add(1, Ordering::Relaxed);
    write_file_batch(vec![command]);
}

// All console I/O is owned by a single writer. A full queue discards only
// the live console copy, not the file record, and never blocks game threads.
struct ConsoleLine {
    timestamp_full: String,
    sequence: u64,
    level: Level,
    scope: String,
    message: String,
}

// The console is append-only during the session. Terminal hosts own resize
// and scrollback; no window event is allowed to clear/replay a partial history.
static CONSOLE_SINK: OnceLock<Option<std::sync::mpsc::SyncSender<ConsoleLine>>> = OnceLock::new();
static CONSOLE_DROPPED: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);
static CONSOLE_LATE_TOTAL: AtomicU64 = AtomicU64::new(0);
static CONSOLE_BATCHES: AtomicU64 = AtomicU64::new(0);
const CONSOLE_QUEUE_LIMIT: usize = 8192;
const CONSOLE_BATCH_LIMIT: usize = 128;
const CONSOLE_REORDER_WINDOW: Duration = Duration::from_millis(24);

/// A late file-relay record cannot rewind the already rendered console clock.
/// The true event time is preserved both in its file entry and a late marker.
fn console_display_time(actual: &str, last: &mut String) -> (String, bool) {
    let event_second = actual.get(..19).unwrap_or(actual);
    let last_second = last.get(..19).unwrap_or(last);
    if !last.is_empty() && event_second < last_second {
        let (wall, _) = current_console_timestamp();
        let display = if wall.as_str() > last.as_str() { wall } else { last.clone() };
        *last = display.clone();
        (display, true)
    } else {
        if actual > last.as_str() {
            *last = actual.to_string();
        }
        (last.clone(), false)
    }
}

fn append_console_line(
    line: ConsoleLine,
    output: &mut Vec<u8>,
    last_display: &mut String,
) {
    let (display_time, late) = console_display_time(&line.timestamp_full, last_display);
    if late {
        CONSOLE_LATE_TOTAL.fetch_add(1, Ordering::Relaxed);
    }
    let rendered = if late {
        let message = format!("{} [late occurred_at={}]", line.message, line.timestamp_full);
        format_console_line_at(&display_time, line.level, &line.scope, &message).into_bytes()
    } else {
        format_console_line_at(&display_time, line.level, &line.scope, &line.message).into_bytes()
    };
    output.extend_from_slice(&rendered);
}

fn console_sink() -> &'static Option<std::sync::mpsc::SyncSender<ConsoleLine>> {
    CONSOLE_SINK.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<ConsoleLine>(CONSOLE_QUEUE_LIMIT);
        match thread::Builder::new()
            .name("bloader-console-writer".to_string())
            .spawn(move || {
                let mut last_display = String::new();
                while let Ok(first) = receiver.recv() {
                    let mut lines = Vec::<ConsoleLine>::with_capacity(CONSOLE_BATCH_LIMIT);
                    lines.push(first);
                    let started = Instant::now();
                    while lines.len() < CONSOLE_BATCH_LIMIT {
                        let Some(remaining) = CONSOLE_REORDER_WINDOW.checked_sub(started.elapsed()) else {
                            break;
                        };
                        match receiver.recv_timeout(remaining) {
                            Ok(line) => lines.push(line),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    lines.sort_unstable_by(|a, b| {
                        a.timestamp_full.cmp(&b.timestamp_full)
                            .then_with(|| a.sequence.cmp(&b.sequence))
                    });
                    CONSOLE_BATCHES.fetch_add(1, Ordering::Relaxed);
                    let mut output = Vec::<u8>::with_capacity(lines.len() * 160);
                    for line in lines {
                        append_console_line(line, &mut output, &mut last_display);
                    }
                    let skipped = CONSOLE_DROPPED.swap(0, Ordering::Relaxed);
                    if skipped > 0 {
                        let (timestamp, _) = current_console_timestamp();
                        append_console_line(
                            ConsoleLine {
                                timestamp_full: timestamp,
                                sequence: CONSOLE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
                                level: Level::WARN,
                                scope: "logging".to_string(),
                                message: format!(
                                    "console busy: {skipped} live records omitted; {}",
                                    if file_io_policy::writes_allowed() {
                                        "full logs are in logs/bloader/latest.log"
                                    } else {
                                        "legacy UWP file logging is disabled"
                                    }
                                ),
                            },
                            &mut output, &mut last_display,
                        );
                    }
                    write_bytes_to_console_locked(&output);
                }
            }) {
            Ok(_) => Some(sender),
            Err(error) => {
                // Do not recursively call console_sink() while its OnceLock
                // initializer still owns the initialization state.
                let line = format!("logging.console.writer.spawn.failed error={error}");
                write_debug_string(&line);
                None
            }
        }
    })
}

fn write_console_record_at(timestamp_full: &str, level: Level, scope: &str, message: &str) {
    let Some(sender) = console_sink() else {
        CONSOLE_DROPPED.fetch_add(1, Ordering::Relaxed);
        CONSOLE_DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let line = ConsoleLine {
        timestamp_full: timestamp_full.to_string(),
        sequence: CONSOLE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        level,
        scope: scope.to_string(),
        message: message.to_string(),
    };
    if sender.try_send(line).is_err() {
        // Never fall back to synchronous WriteFile from a native hook.
        CONSOLE_DROPPED.fetch_add(1, Ordering::Relaxed);
        CONSOLE_DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
    }
}

fn write_bytes_to_console_locked(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }

    let Some(handle) = CONSOLE_HANDLE.get().map(|value| value.0) else {
        return;
    };
    if handle.is_invalid() {
        return;
    }

    crate::platform::console_capture::with_passthrough(|| {
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
    });
}

fn console_supports_ansi() -> bool {
    CONSOLE_FORCE_ANSI.load(Ordering::Acquire)
        || CONSOLE_ANSI_ENABLED.load(Ordering::Acquire)
}

fn query_console_ansi(handle: HANDLE) -> bool {
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
    crate::game::game_log::with_passthrough(|| unsafe {
        OutputDebugStringW(wide.as_ptr());
    });
}

#[cfg(test)]
mod compact_console_tests {
    use super::*;

    #[test]
    fn console_batch_keeps_every_record_without_clearing_scrollback() {
        let mut output = Vec::new();
        let mut last_display = String::new();
        // Simulate a long console session. Window movement/resize must never
        // truncate this output or insert terminal history erase commands.
        for index in 0..600 {
            append_console_line(
                ConsoleLine {
                    timestamp_full: format!("2026-11-11 14:32:{:02}.000", index / 60),
                    sequence: index,
                    level: Level::INFO,
                    scope: "loader".into(),
                    message: format!("record-{index:04}"),
                },
                &mut output,
                &mut last_display,
            );
        }
        let text = String::from_utf8(output).unwrap();
        for index in 0..600 {
            assert!(text.contains(&format!("record-{index:04}")));
        }
        assert!(!text.contains("\x1b[2J"));
        assert!(!text.contains("\x1b[3J"));
    }
    #[test]
    fn console_uses_second_precision_without_sequence() {
        assert_eq!(compact_console_clock("10:09:04.376"), "10:09:04");
        assert_eq!(compact_console_clock("10:09:04"), "10:09:04");

        let rendered = format_console_line_at(
            "10:09:04.376",
            Level::INFO,
            "mod:Cosmetics",
            "Native module loaded",
        );
        assert!(rendered.contains("10:09:04"));
        assert!(rendered.contains("[Cosmetics]"));
        assert!(!rendered.contains("10:09:04.376"));
        assert!(!rendered.contains("#000"));
    }
}

#[cfg(test)]
mod external_line_tests {
    use super::*;

    #[test]
    fn duplicate_channels_do_not_suppress_repeated_native_errors() {
        let clock = "19:03:25".to_string();
        let mut record = |channel| CapturedRecord {
            timestamp_full: "2026-11-09 19:03:25.000".into(),
            timestamp_clock: clock.clone(),
            level: Level::ERROR,
            scope: "mod:LeviLamina".into(),
            message: "mod init failed".into(),
            channel,
        };
        assert!(record_captured_line_once(&record(CaptureChannel::Console)));
        assert!(record_captured_line_once(&record(CaptureChannel::Console)));
        assert!(!record_captured_line_once(&record(CaptureChannel::FileRelay)));
        assert!(!record_captured_line_once(&record(CaptureChannel::FileRelay)));
        assert!(record_captured_line_once(&record(CaptureChannel::FileRelay)));
    }

    #[test]
    fn console_late_records_do_not_rewind_clock() {
        let mut last = "2026-11-09 19:03:26.000".to_string();
        let (display, late) = console_display_time("2026-11-09 19:03:25.600", &mut last);
        assert!(late);
        assert!(display.as_str() >= "2026-11-09 19:03:26.000");
        assert_eq!(last, display);

        let (display, late) = console_display_time("2026-11-09 19:03:27.000", &mut last);
        assert!(!late || display.as_str() >= "2026-11-09 19:03:27.000");
    }

    #[test]
    fn short_external_timestamps_are_kept_during_first_second() {
        // The old lexicographic comparison rejected an equal-second header
        // with no millisecond suffix even though it was from this process.
        let event = "2026-11-09 19:03:25";
        let start = "2026-11-09 19:03:25.123";
        assert!(external_timestamp_at_or_after(event, start));
        assert!(!external_timestamp_at_or_after("2026-11-09 19:03:24", start));
    }

    #[test]
    fn native_three_letter_levels_are_recognized() {
        assert_eq!(external_level("IFO"), Some(Level::INFO));
        assert_eq!(external_level("INF"), Some(Level::INFO));
        assert_eq!(external_level("WRN"), Some(Level::WARN));
        assert_eq!(external_level("ERR"), Some(Level::ERROR));
        assert_eq!(external_level("DBG"), Some(Level::DEBUG));
        assert_eq!(external_level("TRC"), Some(Level::TRACE));
        assert_eq!(external_level("INFO"), Some(Level::INFO));
        assert_eq!(external_level("WARNING"), Some(Level::WARN));
        assert_eq!(external_level("bogus"), None);
    }

    #[test]
    fn parses_native_levilamina_console_sink_without_win32_detours() {
        let raw = "19:03:25.123 INFO LeviLamina Loading BedrockSceneCapture v0.2.0";
        let record = parse_levilamina_console_line(raw).expect("LeviLamina console");
        assert_eq!(record.source, "LeviLamina");
        assert_eq!(record.timestamp_clock, "19:03:25.123");
        assert_eq!(record.level, Level::INFO);
        assert_eq!(record.message, "Loading BedrockSceneCapture v0.2.0");
        assert!(record.timestamp_full.ends_with(" 19:03:25.123"));
        assert!(parse_levilamina_console_line("Minecraft generic console output").is_none());
        assert!(parse_levilamina_console_line("19:03:25.123 BOGUS LeviLamina something").is_none());
        assert!(parse_levilamina_console_line("中19:03:25 INFO LeviLamina x").is_none());
        assert!(parse_levilamina_console_line("19:03:25. INFO LeviLamina x").is_none());
    }

    #[test]
    fn console_short_timestamp_is_normalized() {
        let parsed = parse_external_structured_line(
            "[19:03:25 INFO] [LeviLamina] Loading BedrockSceneCapture"
        ).expect("short console timestamp");
        assert!(parsed.timestamp_full.ends_with(" 19:03:25"));
        assert_eq!(parsed.timestamp_clock, "19:03:25");
        assert_eq!(parsed.source, "LeviLamina");
    }

    #[test]
    fn native_levilamina_line_parses_and_renders_unified() {
        let line = "[2026-10-30 20:25:10.932 IFO][LeviLamina] test message";
        let parsed = parse_external_structured_line(line).expect("native line");
        assert_eq!(parsed.level, Level::INFO);
        assert_eq!(parsed.source, "LeviLamina");
        assert_eq!(parsed.timestamp_full, "2026-10-30 20:25:10.932");
        assert_eq!(parsed.timestamp_clock, "20:25:10.932");
        assert_eq!(parsed.message, "test message");
    }
}

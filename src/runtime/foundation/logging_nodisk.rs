use std::io::{self, Write};
use std::sync::OnceLock;

use chrono::Local;
use tracing::{Level, debug, error, info, trace, warn};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::WriteFile;
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ENABLE_WRAP_AT_EOL_OUTPUT, GetConsoleMode, SetConsoleMode,
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

#[derive(Clone, Copy)]
struct LiveMakeWriter;
struct LiveWriter;

impl<'a> fmt::MakeWriter<'a> for LiveMakeWriter {
    type Writer = LiveWriter;
    fn make_writer(&'a self) -> Self::Writer {
        LiveWriter
    }
}

impl Write for LiveWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        write_bytes_to_console(bytes);
        if let Ok(text) = std::str::from_utf8(bytes) {
            write_debug_string(text);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn init(level: &str) {
    if LOGGING_READY.get().is_some() {
        return;
    }
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    let layer = fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(LiveMakeWriter)
        .with_span_events(FmtSpan::NONE)
        .with_filter(filter);
    let _ = tracing_subscriber::registry().with(layer).try_init();
    let _ = LOGGING_READY.set(());
    write_bootstrap_marker(&format!(
        "logging.init.ready level={level} sink=console+debug no_disk=true"
    ));
}

pub fn is_ready() -> bool {
    LOGGING_READY.get().is_some()
}

pub fn captured_mod_output(mod_name: &str, _mod_id: &str, stream: &str, message: &str) {
    log_message(Level::INFO, mod_name, &format!("{stream} | {message}"));
}

pub fn captured_process_output(stream: &str, message: &str) {
    log_message(
        Level::INFO,
        "native-stdio",
        &format!("{stream} | {message}"),
    );
}

pub fn set_console_handle(handle: HANDLE) {
    let _ = CONSOLE_HANDLE.set(SendHandle(handle));
    enable_console_ansi(handle);
    write_bootstrap_marker(&format!(
        "logging.console.ready handle=0x{:X}",
        handle.0 as usize
    ));
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
            "{loader_name} v{loader_version} | host={application_name} v{application_version} | locale={locale} | diagnostics=memory-only"
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
pub fn emergency_error_message(scope: &str, message: &str) {
    emergency_log_message(Level::ERROR, scope, message);
}
pub fn emergency_warn_message(scope: &str, message: &str) {
    emergency_log_message(Level::WARN, scope, message);
}
pub fn emergency_info_message(scope: &str, message: &str) {
    emergency_log_message(Level::INFO, scope, message);
}

pub fn write_bootstrap_marker(message: &str) {
    let line = format!(
        "[{}] [BOOT] {}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        message
    );
    write_debug_string(&line);
    write_bytes_to_console(line.as_bytes());
}

fn log_message(level: Level, scope: &str, message: &str) {
    if is_ready() {
        emit_event(level, scope, message);
    } else {
        emergency_log_message(level, scope, message);
    }
}

fn emergency_log_message(level: Level, scope: &str, message: &str) {
    let line = format!(
        "[{}] [{}] [{}] {}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        level_label(level),
        scope,
        message
    );
    write_bytes_to_console(line.as_bytes());
    write_debug_string(&line);
}

fn emit_event(level: Level, scope: &str, message: &str) {
    let scoped = format!("[{scope}] {message}");
    match level {
        Level::ERROR => error!(target: "runtime", "{scoped}"),
        Level::WARN => warn!(target: "runtime", "{scoped}"),
        Level::INFO => info!(target: "runtime", "{scoped}"),
        Level::DEBUG => debug!(target: "runtime", "{scoped}"),
        Level::TRACE => trace!(target: "runtime", "{scoped}"),
    }
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

fn write_bytes_to_console(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let Some(handle) = CONSOLE_HANDLE.get().map(|v| v.0) else {
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

fn enable_console_ansi(handle: HANDLE) {
    if handle.is_invalid() {
        return;
    }
    unsafe {
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(handle, &mut mode).is_ok() {
            let _ = SetConsoleMode(
                handle,
                mode | ENABLE_PROCESSED_OUTPUT
                    | ENABLE_WRAP_AT_EOL_OUTPUT
                    | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            );
        }
    }
}

fn write_debug_string(line: &str) {
    let wide: Vec<u16> = line.encode_utf16().chain(Some(0)).collect();
    unsafe {
        OutputDebugStringW(wide.as_ptr());
    }
}

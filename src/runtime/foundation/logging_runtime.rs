#[path = "logging.rs"]
mod inner;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

pub use inner::{
    captured_mod_output, captured_process_output, console_is_ready, debug_message,
    emergency_error_message, emergency_info_message, emergency_warn_message, error_message,
    is_ready, replay_console_message, scoped_debug_message, scoped_error_message,
    scoped_trace_message, scoped_warn_message, set_console_handle, set_console_stream_handle,
    startup_banner, trace_message, warn_message,
};

const DEFAULT_ARCHIVE_RETENTION_DAYS: u32 = 7;
const MAX_ARCHIVE_RETENTION_DAYS: u32 = 3650;
const BOOTSTRAP_BACKLOG_LIMIT: usize = 512;

static ARCHIVE_RETENTION_DAYS: AtomicU32 = AtomicU32::new(DEFAULT_ARCHIVE_RETENTION_DAYS);
static EFFECTIVE_LEVEL: AtomicU8 = AtomicU8::new(3);
static LOGGING_CONFIGURED: AtomicBool = AtomicBool::new(false);
static BOOTSTRAP_BACKLOG: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn bootstrap_backlog() -> &'static Mutex<Vec<String>> {
    BOOTSTRAP_BACKLOG.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn set_archive_retention_days(days: u32) {
    ARCHIVE_RETENTION_DAYS.store(normalize_archive_days(days), Ordering::Release);
    if inner::is_ready() {
        let _ = prune_archive_logs();
    }
}

pub fn init(configured_level: &str) {
    let (effective_level, valid) = normalize_level(configured_level);
    let level_rank = level_rank(effective_level);
    EFFECTIVE_LEVEL.store(level_rank, Ordering::Release);
    inner::set_console_level(effective_level);

    cleanup_legacy_diagnostic_artifacts();
    let pruned = prune_archive_logs().unwrap_or(0);
    inner::init(effective_level);
    LOGGING_CONFIGURED.store(true, Ordering::Release);

    let pending = {
        let mut pending = bootstrap_backlog()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        std::mem::take(&mut *pending)
    };
    if level_rank >= 4 {
        for marker in pending {
            inner::write_bootstrap_marker(&marker);
        }
    }

    if !valid {
        inner::warn_message(&format!(
            "[logging] invalid log_level={configured_level}; using info"
        ));
    }
    if pruned != 0 {
        inner::scoped_debug_message(
            "logging",
            &format!(
                "archive cleanup removed {pruned} expired BLoader log file(s) | retention_days={}",
                ARCHIVE_RETENTION_DAYS.load(Ordering::Acquire)
            ),
        );
    }

    crate::runtime::foundation::startup_diagnostics::emit(configured_level, effective_level);
}

pub fn write_bootstrap_marker(message: &str) {
    if LOGGING_CONFIGURED.load(Ordering::Acquire) {
        if EFFECTIVE_LEVEL.load(Ordering::Acquire) >= 4 {
            inner::write_bootstrap_marker(message);
        }
        return;
    }

    let mut pending = bootstrap_backlog()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if pending.len() < BOOTSTRAP_BACKLOG_LIMIT {
        pending.push(message.to_string());
    }
}

pub fn info_message(message: &str) {
    if loader_info_is_debug(message) {
        inner::debug_message(message);
    } else {
        inner::info_message(message);
    }
}

pub fn scoped_info_message(scope: &str, message: &str) {
    if scoped_info_is_debug(scope, message) {
        inner::scoped_debug_message(scope, message);
    } else {
        inner::scoped_info_message(scope, message);
    }
}

fn normalize_level(value: &str) -> (&'static str, bool) {
    match value.trim().to_ascii_lowercase().as_str() {
        "error" => ("error", true),
        "warn" | "warning" => ("warn", true),
        "info" => ("info", true),
        "debug" => ("debug", true),
        "trace" => ("trace", true),
        _ => ("info", false),
    }
}

fn level_rank(value: &str) -> u8 {
    match value {
        "error" => 1,
        "warn" => 2,
        "debug" => 4,
        "trace" => 5,
        _ => 3,
    }
}

fn normalize_archive_days(days: u32) -> u32 {
    days.clamp(1, MAX_ARCHIVE_RETENTION_DAYS)
}

fn cleanup_legacy_diagnostic_artifacts() {
    if !crate::runtime::foundation::file_io_policy::writes_allowed() {
        return;
    }

    for path in [
        PathBuf::from("logs").join("mod-registry.json"),
        PathBuf::from("logs").join("native-load-status.json"),
        PathBuf::from("logs").join("native-load-status.json.tmp"),
        PathBuf::from("logs").join("preloader-status.json"),
    ] {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_dir_all(PathBuf::from("logs").join("captured-stdio"));
}

fn prune_archive_logs() -> std::io::Result<usize> {
    if !crate::runtime::foundation::file_io_policy::writes_allowed() {
        return Ok(0);
    }

    let archive_dir = PathBuf::from("logs").join("archive");
    let Ok(entries) = fs::read_dir(&archive_dir) else {
        return Ok(0);
    };
    let max_age = Duration::from_secs(
        u64::from(ARCHIVE_RETENTION_DAYS.load(Ordering::Acquire)).saturating_mul(86_400),
    );
    let now = SystemTime::now();
    let mut removed = 0usize;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with("bloader-") || !name.ends_with(".log") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age >= max_age && fs::remove_file(&path).is_ok() {
            removed = removed.saturating_add(1);
        }
    }

    Ok(removed)
}

fn loader_info_is_debug(message: &str) -> bool {
    message.starts_with("[config] Loading configuration file from:")
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
        || message.starts_with("Loading Mod:")
}

fn scoped_info_is_debug(scope: &str, message: &str) -> bool {
    if scope == "xuser-bridge"
        && (message.starts_with("XUser token/signature request")
            || message.starts_with("BMCBL XUser pipe payload")
            || message.starts_with("early XUser diagnostics replay complete")
            || message.starts_with("XUser Bridge 入口已执行 | protocol="))
    {
        return true;
    }

    if matches!(scope, "native-stdio" | "stdio-capture")
        && message.starts_with("process stdio capture installed")
    {
        return true;
    }

    (scope == "bootstrap" && message.starts_with("Immediate startup execution mode:"))
        || (scope == "premain-gate"
            && message.starts_with("Critical preload phase completed"))
        || (scope == "native-loader"
            && (message.starts_with("LOAD_SUCCESS |") || message.starts_with("SUMMARY |")))
        || (scope == "runtime-ready" && message.starts_with("Delayed Mod readiness uses"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_archive_retention_to_safe_range() {
        assert_eq!(normalize_archive_days(0), 1);
        assert_eq!(normalize_archive_days(7), 7);
        assert_eq!(normalize_archive_days(u32::MAX), MAX_ARCHIVE_RETENTION_DAYS);
    }

    #[test]
    fn normal_log_level_is_not_promoted_to_debug() {
        assert_eq!(normalize_level("info"), ("info", true));
        assert_eq!(normalize_level("debug"), ("debug", true));
        assert_eq!(normalize_level("invalid"), ("info", false));
    }
}

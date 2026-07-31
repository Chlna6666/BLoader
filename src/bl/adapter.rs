#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::bl::events;
use crate::runtime::foundation::logging;

#[derive(Default)]
struct AdapterState {
    last_world_key: Option<String>,
    last_world_name: Option<String>,
    last_world_source: Option<String>,
    runtime_world_name: Option<String>,
    runtime_world_source: Option<String>,
    last_world_activity_at: Option<Instant>,
    last_save_activity_at: Option<Instant>,
    last_escape_at: Option<Instant>,
    save_status: Option<String>,
    save_last_path: Option<String>,
    line_buffers: HashMap<String, Vec<u8>>,
}

static STATE: OnceLock<Mutex<AdapterState>> = OnceLock::new();
const WORLD_ACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_SAVE_RESET_TIMEOUT: Duration = Duration::from_secs(3);
const EXIT_ESCAPE_WINDOW: Duration = Duration::from_secs(15);

fn state() -> &'static Mutex<AdapterState> {
    STATE.get_or_init(|| Mutex::new(AdapterState::default()))
}

pub fn observe_file_open(path: &Path) {
    let normalized = normalize(path);

    if let Some((world_key, world_root)) = extract_world_key_and_root(&normalized) {
        let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
        guard.last_world_activity_at = Some(Instant::now());
        if guard.last_world_key.as_deref() == Some(&world_key) {
            return;
        }
        guard.last_world_key = Some(world_key.clone());
        let world_name = read_world_name(&world_root).unwrap_or_else(|| world_key.clone());
        guard.last_world_name = Some(world_name.clone());
        guard.last_world_source = Some("world_fs_access".to_string());
        drop(guard);
        logging::info_message(&format!(
            "[BL] World adapter resolved world: {} ({})",
            world_name, world_key
        ));
        events::emit_world_enter(&world_name, "world_fs_access");
    }
}

pub fn current_world_name() -> Option<String> {
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    guard
        .runtime_world_name
        .clone()
        .or_else(|| guard.last_world_name.clone())
}

pub fn current_world_source() -> Option<String> {
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    guard
        .runtime_world_source
        .clone()
        .or_else(|| guard.last_world_source.clone())
}

pub fn update_runtime_world(world_name: &str, source: &str) {
    if world_name.trim().is_empty() {
        return;
    }

    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    let changed = guard.runtime_world_name.as_deref() != Some(world_name);
    guard.runtime_world_name = Some(world_name.to_string());
    guard.runtime_world_source = Some(source.to_string());
    guard.last_world_activity_at = Some(Instant::now());
    drop(guard);

    if changed {
        logging::info_message(&format!(
            "[BL] Runtime world resolved: {} ({})",
            world_name, source
        ));
        events::emit_world_enter(world_name, source);
    }
}

pub fn refresh_runtime_world_from_level_scan() {
    let already_known = {
        let guard = state().lock().unwrap_or_else(|e| e.into_inner());
        guard.runtime_world_name.is_some() || guard.last_world_name.is_some()
    };
    if already_known {
        return;
    }

    // MC code removed
}

pub fn observe_file_write(path: &Path, bytes: &[u8]) {
    note_world_activity(path);
    note_save_activity(path, "write");

    if bytes.is_empty() || !looks_like_log_path(path) {
        return;
    }

    let key = normalize(path);
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    let buf = guard.line_buffers.entry(key.clone()).or_default();
    buf.extend_from_slice(bytes);
    let mut lines = Vec::new();
    while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(&buf[..pos])
            .trim()
            .trim_end_matches('\r')
            .to_string();
        buf.drain(..=pos);
        lines.push(line);
    }
    drop(guard);

    for line in lines {
        process_log_line(&key, &line);
    }
}

pub fn prune_stale_world_state() {
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if guard.runtime_world_name.is_some() {
        return;
    }

    let save_status = guard.save_status.clone().unwrap_or_default();
    let escape_elapsed = guard.last_escape_at.map(|at| at.elapsed());
    let save_elapsed = guard.last_save_activity_at.map(|at| at.elapsed());
    let exit_like_save = save_status.eq_ignore_ascii_case("rename:level.dat")
        || save_status.eq_ignore_ascii_case("rename:current");
    if exit_like_save
        && escape_elapsed.is_some_and(|elapsed| elapsed <= EXIT_ESCAPE_WINDOW)
        && save_elapsed.is_some_and(|elapsed| elapsed >= EXIT_SAVE_RESET_TIMEOUT)
    {
        let stale_name = guard.last_world_name.clone().unwrap_or_default();
        let stale_source = guard
            .last_world_source
            .clone()
            .unwrap_or_else(|| "world_fs_access".to_string());
        drop(guard);

        logging::info_message(&format!(
            "[BL] Cleared world state after exit-save confirmation: {} ({})",
            if stale_name.is_empty() {
                "<unknown>"
            } else {
                &stale_name
            },
            stale_source
        ));
        events::reset_world_state();
        return;
    }

    let Some(last_activity) = guard.last_world_activity_at else {
        return;
    };
    if last_activity.elapsed() < WORLD_ACTIVITY_TIMEOUT {
        return;
    }

    let stale_name = guard.last_world_name.clone().unwrap_or_default();
    let stale_source = guard
        .last_world_source
        .clone()
        .unwrap_or_else(|| "world_fs_access".to_string());
    drop(guard);

    logging::info_message(&format!(
        "[BL] Cleared stale world state: {} ({}) after {}s inactivity",
        if stale_name.is_empty() {
            "<unknown>"
        } else {
            &stale_name
        },
        stale_source,
        WORLD_ACTIVITY_TIMEOUT.as_secs()
    ));
    events::reset_world_state();
}

pub fn observe_file_rename(path_text: &str) {
    let path = PathBuf::from(path_text);
    note_world_activity(&path);
    note_save_activity(&path, "rename");
}

pub fn notify_leave_world(source: &str) {
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    let world_name = guard
        .runtime_world_name
        .clone()
        .or_else(|| guard.last_world_name.clone())
        .unwrap_or_else(|| "<unknown>".to_string());
    drop(guard);
    logging::info_message(&format!(
        "[BL] Leave-world detected via {} for {}",
        source, world_name
    ));
}

pub fn clear_world_state() {
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    guard.last_world_key = None;
    guard.last_world_name = None;
    guard.last_world_source = None;
    guard.runtime_world_name = None;
    guard.runtime_world_source = None;
    guard.last_world_activity_at = None;
    guard.last_save_activity_at = None;
    guard.last_escape_at = None;
    guard.save_status = None;
    guard.save_last_path = None;
}

pub fn notify_escape_pressed() {
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    guard.last_escape_at = Some(Instant::now());
}

pub fn save_status() -> String {
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    guard.save_status.clone().unwrap_or_default()
}

pub fn save_last_path() -> String {
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    guard.save_last_path.clone().unwrap_or_default()
}

fn process_log_line(source_path: &str, line: &str) {
    if line.is_empty() {
        return;
    }

    let lower = line.to_ascii_lowercase();

    // Conservative parsing: only emit when the line very likely contains chat-like content.
    if let Some((author, message)) = parse_angle_bracket_chat(line) {
        events::emit_chat(&author, &message, "content_log");
        return;
    }

    if lower.contains("chat") && lower.contains("message") {
        events::emit_chat("system", line, "content_log");
        return;
    }

    if lower.contains("chat.type.text") || lower.contains("textbox") || lower.contains("saycommand")
    {
        events::emit_chat("system", line, "content_log");
        return;
    }

    logging::debug_message(&format!(
        "[BL] Ignored log line from {}: {}",
        source_path, line
    ));
}

fn parse_angle_bracket_chat(line: &str) -> Option<(String, String)> {
    let start = line.find('<')?;
    let end = line[start + 1..].find('>')? + start + 1;
    let author = line[start + 1..end].trim();
    let message = line[end + 1..].trim();
    if author.is_empty() || message.is_empty() {
        return None;
    }
    Some((author.to_string(), message.to_string()))
}

fn looks_like_log_path(path: &Path) -> bool {
    let normalized = normalize(path);
    normalized.contains("\\logs\\")
        || normalized.ends_with("\\contentlog.txt")
        || normalized.ends_with("\\content_log.txt")
}

fn note_world_activity(path: &Path) {
    let normalized = normalize(path);
    if extract_world_key_and_root(&normalized).is_none() {
        return;
    }

    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    guard.last_world_activity_at = Some(Instant::now());
}

fn note_save_activity(path: &Path, action: &str) {
    let normalized = normalize(path);
    if extract_world_key_and_root(&normalized).is_none() {
        return;
    }
    if !normalized.ends_with("\\level.dat")
        && !normalized.ends_with("\\level.dat_old")
        && !normalized.ends_with("\\db\\current")
        && !normalized.ends_with("\\db\\current.bak")
    {
        return;
    }

    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    guard.last_world_activity_at = Some(Instant::now());
    guard.last_save_activity_at = Some(Instant::now());
    guard.save_status = Some(format!(
        "{action}:{}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    guard.save_last_path = Some(path.to_string_lossy().to_string());
}

fn extract_world_key_and_root(path: &str) -> Option<(String, PathBuf)> {
    let marker = "\\games\\com.mojang\\minecraftworlds\\";
    let idx = path.find(marker)?;
    let tail = &path[idx + marker.len()..];
    let world_key = tail.split('\\').next()?.trim();
    if world_key.is_empty() {
        return None;
    }

    let trigger = [
        "\\levelname.txt",
        "\\level.dat",
        "\\db\\current",
        "\\db\\current.bak",
    ];
    if !trigger.iter().any(|suffix| path.ends_with(suffix)) {
        return None;
    }

    let original = PathBuf::from(path);
    let world_root = original
        .ancestors()
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().eq_ignore_ascii_case(world_key))
                .unwrap_or(false)
        })?
        .to_path_buf();

    Some((world_key.to_string(), world_root))
}

fn read_world_name(world_root: &Path) -> Option<String> {
    let level_name = world_root.join("levelname.txt");
    let text = std::fs::read_to_string(level_name).ok()?;
    let name = text.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

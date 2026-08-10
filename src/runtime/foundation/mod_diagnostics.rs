use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::Local;
use serde::Serialize;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
use windows::Win32::System::Threading::GetCurrentThreadId;

use crate::runtime::foundation::file_io_policy;

#[derive(Clone, Debug, Serialize)]
pub struct ModIdentity {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub kind: String,
    pub dll_path: String,
    pub aliases: Vec<String>,
    pub module_handle: Option<String>,
    pub state: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct LifecycleEvent {
    pub sequence: u64,
    pub timestamp: String,
    pub thread_id: u32,
    pub mod_id: String,
    pub mod_name: String,
    pub phase: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
struct ActiveScope {
    token: u64,
    identity: ModIdentity,
    phase: String,
    thread_id: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActiveScopeSnapshot {
    pub identity: ModIdentity,
    pub phase: String,
    pub thread_id: u32,
}

pub struct ModScopeGuard {
    token: u64,
}

static MODS: OnceLock<Mutex<Vec<ModIdentity>>> = OnceLock::new();
static EVENTS: OnceLock<Mutex<VecDeque<LifecycleEvent>>> = OnceLock::new();
static ACTIVE_SCOPES: OnceLock<Mutex<Vec<ActiveScope>>> = OnceLock::new();
static NEXT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static NEXT_SCOPE_TOKEN: AtomicU64 = AtomicU64::new(1);

fn mods() -> &'static Mutex<Vec<ModIdentity>> {
    MODS.get_or_init(|| Mutex::new(Vec::new()))
}

fn events() -> &'static Mutex<VecDeque<LifecycleEvent>> {
    EVENTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn active_scopes() -> &'static Mutex<Vec<ActiveScope>> {
    ACTIVE_SCOPES.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn default_aliases(id: &str, name: &str, dll_path: &Path, extra: &[String]) -> Vec<String> {
    let mut aliases = Vec::new();
    for value in [Some(id), Some(name), dll_path.file_stem().and_then(|value| value.to_str())]
        .into_iter()
        .flatten()
        .chain(extra.iter().map(String::as_str))
    {
        let normalized = value.trim();
        if normalized.is_empty()
            || aliases
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(normalized))
        {
            continue;
        }
        aliases.push(normalized.to_string());
    }
    aliases
}

pub fn register_discovered(
    id: impl Into<String>,
    name: impl Into<String>,
    version: Option<String>,
    kind: impl Into<String>,
    dll_path: &Path,
    aliases: Vec<String>,
) -> ModIdentity {
    let id = id.into();
    let name = name.into();
    let kind = kind.into();
    let canonical = canonical_text(dll_path);
    let aliases = default_aliases(&id, &name, dll_path, &aliases);
    let mut registry = mods().lock().unwrap_or_else(|error| error.into_inner());

    if let Some(existing) = registry
        .iter_mut()
        .find(|item| paths_equal_text(&item.dll_path, &canonical))
    {
        existing.id = id;
        existing.name = name;
        existing.version = version;
        existing.kind = kind;
        existing.aliases = aliases;
        return existing.clone();
    }

    let identity = ModIdentity {
        id,
        name,
        version,
        kind,
        dll_path: canonical,
        aliases,
        module_handle: None,
        state: "discovered".to_string(),
    };
    registry.push(identity.clone());
    drop(registry);
    record_lifecycle(&identity, "discovered", "manifest accepted");
    identity
}

pub fn mark_loading(identity: &ModIdentity, phase: &str) {
    update_identity(identity, None, "loading");
    record_lifecycle(identity, "load_begin", phase);
}

pub fn mark_loaded(identity: &ModIdentity, module: usize, phase: &str) -> ModIdentity {
    let handle = format!("0x{module:X}");
    let updated = update_identity(identity, Some(handle), "loaded");
    record_lifecycle(&updated, "load_success", phase);
    let _ = write_registry_snapshot();
    updated
}

pub fn mark_failed(identity: &ModIdentity, phase: &str, detail: &str) {
    update_identity(identity, None, "failed");
    record_lifecycle(identity, "load_failed", &format!("{phase}: {detail}"));
    let _ = write_registry_snapshot();
}

fn update_identity(identity: &ModIdentity, module_handle: Option<String>, state: &str) -> ModIdentity {
    let mut registry = mods().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(existing) = registry
        .iter_mut()
        .find(|item| paths_equal_text(&item.dll_path, &identity.dll_path))
    {
        if module_handle.is_some() {
            existing.module_handle = module_handle;
        }
        existing.state = state.to_string();
        return existing.clone();
    }

    let mut inserted = identity.clone();
    inserted.module_handle = module_handle;
    inserted.state = state.to_string();
    registry.push(inserted.clone());
    inserted
}

pub fn enter_scope(identity: &ModIdentity, phase: impl Into<String>) -> ModScopeGuard {
    let token = NEXT_SCOPE_TOKEN.fetch_add(1, Ordering::Relaxed);
    let phase = phase.into();
    let thread_id = unsafe { GetCurrentThreadId() };
    active_scopes()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(ActiveScope {
            token,
            identity: identity.clone(),
            phase: phase.clone(),
            thread_id,
        });
    record_lifecycle(identity, "scope_enter", &phase);
    ModScopeGuard { token }
}

pub fn with_scope<T>(identity: &ModIdentity, phase: impl Into<String>, f: impl FnOnce() -> T) -> T {
    let _guard = enter_scope(identity, phase);
    f()
}

impl Drop for ModScopeGuard {
    fn drop(&mut self) {
        let removed = {
            let mut scopes = active_scopes()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            scopes
                .iter()
                .position(|scope| scope.token == self.token)
                .map(|index| scopes.remove(index))
        };
        if let Some(scope) = removed {
            record_lifecycle(&scope.identity, "scope_exit", &scope.phase);
        }
    }
}

pub fn active_scope_for_thread(thread_id: u32) -> Option<ActiveScopeSnapshot> {
    let scopes = active_scopes().try_lock().ok()?;
    scopes
        .iter()
        .rev()
        .find(|scope| scope.thread_id == thread_id)
        .or_else(|| (scopes.len() == 1).then(|| &scopes[0]))
        .map(|scope| ActiveScopeSnapshot {
            identity: scope.identity.clone(),
            phase: scope.phase.clone(),
            thread_id: scope.thread_id,
        })
}

pub fn mark_crashed(identity: &ModIdentity, phase: &str, detail: &str) {
    let updated = update_identity(identity, None, "crashed");
    record_lifecycle(&updated, "crash", &format!("{phase}: {detail}"));
    let _ = write_registry_snapshot();
}

pub fn all_mods() -> Vec<ModIdentity> {
    mods()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

pub fn active_identity() -> Option<ModIdentity> {
    let scopes = active_scopes()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if scopes.len() == 1 {
        return scopes.first().map(|scope| scope.identity.clone());
    }

    let current_thread = unsafe { GetCurrentThreadId() };
    let mut matching = scopes
        .iter()
        .filter(|scope| scope.thread_id == current_thread)
        .map(|scope| scope.identity.clone());
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

pub fn active_context_text() -> String {
    let Ok(scopes) = active_scopes().try_lock() else {
        return "<active-scope-lock-unavailable>".to_string();
    };
    if scopes.is_empty() {
        return "<none>".to_string();
    }
    scopes
        .iter()
        .map(|scope| {
            format!(
                "{} ({}) phase={} thread={}",
                scope.identity.name, scope.identity.id, scope.phase, scope.thread_id
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

pub fn resolve_output_owner(line: &str) -> Option<ModIdentity> {
    if let Some(active) = active_identity() {
        return Some(active);
    }

    let prefix = bracket_prefix(line).or_else(|| token_prefix(line));
    let registry = mods().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(prefix) = prefix {
        if let Some(identity) = registry.iter().find(|identity| {
            identity
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(prefix))
        }) {
            return Some(identity.clone());
        }
    }

    let lower = line.to_ascii_lowercase();
    let mut candidates = registry
        .iter()
        .filter(|identity| {
            identity.aliases.iter().any(|alias| {
                alias.len() >= 4 && lower.contains(&alias.to_ascii_lowercase())
            })
        })
        .cloned();
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

pub fn find_by_name(name: &str) -> Option<ModIdentity> {
    mods()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .find(|identity| {
            identity.name.eq_ignore_ascii_case(name)
                || identity.id.eq_ignore_ascii_case(name)
                || identity
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name))
        })
        .cloned()
}

pub fn find_by_module_path(path: &str) -> Option<ModIdentity> {
    mods()
        .try_lock()
        .ok()?
        .iter()
        .find(|identity| paths_equal_text(&identity.dll_path, path))
        .cloned()
}

pub fn identify_address(address: usize) -> Option<ModIdentity> {
    if address == 0 {
        return None;
    }
    unsafe {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        if VirtualQuery(
            Some(address as *const _),
            &mut info,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        ) == 0
        {
            return None;
        }
        let module = HMODULE(info.AllocationBase);
        if module.is_invalid() {
            return None;
        }
        let path = crate::utils::get_module_path(module.0 as usize);
        find_by_module_path(&canonical_text(&path))
    }
}

pub fn first_identity_for_addresses(addresses: &[usize]) -> Option<ModIdentity> {
    addresses.iter().find_map(|address| identify_address(*address))
}

pub fn inventory_text() -> String {
    let Ok(registry) = mods().try_lock() else {
        return "<mod-registry-lock-unavailable>".to_string();
    };
    if registry.is_empty() {
        return "<none>".to_string();
    }
    registry
        .iter()
        .map(|identity| {
            format!(
                "name={} id={} version={} kind={} state={} handle={} path={}",
                identity.name,
                identity.id,
                identity.version.as_deref().unwrap_or("unknown"),
                identity.kind,
                identity.state,
                identity.module_handle.as_deref().unwrap_or("none"),
                identity.dll_path,
            )
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

pub fn recent_events_text(limit: usize) -> String {
    let Ok(events) = events().try_lock() else {
        return "<mod-event-lock-unavailable>".to_string();
    };
    if events.is_empty() {
        return "<none>".to_string();
    }
    events
        .iter()
        .rev()
        .take(limit)
        .rev()
        .map(|event| {
            format!(
                "#{} {} thread={} mod={} ({}) phase={} detail={}",
                event.sequence,
                event.timestamp,
                event.thread_id,
                event.mod_name,
                event.mod_id,
                event.phase,
                event.detail,
            )
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

pub fn record_lifecycle(identity: &ModIdentity, phase: &str, detail: &str) {
    let event = LifecycleEvent {
        sequence: NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        timestamp: Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        thread_id: unsafe { GetCurrentThreadId() },
        mod_id: identity.id.clone(),
        mod_name: identity.name.clone(),
        phase: phase.to_string(),
        detail: detail.to_string(),
    };
    let mut queue = events().lock().unwrap_or_else(|error| error.into_inner());
    queue.push_back(event);
    while queue.len() > 256 {
        queue.pop_front();
    }
}

pub fn write_registry_snapshot() -> std::io::Result<()> {
    if !file_io_policy::writes_allowed() {
        return Ok(());
    }

    #[derive(Serialize)]
    struct Snapshot {
        generated_at: String,
        mods: Vec<ModIdentity>,
        recent_events: Vec<LifecycleEvent>,
    }

    let snapshot = Snapshot {
        generated_at: Local::now().to_rfc3339(),
        mods: mods()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
        recent_events: events()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect(),
    };
    let path = PathBuf::from("logs").join("mod-registry.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(&snapshot)?)
}

fn bracket_prefix(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let end = rest.find(']')?;
    let value = rest[..end].trim();
    (!value.is_empty()).then_some(value)
}

fn token_prefix(line: &str) -> Option<&str> {
    let end = line.find([':', '|'])?;
    let value = line[..end].trim();
    (!value.is_empty() && value.len() <= 64).then_some(value)
}

fn canonical_text(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn paths_equal_text(left: &str, right: &str) -> bool {
    left.replace('/', "\\").eq_ignore_ascii_case(&right.replace('/', "\\"))
}

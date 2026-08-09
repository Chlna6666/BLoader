#![allow(dead_code)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr;
use std::slice;
use std::sync::{Mutex, OnceLock};

use crate::bl::abi::{
    BL_API_VERSION_1, BL_EVENT_BLOCK_ACTION, BL_EVENT_BOOTSTRAP_COMPLETE, BL_EVENT_CHAT,
    BL_EVENT_CREATED_LEVEL, BL_EVENT_KEY, BL_EVENT_LOCAL_PLAYER_BOUND, BL_EVENT_PLAYER_ACTION,
    BL_EVENT_RENDER_FRAME, BL_EVENT_RESOURCE_RELOAD, BL_EVENT_SET_LOCAL_PLAYER_AS_INIT,
    BL_EVENT_SHUTDOWN, BL_EVENT_START_GAME_PACKET, BL_EVENT_TICK,
    BL_EVENT_WORLD_ENTER, BL_LOG_DEBUG, BL_LOG_ERROR, BL_LOG_WARN, BL_PATH_CACHE_DIR,
    BL_PATH_GAME_DIR, BL_PATH_MODS_DIR, BL_PATH_UI_RESOURCE_PACK_DIR, BL_REGISTRY_EVENT,
    BL_REGISTRY_FEATURE_PANEL, BL_REGISTRY_FEATURE_TOGGLE, BL_REGISTRY_RESOURCE,
    BL_REGISTRY_TEXT_PANEL, BL_REGISTRY_UI_PANEL, BlEventCallback, BlHostApiV1,
    BlResourceCallback, BlStringView,
};
use crate::runtime::foundation::{crash_report, logging, mod_diagnostics};
#[cfg(feature = "panel-ui")]
use crate::bl::abi::{BlFeatureToggleCallback, BlTextCallback, BlUiCallback};
use crate::utils::get_exe_directory;

#[derive(Clone)]
pub struct RegisteredHandler {
    pub name: String,
    pub owner_name: String,
    pub callback: usize,
    pub user_data: usize,
}

#[derive(Clone)]
pub struct LoadedBlMod {
    pub id: String,
    pub name: String,
    pub dll_path: String,
    pub api_version: u32,
    pub version: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub module: usize,
    pub on_unload: Option<unsafe extern "system" fn()>,
}

#[cfg(feature = "panel-ui")]
#[derive(Clone)]
pub struct LoadedBlModView {
    pub id: String,
    pub name: String,
    pub dll_path: String,
    pub api_version: u32,
    pub version: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub module: usize,
    pub event_count: usize,
    pub ui_panel_count: usize,
    pub resource_count: usize,
    pub text_panel_count: usize,
    pub feature_toggle_count: usize,
    pub feature_panel_count: usize,
}

#[cfg(feature = "panel-ui")]
#[derive(Clone)]
pub struct RegisteredFeatureToggle {
    pub id: String,
    pub title: String,
    pub description: String,
    pub owner_name: String,
    pub enabled: bool,
    pub callback: usize,
    pub user_data: usize,
}

#[cfg(feature = "panel-ui")]
#[derive(Clone)]
pub struct FeatureToggleView {
    pub id: String,
    pub title: String,
    pub description: String,
    pub owner_name: String,
    pub enabled: bool,
}

#[cfg(feature = "panel-ui")]
#[derive(Clone)]
pub struct RegisteredFeaturePanel {
    pub id: String,
    pub title: String,
    pub description: String,
    pub owner_name: String,
    pub callback: usize,
    pub user_data: usize,
}

#[cfg(feature = "panel-ui")]
#[derive(Clone)]
pub struct FeaturePanelView {
    pub id: String,
    pub title: String,
    pub description: String,
    pub owner_name: String,
}

#[derive(Default)]
struct BlHostState {
    loaded_mods: Vec<LoadedBlMod>,
    event_handlers: Vec<RegisteredHandler>,
    #[cfg(feature = "panel-ui")]
    ui_panels: Vec<RegisteredHandler>,
    resources: Vec<RegisteredHandler>,
    #[cfg(feature = "panel-ui")]
    text_panels: Vec<RegisteredHandler>,
    #[cfg(feature = "panel-ui")]
    feature_toggles: Vec<RegisteredFeatureToggle>,
    #[cfg(feature = "panel-ui")]
    feature_panels: Vec<RegisteredFeaturePanel>,
}

static HOST_STATE: OnceLock<Mutex<BlHostState>> = OnceLock::new();
thread_local! {
    static ACTIVE_MOD_NAME: RefCell<Option<String>> = const { RefCell::new(None) };
}
static HOST_API: BlHostApiV1 = BlHostApiV1 {
    api_version: BL_API_VERSION_1,
    reserved: 0,
    log: Some(host_log),
    register: Some(host_register),
    get_host_version: Some(host_get_host_version),
    get_path: Some(host_get_path),
    resolve_symbol: Some(host_resolve_symbol),
    get_runtime_info: Some(host_get_runtime_info),
    path_exists: Some(host_path_exists),
    create_dir: Some(host_create_dir),
    read_text_file: Some(host_read_text_file),
    write_text_file: Some(host_write_text_file),
    ui_begin_window: None,
    ui_end_window: None,
    ui_text: None,
    ui_bullet_text: None,
    ui_button: None,
    ui_checkbox: None,
    ui_slider_float: None,
    ui_drag_float: None,
    ui_progress_bar: None,
    ui_separator: None,
    ui_same_line: None,
    hud_begin_block: None,
    hud_text_line: None,
    hud_end_block: None,
    register_bedrock_screen: None,
    request_bedrock_screen: None,
    ui_show_toast: None,
};

fn state() -> &'static Mutex<BlHostState> {
    HOST_STATE.get_or_init(|| Mutex::new(BlHostState::default()))
}

fn lock_state() -> std::sync::MutexGuard<'static, BlHostState> {
    state().lock().unwrap_or_else(|e| e.into_inner())
}

fn view_to_string(view: BlStringView) -> String {
    if view.ptr.is_null() || view.len == 0 {
        return String::new();
    }
    let bytes = unsafe { slice::from_raw_parts(view.ptr as *const u8, view.len) };
    String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .to_string()
}

fn is_public_runtime_info_key(key: &str) -> bool {
    if key.starts_with("ui.native_hud.") {
        return matches!(
            key,
            "ui.native_hud.status"
                | "ui.native_hud.mode"
                | "ui.native_hud.candidate_count"
                | "ui.native_hud.reason"
        );
    }
    matches!(
        key,
        "game.version"
            | "game.channel"
            | "game.exe_name"
            | "game.exe_path"
            | "loader.name"
            | "loader.version"
            | "loader.mod_count"
            | "loader.mod_list"
            | "loader.render_callbacks.d3d11"
            | "loader.render_callbacks.d3d12"
            | "process.memory_working_set_mb"
    ) || ["input.", "ui.", "mapping.", "client.", "client_instance."]
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

unsafe extern "system" fn host_log(level: u32, message: BlStringView) {
    let msg = view_to_string(message);
    let scope = current_mod_name().unwrap_or_else(|| "BL".to_string());
    match level {
        BL_LOG_DEBUG => logging::scoped_debug_message(&scope, &msg),
        BL_LOG_WARN => logging::scoped_warn_message(&scope, &msg),
        BL_LOG_ERROR => logging::scoped_error_message(&scope, &msg),
        _ => logging::scoped_info_message(&scope, &msg),
    }
}

unsafe extern "system" fn host_register(
    kind: u32,
    name: BlStringView,
    callback: *const c_void,
    user_data: *mut c_void,
) -> i32 {
    if callback.is_null() {
        return -1;
    }

    if matches!(
        kind,
        BL_REGISTRY_UI_PANEL
            | BL_REGISTRY_TEXT_PANEL
            | BL_REGISTRY_FEATURE_TOGGLE
            | BL_REGISTRY_FEATURE_PANEL
    ) {
        logging::warn_message(&format!(
            "BL panel registration rejected: kind={} owner={} (panel service is not compiled)",
            kind,
            current_mod_name().unwrap_or_else(|| "BL".to_string())
        ));
        return -4;
    }

    let entry = RegisteredHandler {
        name: view_to_string(name),
        owner_name: current_mod_name().unwrap_or_else(|| "BL".to_string()),
        callback: callback as usize,
        user_data: user_data as usize,
    };

    let mut state = lock_state();
    match kind {
        BL_REGISTRY_EVENT => state.event_handlers.push(entry),
        BL_REGISTRY_RESOURCE => state.resources.push(entry),
        _ => return -2,
    }

    logging::debug_message(&format!(
        "[BL] Registered kind={} name={}",
        kind,
        view_to_string(name)
    ));
    0
}

unsafe extern "system" fn host_get_path(kind: u32, out_path: *mut u8, out_len: usize) -> usize {
    let path = match kind {
        BL_PATH_GAME_DIR => get_exe_directory(),
        BL_PATH_MODS_DIR => get_exe_directory().join("mods"),
        BL_PATH_CACHE_DIR => PathBuf::new(),
        BL_PATH_UI_RESOURCE_PACK_DIR => PathBuf::new(),
        _ => PathBuf::new(),
    };
    copy_path(path, out_path, out_len)
}

unsafe extern "system" fn host_get_host_version(out_buf: *mut u8, out_len: usize) -> usize {
    copy_text(crate::runtime::foundation::build_info::VERSION, out_buf, out_len)
}

unsafe extern "system" fn host_resolve_symbol(_name: BlStringView) -> usize {
    0
}

unsafe extern "system" fn host_get_runtime_info(
    key: BlStringView,
    out_buf: *mut u8,
    out_len: usize,
) -> usize {
    let key = view_to_string(key);
    if !is_public_runtime_info_key(&key) {
        return copy_text("", out_buf, out_len);
    }
    let value = match key.as_str() {
        "game.version" => crate::utils::current_application_version().unwrap_or_default(),
        "game.channel" => String::new(),
        "game.exe_name" => crate::utils::get_exe_path()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
        "game.exe_path" => crate::utils::get_exe_path().display().to_string(),
        "loader.name" => crate::runtime::foundation::build_info::NAME.to_string(),
        "loader.version" => crate::runtime::foundation::build_info::VERSION.to_string(),
        "loader.mod_count" => loaded_mod_count().to_string(),
        "loader.mod_list" => loaded_mod_summaries().join(" | "),
        "loader.render_callbacks.d3d11" | "loader.render_callbacks.d3d12" => {
            "disabled: renderer service not compiled".to_string()
        }
        "mapping.status" | "mapping.summary" | "mapping.cache_status" => {
            "disabled: symbol subsystem not compiled".to_string()
        }
        "mapping.ready" => "false".to_string(),
        "mapping.symbol_count" => "0".to_string(),
        "mapping.highlights"
        | "mapping.cache_path"
        | "mapping.module_name"
        | "mapping.pack_id"
        | "mapping.public_symbols" => String::new(),
        "client.instance"
        | "client.local_player"
        | "client.level" => "0".to_string(),
        "client.ready"
        | "client.local_player_ready"
        | "client_instance.ready" => "false".to_string(),
        "client.status" => "disabled".to_string(),
        "ui.native_hud.status" | "ui.native_hud.mode" | "ui.native_hud.reason" => {
            "disabled".to_string()
        }
        "ui.native_hud.candidate_count" => "0".to_string(),
        "input.mouse_wheel_total_steps" | "input.mouse_wheel_last_steps" => "0".to_string(),
        "input.global.left_down"
        | "input.global.right_down"
        | "input.global.middle_down"
        | "ui.arcui.visible" => "false".to_string(),
        "ui.arcui.backend" => "disabled".to_string(),
        "process.memory_working_set_mb" => process_working_set_mb().to_string(),
        _ => String::new(),
    };
    copy_text(&value, out_buf, out_len)
}

unsafe extern "system" fn host_path_exists(path: BlStringView) -> bool {
    let path = PathBuf::from(view_to_string(path));
    path.exists()
}

unsafe extern "system" fn host_create_dir(path: BlStringView) -> bool {
    let path = PathBuf::from(view_to_string(path));
    std::fs::create_dir_all(path).is_ok()
}

unsafe extern "system" fn host_read_text_file(
    path: BlStringView,
    out_buf: *mut u8,
    out_len: usize,
) -> usize {
    let path = PathBuf::from(view_to_string(path));
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0;
    };
    let bytes = content.as_bytes();
    if !out_buf.is_null() && out_len > 0 {
        let copy_len = bytes.len().min(out_len.saturating_sub(1));
        ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, copy_len);
        *out_buf.add(copy_len) = 0;
    }
    bytes.len()
}

unsafe extern "system" fn host_write_text_file(path: BlStringView, content: BlStringView) -> i32 {
    let path = PathBuf::from(view_to_string(path));
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return -2;
        }
    }
    std::fs::write(path, view_to_string(content))
        .map(|_| 0)
        .unwrap_or(-1)
}

fn copy_path(path: PathBuf, out_path: *mut u8, out_len: usize) -> usize {
    let text = path.to_string_lossy();
    copy_text(&text, out_path, out_len)
}

fn copy_text(text: &str, out_path: *mut u8, out_len: usize) -> usize {
    let bytes = text.as_bytes();
    if !out_path.is_null() && out_len > 0 {
        let copy_len = bytes.len().min(out_len.saturating_sub(1));
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), out_path, copy_len);
            *out_path.add(copy_len) = 0;
        }
    }
    bytes.len()
}

pub fn host_api() -> *const BlHostApiV1 {
    &HOST_API
}

#[unsafe(export_name = "bl_register_mod_lang")]
pub unsafe extern "system" fn bl_register_mod_lang(
    locale: BlStringView,
    content: BlStringView,
) -> bool {
    let owner_name = current_mod_name().unwrap_or_else(active_mod_name_for_registration);
    crate::runtime::foundation::i18n::register_mod_lang(
        &owner_name,
        &view_to_string(locale),
        &view_to_string(content),
    )
}

#[unsafe(export_name = "bl_i18n_tr")]
pub unsafe extern "system" fn bl_i18n_tr(
    key: BlStringView,
    out_buf: *mut u8,
    out_len: usize,
) -> usize {
    let owner_name = current_mod_name().unwrap_or_else(|| "BL".to_string());
    let value = crate::runtime::foundation::i18n::tr_for(&owner_name, &view_to_string(key));
    copy_text(&value, out_buf, out_len)
}

#[unsafe(export_name = "bl_i18n_current_locale")]
pub unsafe extern "system" fn bl_i18n_current_locale(out_buf: *mut u8, out_len: usize) -> usize {
    let value = crate::runtime::foundation::i18n::current_locale();
    copy_text(&value, out_buf, out_len)
}

pub fn register_loaded_mod(
    id: String,
    name: String,
    dll_path: String,
    api_version: u32,
    version: Option<String>,
    author: Option<String>,
    description: Option<String>,
    module: usize,
    on_unload: Option<unsafe extern "system" fn()>,
) {
    logging::debug_message(&format!(
        "[BL] Registered loaded mod {} ({}) @0x{:X}",
        name, id, module
    ));
    let mut state = lock_state();
    state.loaded_mods.push(LoadedBlMod {
        id,
        name,
        dll_path,
        api_version,
        version,
        author,
        description,
        module,
        on_unload,
    });
}

pub fn dispatch_bootstrap_complete() {
    dispatch_event(BL_EVENT_BOOTSTRAP_COMPLETE, ptr::null());
    dispatch_resource_reload(BL_EVENT_RESOURCE_RELOAD);
}

pub fn dispatch_render_frame() {
    dispatch_event(BL_EVENT_RENDER_FRAME, ptr::null());
}

#[cfg(feature = "panel-ui")]
pub fn dispatch_ui_frame() {
    dispatch_event(crate::bl::abi::BL_EVENT_UI_FRAME, ptr::null());
}

pub fn dispatch_shutdown() {
    let unloads = {
        let state = lock_state();
        state
            .loaded_mods
            .iter()
            .filter_map(|m| {
                m.on_unload
                    .map(|f| (m.id.clone(), m.name.clone(), m.module, f))
            })
            .collect::<Vec<_>>()
    };

    dispatch_event(BL_EVENT_SHUTDOWN, ptr::null());

    for (id, name, module, f) in unloads {
        run_mod_callback_safely(&name, "on_unload", || unsafe { f() });
        logging::info_message(&format!(
            "[BL] Unloaded mod: {} ({}) @0x{:X}",
            name, id, module
        ));
    }
}

pub fn dispatch_event(event_id: u32, payload: *const c_void) {
    let callbacks = {
        let state = lock_state();
        state.event_handlers.clone()
    };

    for handler in callbacks {
        let callback: BlEventCallback = unsafe { std::mem::transmute(handler.callback) };
        run_mod_callback_safely(&handler.owner_name, "event", || unsafe {
            callback(event_id, payload, handler.user_data as *mut c_void);
        });
    }
}

#[cfg(feature = "panel-ui")]
pub fn dispatch_ui_panels() {
    let panels = {
        let state = lock_state();
        state.ui_panels.clone()
    };

    for panel in panels {
        let callback: BlUiCallback = unsafe { std::mem::transmute(panel.callback) };
        run_mod_callback_safely(&panel.owner_name, "ui_panel", || unsafe {
            callback(panel.user_data as *mut c_void);
        });
    }
}

#[cfg(feature = "panel-ui")]
pub fn dispatch_text_panels() {
    let handlers: Vec<RegisteredHandler> = {
        let state = lock_state();
        state.text_panels.clone()
    };
    for handler in handlers {
        let callback: BlTextCallback = unsafe { std::mem::transmute(handler.callback) };
        run_mod_callback_safely(&handler.owner_name, "text_panel", || unsafe {
            callback(handler.user_data as *mut c_void)
        });
    }
}

#[cfg(feature = "panel-ui")]
pub fn has_text_panels() -> bool {
    let state = lock_state();
    !state.text_panels.is_empty()
}

pub fn dispatch_resource_reload(reason: u32) {
    dispatch_event(BL_EVENT_RESOURCE_RELOAD, ptr::null());

    let resources = {
        let state = lock_state();
        state.resources.clone()
    };

    for resource in resources {
        let callback: BlResourceCallback = unsafe { std::mem::transmute(resource.callback) };
        run_mod_callback_safely(&resource.owner_name, "resource_reload", || unsafe {
            callback(reason, resource.user_data as *mut c_void);
        });
    }
}

pub fn with_active_mod_name<T>(name: &str, f: impl FnOnce() -> T) -> T {
    ACTIVE_MOD_NAME.with(|slot| {
        let previous = slot.replace(Some(name.to_string()));
        let result = f();
        slot.replace(previous);
        result
    })
}

pub fn with_active_mod_identity<T>(
    identity: &mod_diagnostics::ModIdentity,
    phase: &str,
    f: impl FnOnce() -> T,
) -> T {
    let _scope = mod_diagnostics::enter_scope(identity, phase.to_string());
    with_active_mod_name(&identity.name, f)
}

fn current_mod_name() -> Option<String> {
    ACTIVE_MOD_NAME.with(|slot| slot.borrow().clone())
}

pub fn active_mod_name_for_registration() -> String {
    current_mod_name().unwrap_or_else(|| "BL".to_string())
}

fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(msg) = payload.downcast_ref::<&str>() {
        return (*msg).to_string();
    }
    if let Some(msg) = payload.downcast_ref::<String>() {
        return msg.clone();
    }
    "Unknown panic".to_string()
}

fn run_mod_callback_safely(owner_name: &str, callback_kind: &str, f: impl FnOnce()) {
    let identity = mod_diagnostics::find_by_name(owner_name);
    let result = if let Some(identity) = identity.as_ref() {
        with_active_mod_identity(identity, callback_kind, || {
            panic::catch_unwind(AssertUnwindSafe(f))
        })
    } else {
        with_active_mod_name(owner_name, || panic::catch_unwind(AssertUnwindSafe(f)))
    };
    // A Mod callback can replace SetUnhandledExceptionFilter. Re-arm after every
    // callback so the next native crash remains attributable.
    crash_report::rearm_unhandled_filter(&format!("after-mod-callback:{owner_name}:{callback_kind}"));
    if let Err(payload) = result {
        let details = panic_payload_to_string(payload.as_ref());
        if let Some(identity) = identity.as_ref() {
            mod_diagnostics::mark_crashed(identity, callback_kind, &format!("Rust panic: {details}"));
        }
        crash_report::capture_rust_panic(
            &format!("mod={owner_name} callback={callback_kind} detail={details}"),
            false,
        );
        logging::scoped_error_message(
            &format!("mod:{owner_name}"),
            &format!("MOD_CALLBACK_PANIC | callback={callback_kind} | detail={details}"),
        );
    }
}

pub fn loaded_mod_summaries() -> Vec<String> {
    let state = lock_state();
    state
        .loaded_mods
        .iter()
        .map(|m| format!("{} ({})", m.name, m.id))
        .collect()
}

pub fn loaded_mods() -> Vec<LoadedBlMod> {
    let state = lock_state();
    state.loaded_mods.clone()
}

#[cfg(feature = "panel-ui")]
pub fn loaded_mod_views() -> Vec<LoadedBlModView> {
    let state = lock_state();
    let mut mods = state
        .loaded_mods
        .iter()
        .map(|loaded| LoadedBlModView {
            id: loaded.id.clone(),
            name: loaded.name.clone(),
            dll_path: loaded.dll_path.clone(),
            api_version: loaded.api_version,
            version: loaded.version.clone(),
            author: loaded.author.clone(),
            description: loaded.description.clone(),
            module: loaded.module,
            event_count: state
                .event_handlers
                .iter()
                .filter(|handler| handler.owner_name == loaded.name)
                .count(),
            ui_panel_count: state
                .ui_panels
                .iter()
                .filter(|handler| handler.owner_name == loaded.name)
                .count(),
            resource_count: state
                .resources
                .iter()
                .filter(|handler| handler.owner_name == loaded.name)
                .count(),
            text_panel_count: state
                .text_panels
                .iter()
                .filter(|handler| handler.owner_name == loaded.name)
                .count(),
            feature_toggle_count: state
                .feature_toggles
                .iter()
                .filter(|toggle| toggle.owner_name == loaded.name)
                .count(),
            feature_panel_count: state
                .feature_panels
                .iter()
                .filter(|panel| panel.owner_name == loaded.name)
                .count(),
        })
        .collect::<Vec<_>>();

    mods.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.dll_path.cmp(&right.dll_path))
    });
    mods
}

#[cfg(feature = "panel-ui")]
pub fn feature_toggles() -> Vec<FeatureToggleView> {
    let mut toggles = {
        let state = lock_state();
        state
            .feature_toggles
            .iter()
            .map(|toggle| FeatureToggleView {
                id: toggle.id.clone(),
                title: toggle.title.clone(),
                description: toggle.description.clone(),
                owner_name: toggle.owner_name.clone(),
                enabled: toggle.enabled,
            })
            .collect::<Vec<_>>()
    };
    toggles.sort_by(|left, right| {
        left.owner_name
            .cmp(&right.owner_name)
            .then_with(|| left.title.cmp(&right.title))
    });
    toggles
}

#[cfg(feature = "panel-ui")]
pub fn feature_panel_for_owner(owner_name: &str) -> Option<FeaturePanelView> {
    let state = lock_state();
    state
        .feature_panels
        .iter()
        .find(|panel| panel.owner_name == owner_name)
        .map(|panel| FeaturePanelView {
            id: panel.id.clone(),
            title: panel.title.clone(),
            description: panel.description.clone(),
            owner_name: panel.owner_name.clone(),
        })
}

#[cfg(feature = "panel-ui")]
pub fn set_feature_toggle(owner_name: &str, id: &str, enabled: bool) -> bool {
    let callback = {
        let mut state = lock_state();
        let Some(toggle) = state
            .feature_toggles
            .iter_mut()
            .find(|toggle| toggle.owner_name == owner_name && toggle.id == id)
        else {
            return false;
        };

        if toggle.enabled == enabled {
            return true;
        }

        toggle.enabled = enabled;
        (toggle.owner_name.clone(), toggle.callback, toggle.user_data)
    };

    let (owner_name, callback_ptr, user_data) = callback;
    let callback: BlFeatureToggleCallback = unsafe { std::mem::transmute(callback_ptr) };
    run_mod_callback_safely(&owner_name, "feature_toggle", || unsafe {
        callback(enabled as u8, user_data as *mut c_void);
    });
    true
}

#[cfg(feature = "panel-ui")]
pub fn render_feature_panel_inline(
    owner_name: &str,
    id: &str,
    panel_key: &str,
    rect: arcui_core::Rect,
) -> bool {
    let callback = {
        let state = lock_state();
        let Some(panel) = state
            .feature_panels
            .iter()
            .find(|panel| panel.owner_name == owner_name && panel.id == id)
        else {
            return false;
        };
        (panel.owner_name.clone(), panel.callback, panel.user_data)
    };

    let (owner_name, callback_ptr, user_data) = callback;
    let callback: BlUiCallback = unsafe { std::mem::transmute(callback_ptr) };
    run_mod_callback_safely(&owner_name, "feature_panel", || {
        crate::bl::ui::host_render_inline_panel(panel_key, rect, callback, user_data as *mut c_void);
    });
    true
}

fn loaded_mod_count() -> usize {
    let state = lock_state();
    state.loaded_mods.len()
}

#[cfg(feature = "panel-ui")]
fn parse_feature_toggle_registration(raw: &str) -> Option<(String, String, String, bool)> {
    let mut lines = raw.lines();
    let id = lines.next()?.trim().to_string();
    let title = lines.next()?.trim().to_string();
    let description = lines.next().unwrap_or_default().trim().to_string();
    let default_enabled = matches!(
        lines.next().unwrap_or_default().trim(),
        "1" | "true" | "TRUE" | "True"
    );

    if id.is_empty() || title.is_empty() {
        return None;
    }

    Some((id, title, description, default_enabled))
}

#[cfg(feature = "panel-ui")]
fn parse_feature_panel_registration(raw: &str) -> Option<(String, String, String)> {
    let mut lines = raw.lines();
    let id = lines.next()?.trim().to_string();
    let title = lines.next()?.trim().to_string();
    let description = lines.next().unwrap_or_default().trim().to_string();

    if id.is_empty() || title.is_empty() {
        return None;
    }

    Some((id, title, description))
}

fn process_working_set_mb() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::{host_get_runtime_info, is_public_runtime_info_key};
    use crate::bl::abi::BlStringView;

    #[test]
    fn exposes_the_symbol_mapping_runtime_info_keys() {
        for key in [
            "mapping.summary",
            "mapping.highlights",
            "mapping.cache_path",
            "mapping.cache_status",
            "mapping.module_name",
            "mapping.symbol_count",
        ] {
            assert!(is_public_runtime_info_key(key), "{key} must be public");
        }
    }

    #[test]
    fn mapping_summary_reports_an_uninitialized_resolver() {
        let key = b"mapping.summary";
        let mut output = [0u8; 64];
        let length = unsafe {
            host_get_runtime_info(
                BlStringView {
                    ptr: key.as_ptr() as *const i8,
                    len: key.len(),
                },
                output.as_mut_ptr(),
                output.len(),
            )
        };

        assert_eq!(
            std::str::from_utf8(&output[..length]).expect("host output is UTF-8"),
            "disabled: symbol subsystem not compiled"
        );
    }

    #[test]
    fn mapping_ready_is_false_without_a_loaded_pack() {
        let key = b"mapping.ready";
        let mut output = [0u8; 16];
        let length = unsafe {
            host_get_runtime_info(
                BlStringView {
                    ptr: key.as_ptr() as *const i8,
                    len: key.len(),
                },
                output.as_mut_ptr(),
                output.len(),
            )
        };

        assert_eq!(
            std::str::from_utf8(&output[..length]).expect("host output is UTF-8"),
            "false"
        );
    }

    #[test]
    fn exposes_client_runtime_info_keys_without_claiming_client_support() {
        for key in [
            "client.instance",
            "client.local_player",
            "client.level",
            "client.ready",
            "client.status",
        ] {
            assert!(is_public_runtime_info_key(key), "{key} must be public");
        }
    }

    #[test]
    fn exposes_native_hud_runtime_info_without_candidate_addresses() {
        for key in [
            "ui.native_hud.status",
            "ui.native_hud.mode",
            "ui.native_hud.candidate_count",
            "ui.native_hud.reason",
        ] {
            assert!(is_public_runtime_info_key(key), "{key} must be public");
        }
        assert!(!is_public_runtime_info_key(
            "ui.native_hud.candidate_address"
        ));
    }
}

pub fn resource_summaries() -> Vec<String> {
    let state = lock_state();
    state.resources.iter().map(|r| r.name.clone()).collect()
}

pub fn render_callback_summaries() -> Vec<String> {
    let mut lines = Vec::new();

    for d3d12 in crate::core::d3d12_queue::d3d12_callback_summaries() {
        lines.push(format!("D3D12: {d3d12}"));
    }

    for d3d11 in crate::core::d3d12_queue::d3d11_callback_summaries() {
        lines.push(format!("D3D11: {d3d11}"));
    }

    lines
}

#[cfg(feature = "panel-ui")]
pub fn should_attach_overlay() -> bool {
    true
}

pub fn has_event_handlers() -> bool {
    let state = lock_state();
    !state.event_handlers.is_empty()
}

pub const fn event_id_tick() -> u32 {
    BL_EVENT_TICK
}

pub const fn event_id_key() -> u32 {
    BL_EVENT_KEY
}

pub const fn event_id_world_enter() -> u32 {
    BL_EVENT_WORLD_ENTER
}

pub const fn event_id_chat() -> u32 {
    BL_EVENT_CHAT
}

pub const fn event_id_created_level() -> u32 {
    BL_EVENT_CREATED_LEVEL
}

pub const fn event_id_start_game_packet() -> u32 {
    BL_EVENT_START_GAME_PACKET
}

pub const fn event_id_set_local_player_as_init() -> u32 {
    BL_EVENT_SET_LOCAL_PLAYER_AS_INIT
}

pub const fn event_id_local_player_bound() -> u32 {
    BL_EVENT_LOCAL_PLAYER_BOUND
}

pub const fn event_id_player_action() -> u32 {
    BL_EVENT_PLAYER_ACTION
}

pub const fn event_id_block_action() -> u32 {
    BL_EVENT_BLOCK_ACTION
}

use std::ffi::{c_char, c_void};
use std::sync::OnceLock;

pub mod tooling;

pub const BL_API_VERSION_1: u32 = 1;

pub const BL_PATH_GAME_DIR: u32 = 1;
pub const BL_PATH_MODS_DIR: u32 = 2;
pub const BL_PATH_CACHE_DIR: u32 = 3;
pub const BL_PATH_UI_RESOURCE_PACK_DIR: u32 = 4;

pub const BL_LOG_DEBUG: u32 = 0;
pub const BL_LOG_INFO: u32 = 1;
pub const BL_LOG_WARN: u32 = 2;
pub const BL_LOG_ERROR: u32 = 3;

pub const BL_EVENT_BOOTSTRAP_COMPLETE: u32 = 1;
pub const BL_EVENT_RENDER_FRAME: u32 = 2;
pub const BL_EVENT_UI_FRAME: u32 = 3;
pub const BL_EVENT_RESOURCE_RELOAD: u32 = 4;
pub const BL_EVENT_SHUTDOWN: u32 = 5;
pub const BL_EVENT_TICK: u32 = 6;
pub const BL_EVENT_KEY: u32 = 7;
pub const BL_EVENT_WORLD_ENTER: u32 = 8;
pub const BL_EVENT_CHAT: u32 = 9;
pub const BL_EVENT_CREATED_LEVEL: u32 = 10;
pub const BL_EVENT_START_GAME_PACKET: u32 = 11;
pub const BL_EVENT_SET_LOCAL_PLAYER_AS_INIT: u32 = 12;
pub const BL_EVENT_LOCAL_PLAYER_BOUND: u32 = 13;
pub const BL_EVENT_PLAYER_ACTION: u32 = 14;
pub const BL_EVENT_BLOCK_ACTION: u32 = 15;
pub const BL_EVENT_CLIENT_JOIN_LEVEL: u32 = 16;
pub const BL_EVENT_RENDER_3D: u32 = 17;

pub const BL_REGISTRY_EVENT: u32 = 1;
pub const BL_REGISTRY_UI_PANEL: u32 = 2;
pub const BL_REGISTRY_RESOURCE: u32 = 3;
pub const BL_REGISTRY_TEXT_PANEL: u32 = 4;
pub const BL_REGISTRY_FEATURE_TOGGLE: u32 = 5;
pub const BL_REGISTRY_FEATURE_PANEL: u32 = 6;
pub const BL_TOAST_TOP_LEFT: u32 = 0;
pub const BL_TOAST_TOP_RIGHT: u32 = 1;
pub const BL_TOAST_BOTTOM_LEFT: u32 = 2;
pub const BL_TOAST_BOTTOM_RIGHT: u32 = 3;
pub const BL_TOAST_INFO: u32 = 0;
pub const BL_TOAST_SUCCESS: u32 = 1;
pub const BL_TOAST_WARNING: u32 = 2;
pub const BL_TOAST_ERROR: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlStringView {
    pub ptr: *const c_char,
    pub len: usize,
}

unsafe impl Send for BlStringView {}
unsafe impl Sync for BlStringView {}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlTickEvent {
    pub frame_index: u64,
    pub delta_seconds: f32,
    pub total_seconds: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlKeyEvent {
    pub virtual_key: u32,
    pub is_down: u8,
    pub is_repeat: u8,
    pub alt: u8,
    pub ctrl: u8,
    pub shift: u8,
    pub reserved: [u8; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlWorldEnterEvent {
    pub world_name: BlStringView,
    pub source: BlStringView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlChatEvent {
    pub author: BlStringView,
    pub message: BlStringView,
    pub channel: BlStringView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlCreatedLevelEvent {
    pub client_instance: usize,
    pub level: usize,
    pub route: BlStringView,
    pub world_name: BlStringView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlStartGamePacketEvent {
    pub this_ptr: usize,
    pub arg1: usize,
    pub arg2: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlSetLocalPlayerAsInitEvent {
    pub this_ptr: usize,
    pub arg1: usize,
    pub arg2: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlLocalPlayerBoundEvent {
    pub player_ptr: usize,
    pub client_instance: usize,
    pub route: BlStringView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlClientJoinLevelEvent {
    pub player_ptr: usize,
    pub client_instance: usize,
    pub route: BlStringView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlPlayerActionEvent {
    pub this_ptr: usize,
    pub packet_ptr: usize,
    pub arg1: usize,
    pub arg2: usize,
    pub action_code: u32,
    pub block_x: i32,
    pub block_y: i32,
    pub block_z: i32,
    pub face: i32,
    pub action_name: BlStringView,
    pub status: BlStringView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlBlockActionEvent {
    pub player_action: u32,
    pub block_x: i32,
    pub block_y: i32,
    pub block_z: i32,
    pub face: i32,
    pub action_name: BlStringView,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlRender3DEvent {
    pub level_render: usize,
    pub screen_context: usize,
}

pub type BlLogFn = unsafe extern "system" fn(level: u32, message: BlStringView);
pub type BlRegisterFn = unsafe extern "system" fn(
    kind: u32,
    name: BlStringView,
    callback: *const c_void,
    user_data: *mut c_void,
) -> i32;
pub type BlGetHostVersionFn = unsafe extern "system" fn(out_buf: *mut u8, out_len: usize) -> usize;
pub type BlGetPathFn =
    unsafe extern "system" fn(kind: u32, out_path: *mut u8, out_len: usize) -> usize;
pub type BlResolveSymbolFn = unsafe extern "system" fn(name: BlStringView) -> usize;
pub type BlGetRuntimeInfoFn =
    unsafe extern "system" fn(key: BlStringView, out_buf: *mut u8, out_len: usize) -> usize;
pub type BlPathExistsFn = unsafe extern "system" fn(path: BlStringView) -> bool;
pub type BlCreateDirFn = unsafe extern "system" fn(path: BlStringView) -> bool;
pub type BlReadTextFileFn =
    unsafe extern "system" fn(path: BlStringView, out_buf: *mut u8, out_len: usize) -> usize;
pub type BlWriteTextFileFn =
    unsafe extern "system" fn(path: BlStringView, content: BlStringView) -> i32;
pub type BlUiBeginWindowFn =
    unsafe extern "system" fn(title: BlStringView, open: *mut bool, flags: u32) -> bool;
pub type BlUiEndWindowFn = unsafe extern "system" fn();
pub type BlUiTextFn = unsafe extern "system" fn(text: BlStringView);
pub type BlUiBulletTextFn = unsafe extern "system" fn(text: BlStringView);
pub type BlUiButtonFn = unsafe extern "system" fn(label: BlStringView) -> bool;
pub type BlUiCheckboxFn = unsafe extern "system" fn(label: BlStringView, value: *mut bool) -> bool;
pub type BlUiSliderFloatFn =
    unsafe extern "system" fn(label: BlStringView, value: *mut f32, min: f32, max: f32) -> bool;
pub type BlUiDragFloatFn =
    unsafe extern "system" fn(label: BlStringView, value: *mut f32, min: f32, max: f32) -> bool;
pub type BlUiProgressBarFn =
    unsafe extern "system" fn(label: BlStringView, value: f32, min: f32, max: f32);
pub type BlUiSeparatorFn = unsafe extern "system" fn();
pub type BlUiSameLineFn = unsafe extern "system" fn();
pub type BlHudBeginBlockFn = unsafe extern "system" fn(id: BlStringView, x: i32, y: i32) -> bool;
pub type BlHudTextLineFn = unsafe extern "system" fn(text: BlStringView);
pub type BlHudEndBlockFn = unsafe extern "system" fn();
pub type BlResourceCallback = unsafe extern "system" fn(reason: u32, user_data: *mut c_void);
pub type BlRegisterBedrockScreenFn = unsafe extern "system" fn(screen_id: BlStringView) -> bool;
pub type BlRequestBedrockScreenFn = unsafe extern "system" fn(screen_id: BlStringView) -> bool;
pub type BlUiShowToastFn = unsafe extern "system" fn(
    title: BlStringView,
    body: BlStringView,
    anchor: u32,
    kind: u32,
    lifetime_seconds: f32,
) -> bool;
pub type BlRegisterModLangFn =
    unsafe extern "system" fn(locale: BlStringView, content: BlStringView) -> bool;
pub type BlI18nTranslateFn =
    unsafe extern "system" fn(key: BlStringView, out_buf: *mut u8, out_len: usize) -> usize;
pub type BlI18nCurrentLocaleFn =
    unsafe extern "system" fn(out_buf: *mut u8, out_len: usize) -> usize;

// Effects render callback types
/// D3D11 渲染回调函数类型
pub type BlD3D11RenderCallback = unsafe extern "system" fn(
    device: *mut c_void,
    context: *mut c_void,
    back_buffer: *mut c_void,
    width: u32,
    height: u32,
);

/// D3D12 渲染回调函数类型
pub type BlD3D12RenderCallback = unsafe extern "system" fn(
    device: *mut c_void,
    command_list: *mut c_void,
    back_buffer: *mut c_void,
    width: u32,
    height: u32,
);

#[repr(C)]
pub struct BlHostApiV1 {
    pub api_version: u32,
    pub reserved: u32,
    pub log: Option<BlLogFn>,
    pub register: Option<BlRegisterFn>,
    pub get_host_version: Option<BlGetHostVersionFn>,
    pub get_path: Option<BlGetPathFn>,
    pub resolve_symbol: Option<BlResolveSymbolFn>,
    pub get_runtime_info: Option<BlGetRuntimeInfoFn>,
    pub path_exists: Option<BlPathExistsFn>,
    pub create_dir: Option<BlCreateDirFn>,
    pub read_text_file: Option<BlReadTextFileFn>,
    pub write_text_file: Option<BlWriteTextFileFn>,
    pub ui_begin_window: Option<BlUiBeginWindowFn>,
    pub ui_end_window: Option<BlUiEndWindowFn>,
    pub ui_text: Option<BlUiTextFn>,
    pub ui_bullet_text: Option<BlUiBulletTextFn>,
    pub ui_button: Option<BlUiButtonFn>,
    pub ui_checkbox: Option<BlUiCheckboxFn>,
    pub ui_slider_float: Option<BlUiSliderFloatFn>,
    pub ui_drag_float: Option<BlUiDragFloatFn>,
    pub ui_progress_bar: Option<BlUiProgressBarFn>,
    pub ui_separator: Option<BlUiSeparatorFn>,
    pub ui_same_line: Option<BlUiSameLineFn>,
    pub hud_begin_block: Option<BlHudBeginBlockFn>,
    pub hud_text_line: Option<BlHudTextLineFn>,
    pub hud_end_block: Option<BlHudEndBlockFn>,
    pub register_bedrock_screen: Option<BlRegisterBedrockScreenFn>,
    pub request_bedrock_screen: Option<BlRequestBedrockScreenFn>,
    pub ui_show_toast: Option<BlUiShowToastFn>,
}

pub type BlOnLoadFn = unsafe extern "system" fn(host: *const BlHostApiV1) -> i32;
pub type BlOnUnloadFn = unsafe extern "system" fn();
pub type BlEventCallback =
    unsafe extern "system" fn(event_id: u32, payload: *const c_void, user_data: *mut c_void);
pub type BlUiCallback = unsafe extern "system" fn(user_data: *mut c_void);
pub type BlTextCallback = unsafe extern "system" fn(user_data: *mut c_void);
pub type BlFeatureToggleCallback = unsafe extern "system" fn(enabled: u8, user_data: *mut c_void);
pub type BlModMainFn = unsafe extern "system" fn(host: *const BlHostApiV1) -> *const BlModApiV1;

#[repr(C)]
pub struct BlModApiV1 {
    pub api_version: u32,
    pub mod_id: BlStringView,
    pub mod_name: BlStringView,
    pub on_load: Option<BlOnLoadFn>,
    pub on_unload: Option<BlOnUnloadFn>,
}

#[derive(Clone, Copy, Debug)]
pub struct FeatureToggleRegistration<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub default_enabled: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct FeaturePanelRegistration<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub description: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastAnchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Copy)]
pub struct BLoader {
    raw: *const BlHostApiV1,
}

static CURRENT_BLOADER: OnceLock<BLoader> = OnceLock::new();

unsafe impl Send for BLoader {}
unsafe impl Sync for BLoader {}

impl BLoader {
    pub fn from_raw(raw: *const BlHostApiV1) -> Self {
        Self { raw }
    }

    pub fn raw(&self) -> &'static BlHostApiV1 {
        unsafe { &*self.raw }
    }

    pub fn log(&self, level: u32, message: &str) {
        if let Some(f) = self.raw().log {
            unsafe { f(level, str_view(message)) };
        }
    }

    pub fn info(&self, message: &str) {
        self.log(BL_LOG_INFO, message);
    }

    pub fn debug(&self, message: &str) {
        self.log(BL_LOG_DEBUG, message);
    }

    pub fn warn(&self, message: &str) {
        self.log(BL_LOG_WARN, message);
    }

    pub fn error(&self, message: &str) {
        self.log(BL_LOG_ERROR, message);
    }

    pub fn trace(&self, message: &str) {
        self.log(BL_LOG_DEBUG, message);
    }

    pub fn logging(&self) -> Logging {
        Logging { loader: *self }
    }

    pub fn registry(&self) -> Registry {
        Registry { loader: *self }
    }

    pub fn runtime(&self) -> Runtime {
        Runtime { loader: *self }
    }

    pub fn paths(&self) -> Paths {
        Paths { loader: *self }
    }

    pub fn fs(&self) -> FileSystem {
        FileSystem { loader: *self }
    }

    pub fn ui(&self) -> Ui {
        Ui { loader: *self }
    }

    pub fn hud(&self) -> Hud {
        Hud { loader: *self }
    }

    pub fn register_event(
        &self,
        name: &str,
        callback: BlEventCallback,
        user_data: *mut c_void,
    ) -> i32 {
        match self.raw().register {
            Some(f) => unsafe {
                f(
                    BL_REGISTRY_EVENT,
                    str_view(name),
                    callback as *const c_void,
                    user_data,
                )
            },
            None => -1,
        }
    }

    pub fn register_ui_panel(
        &self,
        name: &str,
        callback: BlUiCallback,
        user_data: *mut c_void,
    ) -> i32 {
        match self.raw().register {
            Some(f) => unsafe {
                f(
                    BL_REGISTRY_UI_PANEL,
                    str_view(name),
                    callback as *const c_void,
                    user_data,
                )
            },
            None => -1,
        }
    }

    pub fn register_resource(
        &self,
        name: &str,
        callback: BlResourceCallback,
        user_data: *mut c_void,
    ) -> i32 {
        match self.raw().register {
            Some(f) => unsafe {
                f(
                    BL_REGISTRY_RESOURCE,
                    str_view(name),
                    callback as *const c_void,
                    user_data,
                )
            },
            None => -1,
        }
    }

    pub fn register_text_panel(
        &self,
        name: &str,
        callback: BlTextCallback,
        user_data: *mut c_void,
    ) -> i32 {
        match self.raw().register {
            Some(f) => unsafe {
                f(
                    BL_REGISTRY_TEXT_PANEL,
                    str_view(name),
                    callback as *const c_void,
                    user_data,
                )
            },
            None => -1,
        }
    }

    pub fn register_feature_toggle(
        &self,
        registration: FeatureToggleRegistration<'_>,
        callback: BlFeatureToggleCallback,
        user_data: *mut c_void,
    ) -> i32 {
        let encoded = encode_feature_toggle_registration(registration);
        match self.raw().register {
            Some(f) => unsafe {
                f(
                    BL_REGISTRY_FEATURE_TOGGLE,
                    str_view(&encoded),
                    callback as *const c_void,
                    user_data,
                )
            },
            None => -1,
        }
    }

    pub fn register_feature_panel(
        &self,
        registration: FeaturePanelRegistration<'_>,
        callback: BlUiCallback,
        user_data: *mut c_void,
    ) -> i32 {
        let encoded = encode_feature_panel_registration(registration);
        match self.raw().register {
            Some(f) => unsafe {
                f(
                    BL_REGISTRY_FEATURE_PANEL,
                    str_view(&encoded),
                    callback as *const c_void,
                    user_data,
                )
            },
            None => -1,
        }
    }

    pub fn get_path(&self, kind: u32) -> Option<String> {
        let f = self.raw().get_path?;
        let len = unsafe { f(kind, std::ptr::null_mut(), 0) };
        if len == 0 {
            return Some(String::new());
        }
        let mut buf = vec![0u8; len + 1];
        unsafe { f(kind, buf.as_mut_ptr(), buf.len()) };
        let nul = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..nul]).to_string())
    }

    pub fn ui_backend(&self) -> Option<String> {
        self.get_runtime_info("ui.arcui.backend")
            .filter(|value| !value.is_empty())
    }

    pub fn get_host_version(&self) -> Option<String> {
        let f = self.raw().get_host_version?;
        let len = unsafe { f(std::ptr::null_mut(), 0) };
        let mut buf = vec![0u8; len + 1];
        unsafe { f(buf.as_mut_ptr(), buf.len()) };
        let nul = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..nul]).to_string())
    }

    pub fn resolve_symbol(&self, name: &str) -> usize {
        match self.raw().resolve_symbol {
            Some(f) => unsafe { f(str_view(name)) },
            None => 0,
        }
    }

    pub fn get_runtime_info(&self, key: &str) -> Option<String> {
        let f = self.raw().get_runtime_info?;
        let len = unsafe { f(str_view(key), std::ptr::null_mut(), 0) };
        let mut buf = vec![0u8; len + 1];
        unsafe { f(str_view(key), buf.as_mut_ptr(), buf.len()) };
        let nul = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..nul]).to_string())
    }

    pub fn world_name(&self) -> Option<String> {
        self.get_runtime_info("world.name")
            .filter(|value| !value.is_empty())
    }

    pub fn world_source(&self) -> Option<String> {
        self.get_runtime_info("world.source")
            .filter(|value| !value.is_empty())
    }

    pub fn world_ready(&self) -> bool {
        matches!(
            self.get_runtime_info("world.ready").as_deref(),
            Some("true")
        )
    }

    pub fn client_instance_ready(&self) -> bool {
        matches!(
            self.get_runtime_info("client.ready").as_deref(),
            Some("true")
        )
    }

    pub fn chunk_ready(&self) -> bool {
        matches!(
            self.get_runtime_info("chunk.ready").as_deref(),
            Some("true")
        )
    }

    pub fn chunk_summary(&self) -> Option<String> {
        self.get_runtime_info("chunk.summary")
            .filter(|value| !value.is_empty())
    }

    pub fn chunk_current(&self) -> Option<String> {
        self.get_runtime_info("chunk.current")
            .filter(|value| !value.is_empty())
    }

    pub fn chunk_local_block(&self) -> Option<String> {
        self.get_runtime_info("chunk.local_block")
            .filter(|value| !value.is_empty())
    }

    pub fn chunk_border_distance(&self) -> Option<String> {
        self.get_runtime_info("chunk.border_distance")
            .filter(|value| !value.is_empty())
    }

    pub fn mapping_summary(&self) -> Option<String> {
        self.get_runtime_info("mapping.summary")
            .filter(|value| !value.is_empty())
    }

    pub fn mapping_cache_path(&self) -> Option<String> {
        self.get_runtime_info("mapping.cache_path")
            .filter(|value| !value.is_empty())
    }

    pub fn camera_fov_status(&self) -> Option<String> {
        self.get_runtime_info("camera.fov.status")
            .filter(|value| !value.is_empty())
    }

    pub fn camera_fov_supported(&self) -> bool {
        matches!(
            self.get_runtime_info("camera.fov.supported").as_deref(),
            Some("true")
        )
    }

    pub fn camera_fov_pointer(&self) -> Option<String> {
        self.get_runtime_info("camera.fov.pointer")
            .filter(|value| !value.is_empty())
    }

    pub fn entt_detected(&self) -> bool {
        matches!(
            self.get_runtime_info("entity.entt.detected").as_deref(),
            Some("true")
        )
    }

    pub fn entt_status(&self) -> Option<String> {
        self.get_runtime_info("entity.entt.status")
            .filter(|value| !value.is_empty())
    }

    pub fn entt_summary(&self) -> Option<String> {
        self.get_runtime_info("entity.entt.summary")
            .filter(|value| !value.is_empty())
    }

    pub fn entt_runtime(&self, key: &str) -> Option<String> {
        self.get_runtime_info(&format!("entity.entt.{key}"))
            .filter(|value| !value.is_empty())
    }

    pub fn entt_component_count(&self) -> Option<String> {
        self.get_runtime_info("entity.entt.component_count")
            .filter(|value| !value.is_empty())
    }

    pub fn entt_known_components(&self) -> Option<String> {
        self.get_runtime_info("entity.entt.known_components")
            .filter(|value| !value.is_empty())
    }

    pub fn entt_component_exists(&self, name: &str) -> bool {
        matches!(
            self.entt_runtime(&format!("component.{name}.exists"))
                .as_deref(),
            Some("true")
        )
    }

    pub fn entt_component_rva(&self, name: &str) -> Option<String> {
        self.entt_runtime(&format!("component.{name}.rva"))
    }

    pub fn entt_component_source(&self, name: &str) -> Option<String> {
        self.entt_runtime(&format!("component.{name}.source"))
    }

    pub fn entt_find_components(&self, prefix: &str) -> Option<String> {
        self.entt_runtime(&format!("find_components.{prefix}"))
    }

    pub fn path_exists(&self, path: &str) -> bool {
        match self.raw().path_exists {
            Some(f) => unsafe { f(str_view(path)) },
            None => false,
        }
    }

    pub fn create_dir(&self, path: &str) -> bool {
        match self.raw().create_dir {
            Some(f) => unsafe { f(str_view(path)) },
            None => false,
        }
    }

    pub fn read_text_file(&self, path: &str) -> Option<String> {
        let f = self.raw().read_text_file?;
        let len = unsafe { f(str_view(path), std::ptr::null_mut(), 0) };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len + 1];
        unsafe { f(str_view(path), buf.as_mut_ptr(), buf.len()) };
        let nul = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..nul]).to_string())
    }

    pub fn write_text_file(&self, path: &str, content: &str) -> i32 {
        match self.raw().write_text_file {
            Some(f) => unsafe { f(str_view(path), str_view(content)) },
            None => -1,
        }
    }

    pub fn ui_begin_window(&self, title: &str, open: Option<&mut bool>, flags: u32) -> bool {
        match self.raw().ui_begin_window {
            Some(f) => unsafe {
                let open_ptr = open.map(|v| v as *mut bool).unwrap_or(std::ptr::null_mut());
                f(str_view(title), open_ptr, flags)
            },
            None => false,
        }
    }

    pub fn ui_end_window(&self) {
        if let Some(f) = self.raw().ui_end_window {
            unsafe { f() };
        }
    }

    pub fn ui_text(&self, text: &str) {
        if let Some(f) = self.raw().ui_text {
            unsafe { f(str_view(text)) };
        }
    }

    pub fn ui_bullet_text(&self, text: &str) {
        if let Some(f) = self.raw().ui_bullet_text {
            unsafe { f(str_view(text)) };
        }
    }

    pub fn ui_button(&self, label: &str) -> bool {
        match self.raw().ui_button {
            Some(f) => unsafe { f(str_view(label)) },
            None => false,
        }
    }

    pub fn ui_checkbox(&self, label: &str, value: &mut bool) -> bool {
        match self.raw().ui_checkbox {
            Some(f) => unsafe { f(str_view(label), value as *mut bool) },
            None => false,
        }
    }

    pub fn ui_slider_float(&self, label: &str, value: &mut f32, min: f32, max: f32) -> bool {
        match self.raw().ui_slider_float {
            Some(f) => unsafe { f(str_view(label), value as *mut f32, min, max) },
            None => false,
        }
    }

    pub fn ui_drag_float(&self, label: &str, value: &mut f32, min: f32, max: f32) -> bool {
        match self.raw().ui_drag_float {
            Some(f) => unsafe { f(str_view(label), value as *mut f32, min, max) },
            None => false,
        }
    }

    pub fn ui_progress_bar(&self, label: &str, value: f32, min: f32, max: f32) {
        if let Some(f) = self.raw().ui_progress_bar {
            unsafe { f(str_view(label), value, min, max) };
        }
    }

    pub fn ui_separator(&self) {
        if let Some(f) = self.raw().ui_separator {
            unsafe { f() };
        }
    }

    pub fn ui_same_line(&self) {
        if let Some(f) = self.raw().ui_same_line {
            unsafe { f() };
        }
    }

    pub fn ui_show_toast(
        &self,
        title: &str,
        body: &str,
        anchor: ToastAnchor,
        kind: ToastKind,
        lifetime_seconds: f32,
    ) -> bool {
        match self.raw().ui_show_toast {
            Some(f) => unsafe {
                f(
                    str_view(title),
                    str_view(body),
                    toast_anchor_raw(anchor),
                    toast_kind_raw(kind),
                    lifetime_seconds,
                )
            },
            None => false,
        }
    }

    pub fn hud_begin_block(&self, id: &str, x: i32, y: i32) -> bool {
        match self.raw().hud_begin_block {
            Some(f) => unsafe { f(str_view(id), x, y) },
            None => false,
        }
    }

    pub fn hud_text_line(&self, text: &str) {
        if let Some(f) = self.raw().hud_text_line {
            unsafe { f(str_view(text)) };
        }
    }

    pub fn hud_end_block(&self) {
        if let Some(f) = self.raw().hud_end_block {
            unsafe { f() };
        }
    }

    pub fn arc_text_begin_block(&self, id: &str, x: i32, y: i32) -> bool {
        self.hud_begin_block(id, x, y)
    }

    pub fn arc_text_line(&self, text: &str) {
        self.hud_text_line(text);
    }

    pub fn arc_text_end_block(&self) {
        self.hud_end_block();
    }

    pub fn show_toast(
        &self,
        title: &str,
        body: &str,
        anchor: ToastAnchor,
        kind: ToastKind,
        lifetime_seconds: f32,
    ) -> bool {
        self.ui_show_toast(title, body, anchor, kind, lifetime_seconds)
    }
}

#[derive(Clone, Copy)]
pub struct Logging {
    loader: BLoader,
}

impl Logging {
    pub fn debug(&self, message: &str) {
        self.loader.debug(message);
    }

    pub fn info(&self, message: &str) {
        self.loader.info(message);
    }

    pub fn warn(&self, message: &str) {
        self.loader.warn(message);
    }

    pub fn error(&self, message: &str) {
        self.loader.error(message);
    }

    pub fn trace(&self, message: &str) {
        self.loader.trace(message);
    }
}

#[derive(Clone, Copy)]
pub struct Registry {
    loader: BLoader,
}

impl Registry {
    pub fn event(&self, name: &str, callback: BlEventCallback, user_data: *mut c_void) -> i32 {
        self.loader.register_event(name, callback, user_data)
    }

    pub fn ui_panel(&self, name: &str, callback: BlUiCallback, user_data: *mut c_void) -> i32 {
        self.loader.register_ui_panel(name, callback, user_data)
    }

    pub fn resource(
        &self,
        name: &str,
        callback: BlResourceCallback,
        user_data: *mut c_void,
    ) -> i32 {
        self.loader.register_resource(name, callback, user_data)
    }

    pub fn text_panel(&self, name: &str, callback: BlTextCallback, user_data: *mut c_void) -> i32 {
        self.loader.register_text_panel(name, callback, user_data)
    }

    pub fn feature_toggle(
        &self,
        registration: FeatureToggleRegistration<'_>,
        callback: BlFeatureToggleCallback,
        user_data: *mut c_void,
    ) -> i32 {
        self.loader
            .register_feature_toggle(registration, callback, user_data)
    }

    pub fn feature_panel(
        &self,
        registration: FeaturePanelRegistration<'_>,
        callback: BlUiCallback,
        user_data: *mut c_void,
    ) -> i32 {
        self.loader
            .register_feature_panel(registration, callback, user_data)
    }
}

#[derive(Clone, Copy)]
pub struct Runtime {
    loader: BLoader,
}

impl Runtime {
    pub fn info(&self, key: &str) -> Option<String> {
        self.loader.get_runtime_info(key)
    }

    pub fn info_or_default(&self, key: &str) -> String {
        self.info(key).unwrap_or_default()
    }

    pub fn host_version(&self) -> Option<String> {
        self.loader.get_host_version()
    }

    pub fn world_name(&self) -> Option<String> {
        self.loader.world_name()
    }

    pub fn world_source(&self) -> Option<String> {
        self.loader.world_source()
    }

    pub fn world_ready(&self) -> bool {
        self.loader.world_ready()
    }

    pub fn chunk_ready(&self) -> bool {
        self.loader.chunk_ready()
    }

    pub fn chunk_summary(&self) -> Option<String> {
        self.loader.chunk_summary()
    }

    pub fn mapping_summary(&self) -> Option<String> {
        self.loader.mapping_summary()
    }
}

#[derive(Clone, Copy)]
pub struct Paths {
    loader: BLoader,
}

impl Paths {
    pub fn get(&self, kind: u32) -> Option<String> {
        self.loader.get_path(kind)
    }

    pub fn game_dir(&self) -> Option<String> {
        self.get(BL_PATH_GAME_DIR).filter(|value| !value.is_empty())
    }

    pub fn mods_dir(&self) -> Option<String> {
        self.get(BL_PATH_MODS_DIR).filter(|value| !value.is_empty())
    }

    pub fn cache_dir(&self) -> Option<String> {
        self.get(BL_PATH_CACHE_DIR)
            .filter(|value| !value.is_empty())
    }

    pub fn ui_resource_pack_dir(&self) -> Option<String> {
        self.get(BL_PATH_UI_RESOURCE_PACK_DIR)
            .filter(|value| !value.is_empty())
    }
}

#[derive(Clone, Copy)]
pub struct FileSystem {
    loader: BLoader,
}

impl FileSystem {
    pub fn path_exists(&self, path: &str) -> bool {
        self.loader.path_exists(path)
    }

    pub fn create_dir(&self, path: &str) -> bool {
        self.loader.create_dir(path)
    }

    pub fn read_text_file(&self, path: &str) -> Option<String> {
        self.loader.read_text_file(path)
    }

    pub fn write_text_file(&self, path: &str, content: &str) -> i32 {
        self.loader.write_text_file(path, content)
    }
}

#[derive(Clone, Copy)]
pub struct Ui {
    loader: BLoader,
}

impl Ui {
    pub fn begin_window(&self, title: &str, open: Option<&mut bool>, flags: u32) -> bool {
        self.loader.ui_begin_window(title, open, flags)
    }

    pub fn end_window(&self) {
        self.loader.ui_end_window();
    }

    pub fn text(&self, text: &str) {
        self.loader.ui_text(text);
    }

    pub fn bullet_text(&self, text: &str) {
        self.loader.ui_bullet_text(text);
    }

    pub fn button(&self, label: &str) -> bool {
        self.loader.ui_button(label)
    }

    pub fn checkbox(&self, label: &str, value: &mut bool) -> bool {
        self.loader.ui_checkbox(label, value)
    }

    pub fn slider_float(&self, label: &str, value: &mut f32, min: f32, max: f32) -> bool {
        self.loader.ui_slider_float(label, value, min, max)
    }

    pub fn drag_float(&self, label: &str, value: &mut f32, min: f32, max: f32) -> bool {
        self.loader.ui_drag_float(label, value, min, max)
    }

    pub fn progress_bar(&self, label: &str, value: f32, min: f32, max: f32) {
        self.loader.ui_progress_bar(label, value, min, max);
    }

    pub fn separator(&self) {
        self.loader.ui_separator();
    }

    pub fn same_line(&self) {
        self.loader.ui_same_line();
    }

    pub fn show_toast(
        &self,
        title: &str,
        body: &str,
        anchor: ToastAnchor,
        kind: ToastKind,
        lifetime_seconds: f32,
    ) -> bool {
        self.loader
            .ui_show_toast(title, body, anchor, kind, lifetime_seconds)
    }
}

#[derive(Clone, Copy)]
pub struct Hud {
    loader: BLoader,
}

impl Hud {
    pub fn begin_block(&self, id: &str, x: i32, y: i32) -> bool {
        self.loader.hud_begin_block(id, x, y)
    }

    pub fn text_line(&self, text: &str) {
        self.loader.hud_text_line(text);
    }

    pub fn end_block(&self) {
        self.loader.hud_end_block();
    }
}

#[doc(hidden)]
pub fn __set_current_bloader(loader: BLoader) {
    let _ = CURRENT_BLOADER.set(loader);
}

pub fn current_bloader() -> Option<BLoader> {
    CURRENT_BLOADER.get().copied()
}

pub fn runtime_info(key: &str) -> Option<String> {
    current_bloader()?.get_runtime_info(key)
}

pub fn game_dir() -> Option<String> {
    path(BL_PATH_GAME_DIR).filter(|value| !value.is_empty())
}

pub fn mods_dir() -> Option<String> {
    path(BL_PATH_MODS_DIR).filter(|value| !value.is_empty())
}

pub fn cache_dir() -> Option<String> {
    path(BL_PATH_CACHE_DIR).filter(|value| !value.is_empty())
}

pub fn ui_resource_pack_dir() -> Option<String> {
    path(BL_PATH_UI_RESOURCE_PACK_DIR).filter(|value| !value.is_empty())
}

pub fn path(kind: u32) -> Option<String> {
    current_bloader()?.get_path(kind)
}

pub fn path_exists(path: &str) -> bool {
    current_bloader()
        .map(|loader| loader.path_exists(path))
        .unwrap_or(false)
}

pub fn create_dir(path: &str) -> bool {
    current_bloader()
        .map(|loader| loader.create_dir(path))
        .unwrap_or(false)
}

pub fn read_text_file(path: &str) -> Option<String> {
    current_bloader()?.read_text_file(path)
}

pub fn write_text_file(path: &str, content: &str) -> i32 {
    current_bloader()
        .map(|loader| loader.write_text_file(path, content))
        .unwrap_or(-1)
}

pub fn ui_text(text: &str) {
    if let Some(loader) = current_bloader() {
        loader.ui_text(text);
    }
}

pub fn ui_bullet_text(text: &str) {
    if let Some(loader) = current_bloader() {
        loader.ui_bullet_text(text);
    }
}

pub fn ui_separator() {
    if let Some(loader) = current_bloader() {
        loader.ui_separator();
    }
}

pub fn ui_same_line() {
    if let Some(loader) = current_bloader() {
        loader.ui_same_line();
    }
}

pub fn hud_text_line(text: &str) {
    if let Some(loader) = current_bloader() {
        loader.hud_text_line(text);
    }
}

pub fn show_toast(
    title: &str,
    body: &str,
    anchor: ToastAnchor,
    kind: ToastKind,
    lifetime_seconds: f32,
) -> bool {
    current_bloader()
        .map(|loader| loader.show_toast(title, body, anchor, kind, lifetime_seconds))
        .unwrap_or(false)
}

pub mod paths {
    pub use crate::{
        BL_PATH_CACHE_DIR, BL_PATH_GAME_DIR, BL_PATH_MODS_DIR, BL_PATH_UI_RESOURCE_PACK_DIR,
        cache_dir, game_dir, mods_dir, path, ui_resource_pack_dir,
    };
}

pub mod runtime {
    pub fn info(key: &str) -> Option<String> {
        crate::runtime_info(key)
    }

    pub fn info_or_default(key: &str) -> String {
        info(key).unwrap_or_default()
    }

    pub fn host_version() -> Option<String> {
        crate::current_bloader()?.get_host_version()
    }

    pub fn world_name() -> Option<String> {
        crate::current_bloader()?.world_name()
    }

    pub fn world_source() -> Option<String> {
        crate::current_bloader()?.world_source()
    }

    pub fn world_ready() -> bool {
        crate::current_bloader()
            .map(|loader| loader.world_ready())
            .unwrap_or(false)
    }
}

pub mod mapping {
    pub fn resolve(name: &str) -> usize {
        crate::current_bloader()
            .map(|loader| loader.resolve_symbol(name))
            .unwrap_or(0)
    }

    pub fn ready() -> bool {
        matches!(
            crate::runtime_info("mapping.ready").as_deref(),
            Some("true")
        )
    }

    pub fn pack_id() -> Option<String> {
        crate::runtime_info("mapping.pack_id").filter(|value| !value.is_empty())
    }

    pub fn public_symbols() -> Vec<String> {
        crate::runtime_info("mapping.public_symbols")
            .unwrap_or_default()
            .lines()
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn summary() -> Option<String> {
        crate::runtime_info("mapping.summary").filter(|value| !value.is_empty())
    }

    pub fn highlights() -> Option<String> {
        crate::runtime_info("mapping.highlights").filter(|value| !value.is_empty())
    }

    pub fn cache_path() -> Option<String> {
        crate::runtime_info("mapping.cache_path").filter(|value| !value.is_empty())
    }

    pub fn cache_status() -> Option<String> {
        crate::runtime_info("mapping.cache_status").filter(|value| !value.is_empty())
    }

    pub fn module_name() -> Option<String> {
        crate::runtime_info("mapping.module_name").filter(|value| !value.is_empty())
    }

    pub fn symbol_count() -> Option<u64> {
        crate::runtime_info("mapping.symbol_count").and_then(|value| value.parse().ok())
    }
}

/// Read-only client pointers captured by verified loader bindings.
///
/// A snapshot remains empty until BLoader has observed the corresponding client
/// lifecycle event. MODs must not retain these pointers across world changes.
pub mod client {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        pub client_instance: Option<usize>,
        pub local_player: Option<usize>,
        pub level: Option<usize>,
    }

    pub fn ready() -> bool {
        matches!(crate::runtime_info("client.ready").as_deref(), Some("true"))
    }

    pub fn local_player_ready() -> bool {
        matches!(
            crate::runtime_info("client.local_player_ready").as_deref(),
            Some("true")
        )
    }

    pub fn status() -> Option<String> {
        crate::runtime_info("client.status").filter(|value| !value.is_empty())
    }

    pub fn snapshot() -> Snapshot {
        Snapshot {
            client_instance: runtime_address("client.instance"),
            local_player: runtime_address("client.local_player"),
            level: runtime_address("client.level"),
        }
    }

    fn runtime_address(key: &str) -> Option<usize> {
        let value = crate::runtime_info(key)?;
        runtime_address_text(&value)
    }

    #[cfg(test)]
    mod tests {
        use super::runtime_address_text;

        #[test]
        fn parses_only_hex_runtime_addresses() {
            assert_eq!(runtime_address_text("0x140001000"), Some(0x140001000));
            assert_eq!(runtime_address_text(""), None);
            assert_eq!(runtime_address_text("not-an-address"), None);
        }
    }

    fn runtime_address_text(value: &str) -> Option<usize> {
        usize::from_str_radix(value.trim_start_matches("0x"), 16).ok()
    }
}

pub mod chunk {
    pub fn ready() -> bool {
        matches!(crate::runtime_info("chunk.ready").as_deref(), Some("true"))
    }

    pub fn summary() -> Option<String> {
        crate::runtime_info("chunk.summary").filter(|value| !value.is_empty())
    }

    pub fn current() -> Option<String> {
        crate::runtime_info("chunk.current").filter(|value| !value.is_empty())
    }

    pub fn block() -> Option<String> {
        crate::runtime_info("chunk.block").filter(|value| !value.is_empty())
    }

    pub fn local_block() -> Option<String> {
        crate::runtime_info("chunk.local_block").filter(|value| !value.is_empty())
    }

    pub fn border_distance() -> Option<String> {
        crate::runtime_info("chunk.border_distance").filter(|value| !value.is_empty())
    }

    pub fn level_pointer() -> Option<String> {
        crate::runtime_info("chunk.level_pointer").filter(|value| !value.is_empty())
    }

    pub fn block_source_pointer() -> Option<String> {
        crate::runtime_info("chunk.block_source_pointer").filter(|value| !value.is_empty())
    }
}

pub mod render3d {
    pub fn ready() -> bool {
        super::render3d_ready()
    }

    pub fn binding_ready() -> bool {
        matches!(
            crate::runtime_info("render3d.binding_ready").as_deref(),
            Some("true")
        )
    }

    pub fn status() -> Option<String> {
        crate::runtime_info("render3d.status").filter(|value| !value.is_empty())
    }

    pub fn line(
        level_render: usize,
        screen_context: usize,
        start: [f32; 3],
        end: [f32; 3],
        color: [f32; 4],
    ) -> bool {
        super::render3d_line(level_render, screen_context, start, end, color)
    }
}

pub mod fs {
    pub use crate::{create_dir, path_exists, read_text_file, write_text_file};
}

pub mod ui {
    pub fn begin_window(title: &str, open: Option<&mut bool>, flags: u32) -> bool {
        crate::current_bloader()
            .map(|loader| loader.ui_begin_window(title, open, flags))
            .unwrap_or(false)
    }

    pub fn end_window() {
        if let Some(loader) = crate::current_bloader() {
            loader.ui_end_window();
        }
    }

    pub fn text(text: &str) {
        crate::ui_text(text);
    }

    pub fn bullet_text(text: &str) {
        crate::ui_bullet_text(text);
    }

    pub fn button(label: &str) -> bool {
        crate::current_bloader()
            .map(|loader| loader.ui_button(label))
            .unwrap_or(false)
    }

    pub fn checkbox(label: &str, value: &mut bool) -> bool {
        crate::current_bloader()
            .map(|loader| loader.ui_checkbox(label, value))
            .unwrap_or(false)
    }

    pub fn slider_float(label: &str, value: &mut f32, min: f32, max: f32) -> bool {
        crate::current_bloader()
            .map(|loader| loader.ui_slider_float(label, value, min, max))
            .unwrap_or(false)
    }

    pub fn drag_float(label: &str, value: &mut f32, min: f32, max: f32) -> bool {
        crate::current_bloader()
            .map(|loader| loader.ui_drag_float(label, value, min, max))
            .unwrap_or(false)
    }

    pub fn progress_bar(label: &str, value: f32, min: f32, max: f32) {
        if let Some(loader) = crate::current_bloader() {
            loader.ui_progress_bar(label, value, min, max);
        }
    }

    pub fn separator() {
        crate::ui_separator();
    }

    pub fn same_line() {
        crate::ui_same_line();
    }

    pub fn show_toast(
        title: &str,
        body: &str,
        anchor: crate::ToastAnchor,
        kind: crate::ToastKind,
        lifetime_seconds: f32,
    ) -> bool {
        crate::current_bloader()
            .map(|loader| loader.ui_show_toast(title, body, anchor, kind, lifetime_seconds))
            .unwrap_or(false)
    }
}

pub mod hud {
    pub fn begin_block(id: &str, x: i32, y: i32) -> bool {
        crate::current_bloader()
            .map(|loader| loader.hud_begin_block(id, x, y))
            .unwrap_or(false)
    }

    pub fn text_line(text: &str) {
        crate::hud_text_line(text);
    }

    pub fn end_block() {
        if let Some(loader) = crate::current_bloader() {
            loader.hud_end_block();
        }
    }
}

pub mod arcui_text {
    pub fn begin_block(id: &str, x: i32, y: i32) -> bool {
        crate::current_bloader()
            .map(|loader| loader.arc_text_begin_block(id, x, y))
            .unwrap_or(false)
    }

    pub fn line(text: &str) {
        if let Some(loader) = crate::current_bloader() {
            loader.arc_text_line(text);
        }
    }

    pub fn end_block() {
        if let Some(loader) = crate::current_bloader() {
            loader.arc_text_end_block();
        }
    }
}

pub mod toast {
    pub use crate::{ToastAnchor, ToastKind};

    pub fn show(
        title: &str,
        body: &str,
        anchor: ToastAnchor,
        kind: ToastKind,
        lifetime_seconds: f32,
    ) -> bool {
        crate::current_bloader()
            .map(|loader| loader.show_toast(title, body, anchor, kind, lifetime_seconds))
            .unwrap_or(false)
    }
}

pub fn str_view(value: &str) -> BlStringView {
    BlStringView {
        ptr: value.as_ptr() as *const c_char,
        len: value.len(),
    }
}

pub mod mc {
    pub use crate::{
        BL_EVENT_BLOCK_ACTION, BL_EVENT_BOOTSTRAP_COMPLETE, BL_EVENT_CHAT,
        BL_EVENT_CLIENT_JOIN_LEVEL, BL_EVENT_CREATED_LEVEL, BL_EVENT_KEY,
        BL_EVENT_LOCAL_PLAYER_BOUND, BL_EVENT_PLAYER_ACTION, BL_EVENT_RENDER_3D,
        BL_EVENT_RENDER_FRAME, BL_EVENT_RESOURCE_RELOAD, BL_EVENT_SET_LOCAL_PLAYER_AS_INIT,
        BL_EVENT_SHUTDOWN, BL_EVENT_START_GAME_PACKET, BL_EVENT_TICK, BL_EVENT_UI_FRAME,
        BL_EVENT_WORLD_ENTER, BlBlockActionEvent, BlChatEvent, BlClientJoinLevelEvent,
        BlCreatedLevelEvent, BlEventCallback, BlKeyEvent, BlLocalPlayerBoundEvent,
        BlPlayerActionEvent, BlRender3DEvent, BlSetLocalPlayerAsInitEvent, BlStartGamePacketEvent,
        BlTickEvent, BlWorldEnterEvent,
    };
}

pub mod bl {
    pub use crate::{
        BL_API_VERSION_1, BL_LOG_DEBUG, BL_LOG_ERROR, BL_LOG_INFO, BL_LOG_WARN, BL_PATH_CACHE_DIR,
        BL_PATH_GAME_DIR, BL_PATH_MODS_DIR, BL_PATH_UI_RESOURCE_PACK_DIR, BL_REGISTRY_EVENT,
        BL_REGISTRY_RESOURCE, BL_REGISTRY_TEXT_PANEL, BL_REGISTRY_UI_PANEL, BLoader, BlCreateDirFn,
        BlD3D11RenderCallback, BlD3D12RenderCallback, BlGetHostVersionFn, BlGetPathFn,
        BlGetRuntimeInfoFn, BlHostApiV1, BlHudBeginBlockFn, BlHudEndBlockFn, BlHudTextLineFn,
        BlLogFn, BlModApiV1, BlModMainFn, BlOnLoadFn, BlOnUnloadFn, BlPathExistsFn,
        BlReadTextFileFn, BlRegisterBedrockScreenFn, BlRegisterFn, BlRequestBedrockScreenFn,
        BlResolveSymbolFn, BlResourceCallback, BlStringView, BlTextCallback, BlUiBeginWindowFn,
        BlUiBulletTextFn, BlUiButtonFn, BlUiCallback, BlUiCheckboxFn, BlUiEndWindowFn,
        BlUiSameLineFn, BlUiSeparatorFn, BlUiShowToastFn, BlUiSliderFloatFn, BlUiTextFn,
        BlWriteTextFileFn, ToastAnchor, ToastKind, register_d3d11_render_callback,
        register_d3d12_render_callback, render3d_line, render3d_ready, str_view,
    };
}

pub mod game {
    pub use crate::mc::*;
}

pub mod mod_api {
    pub use crate::bl::*;
}

pub mod project {
    pub use crate::tooling::{
        BlManifest, BlPackageMetadata, GeneratedManifestBundle, ResourceEntry, collect_resources,
        generate_manifest_bundle, load_package_metadata, write_manifest_bundle,
    };
}

pub mod i18n {
    /// Register a `.lang` payload for the current mod.
    pub fn register_lang(locale: &str, content: &str) -> bool {
        crate::register_mod_lang(locale, content)
    }

    /// Register a `.lang` file for the current mod by reading it from disk.
    pub fn register_lang_file(locale: &str, path: &str) -> bool {
        std::fs::read_to_string(path)
            .ok()
            .map(|content| register_lang(locale, &content))
            .unwrap_or(false)
    }

    /// Translate a key inside the current mod localization scope, then fall back to host keys.
    pub fn tr(key: &str) -> String {
        crate::translate(key)
    }

    pub fn tr_or(key: &str, fallback: &str) -> String {
        let value = tr(key);
        if value == key {
            fallback.to_string()
        } else {
            value
        }
    }

    pub fn current_locale() -> Option<String> {
        crate::i18n_current_locale()
    }
}

pub mod effects {
    //! Effects module API for MODs
    //!
    //! This module provides functions for registering render callbacks
    //! with the BLoader effects system.

    pub use crate::{BlD3D11RenderCallback, BlD3D12RenderCallback};

    /// 注册 D3D11 渲染回调
    ///
    /// 此函数允许 MOD 注册自己的 D3D11 渲染回调，在 Present 调用前执行
    ///
    /// 注意：此函数需要加载器支持。如果加载器未实现，调用将无效。
    pub fn register_d3d11_render_callback(callback: BlD3D11RenderCallback) {
        crate::register_d3d11_render_callback(callback);
    }

    /// 注册 D3D12 渲染回调
    ///
    /// 此函数允许 MOD 注册自己的 D3D12 渲染回调，在 Present 调用前执行
    ///
    /// 注意：此函数需要加载器支持。如果加载器未实现，调用将无效。
    pub fn register_d3d12_render_callback(callback: BlD3D12RenderCallback) {
        crate::register_d3d12_render_callback(callback);
    }
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {{
        if let Some(loader) = $crate::current_bloader() {
            loader.debug(&format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {{
        if let Some(loader) = $crate::current_bloader() {
            loader.info(&format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {{
        if let Some(loader) = $crate::current_bloader() {
            loader.warn(&format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {{
        if let Some(loader) = $crate::current_bloader() {
            loader.error(&format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {{
        if let Some(loader) = $crate::current_bloader() {
            loader.trace(&format!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! runtime_info {
    ($key:expr) => {{ $crate::runtime_info($key).unwrap_or_default() }};
}

#[macro_export]
macro_rules! ui_text {
    ($($arg:tt)*) => {{
        $crate::ui_text(&format!($($arg)*));
    }};
}

#[macro_export]
macro_rules! ui_bullet_text {
    ($($arg:tt)*) => {{
        $crate::ui_bullet_text(&format!($($arg)*));
    }};
}

#[macro_export]
macro_rules! hud_text_line {
    ($($arg:tt)*) => {{
        $crate::hud_text_line(&format!($($arg)*));
    }};
}

#[macro_export]
macro_rules! bl_export_mod {
    (
        on_load: $on_load:path,
        on_unload: $on_unload:path
    ) => {
        $crate::bl_export_mod!(
            mod_id: env!("BL_MOD_ID"),
            mod_name: env!("BL_MOD_NAME"),
            on_load: $on_load,
            on_unload: $on_unload
        );
    };
    (
        on_load: $on_load:path
    ) => {
        $crate::bl_export_mod!(
            mod_id: env!("BL_MOD_ID"),
            mod_name: env!("BL_MOD_NAME"),
            on_load: $on_load,
            on_unload: __bl_default_on_unload
        );
    };
    (
        mod_id: $mod_id:expr,
        mod_name: $mod_name:expr,
        on_load: $on_load:path,
        on_unload: $on_unload:path
    ) => {
        static BL_MOD_ID_BYTES: &[u8] = concat!($mod_id, "\0").as_bytes();
        static BL_MOD_NAME_BYTES: &[u8] = concat!($mod_name, "\0").as_bytes();

        unsafe extern "system" fn __bl_on_load(host: *const $crate::BlHostApiV1) -> i32 {
            let loader = $crate::BLoader::from_raw(host);
            $crate::__set_current_bloader(loader);
            $on_load(&loader)
        }

        unsafe extern "system" fn __bl_on_unload() {
            $on_unload()
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn bl_mod_main_v1(
            _host: *const $crate::BlHostApiV1,
        ) -> *const $crate::BlModApiV1 {
            static MOD_API: $crate::BlModApiV1 = $crate::BlModApiV1 {
                api_version: $crate::BL_API_VERSION_1,
                mod_id: $crate::BlStringView {
                    ptr: BL_MOD_ID_BYTES.as_ptr() as *const i8,
                    len: $mod_id.len(),
                },
                mod_name: $crate::BlStringView {
                    ptr: BL_MOD_NAME_BYTES.as_ptr() as *const i8,
                    len: $mod_name.len(),
                },
                on_load: Some(__bl_on_load),
                on_unload: Some(__bl_on_unload),
            };

            &MOD_API
        }
    };
}

// ==================== Effects 注册函数（由加载器实现） ====================

/// 运行时获取 bl_register_d3d11_render_callback 函数指针
fn resolve_loader_export(export: windows::core::PCSTR) -> Option<unsafe extern "system" fn()> {
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

    unsafe {
        // 优先从注入的 BLoader.dll 解析导出；找不到时再回退到主模块。
        if let Ok(module) = GetModuleHandleW(windows::core::w!("BLoader.dll")) {
            if let Some(proc) = GetProcAddress(module, export) {
                return Some(std::mem::transmute(proc));
            }
        }

        let module = GetModuleHandleW(None).ok()?;
        let proc = GetProcAddress(module, export)?;
        Some(std::mem::transmute(proc))
    }
}

fn encode_feature_toggle_registration(registration: FeatureToggleRegistration<'_>) -> String {
    let sanitize = |value: &str| value.replace(['\r', '\n'], " ").trim().to_string();
    format!(
        "{}\n{}\n{}\n{}",
        sanitize(registration.id),
        sanitize(registration.title),
        sanitize(registration.description),
        if registration.default_enabled {
            "1"
        } else {
            "0"
        }
    )
}

fn encode_feature_panel_registration(registration: FeaturePanelRegistration<'_>) -> String {
    let sanitize = |value: &str| value.replace(['\r', '\n'], " ").trim().to_string();
    format!(
        "{}\n{}\n{}",
        sanitize(registration.id),
        sanitize(registration.title),
        sanitize(registration.description),
    )
}

fn toast_anchor_raw(anchor: ToastAnchor) -> u32 {
    match anchor {
        ToastAnchor::TopLeft => BL_TOAST_TOP_LEFT,
        ToastAnchor::TopRight => BL_TOAST_TOP_RIGHT,
        ToastAnchor::BottomLeft => BL_TOAST_BOTTOM_LEFT,
        ToastAnchor::BottomRight => BL_TOAST_BOTTOM_RIGHT,
    }
}

fn toast_kind_raw(kind: ToastKind) -> u32 {
    match kind {
        ToastKind::Info => BL_TOAST_INFO,
        ToastKind::Success => BL_TOAST_SUCCESS,
        ToastKind::Warning => BL_TOAST_WARNING,
        ToastKind::Error => BL_TOAST_ERROR,
    }
}

fn get_bl_register_d3d11_render_callback()
-> Option<unsafe extern "system" fn(BlD3D11RenderCallback)> {
    resolve_loader_export(windows::core::s!("bl_register_d3d11_render_callback"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

/// 运行时获取 bl_register_d3d12_render_callback 函数指针
fn get_bl_register_d3d12_render_callback()
-> Option<unsafe extern "system" fn(BlD3D12RenderCallback)> {
    resolve_loader_export(windows::core::s!("bl_register_d3d12_render_callback"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_render3d_ready() -> Option<unsafe extern "system" fn() -> bool> {
    resolve_loader_export(windows::core::s!("bl_render3d_ready"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_render3d_line() -> Option<
    unsafe extern "system" fn(
        usize,
        usize,
        f32,
        f32,
        f32,
        f32,
        f32,
        f32,
        f32,
        f32,
        f32,
        f32,
    ) -> bool,
> {
    resolve_loader_export(windows::core::s!("bl_render3d_line"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_register_mod_lang() -> Option<BlRegisterModLangFn> {
    resolve_loader_export(windows::core::s!("bl_register_mod_lang"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_i18n_tr() -> Option<BlI18nTranslateFn> {
    resolve_loader_export(windows::core::s!("bl_i18n_tr"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_i18n_current_locale() -> Option<BlI18nCurrentLocaleFn> {
    resolve_loader_export(windows::core::s!("bl_i18n_current_locale"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_camera_zoom_set_enabled() -> Option<unsafe extern "system" fn(bool) -> bool> {
    resolve_loader_export(windows::core::s!("bl_camera_zoom_set_enabled"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_camera_zoom_set_percent() -> Option<unsafe extern "system" fn(u32) -> bool> {
    resolve_loader_export(windows::core::s!("bl_camera_zoom_set_percent"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_camera_zoom_get_enabled() -> Option<unsafe extern "system" fn() -> bool> {
    resolve_loader_export(windows::core::s!("bl_camera_zoom_get_enabled"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_camera_zoom_get_percent() -> Option<unsafe extern "system" fn() -> u32> {
    resolve_loader_export(windows::core::s!("bl_camera_zoom_get_percent"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_gamma_set_enabled() -> Option<unsafe extern "system" fn(bool) -> bool> {
    resolve_loader_export(windows::core::s!("bl_gamma_set_enabled"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_gamma_set_value() -> Option<unsafe extern "system" fn(f32) -> bool> {
    resolve_loader_export(windows::core::s!("bl_gamma_set_value"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_gamma_get_enabled() -> Option<unsafe extern "system" fn() -> bool> {
    resolve_loader_export(windows::core::s!("bl_gamma_get_enabled"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

fn get_bl_gamma_get_value() -> Option<unsafe extern "system" fn() -> f32> {
    resolve_loader_export(windows::core::s!("bl_gamma_get_value"))
        .map(|proc| unsafe { std::mem::transmute(proc) })
}

/// 注册 D3D12 渲染回调（由加载器实现）
///
/// 此函数由加载器在运行时提供，MOD 通过 bl_sdk::effects::register_d3d12_render_callback 调用
pub fn register_d3d12_render_callback(callback: BlD3D12RenderCallback) {
    if let Some(func) = get_bl_register_d3d12_render_callback() {
        unsafe { func(callback) };
    } else {
        // 无法获取函数指针，可能是加载器未完全初始化
        // 尝试直接使用空实现，避免崩溃
    }
}

/// 注册 D3D11 渲染回调（由加载器实现）
///
/// 此函数由加载器在运行时提供，MOD 通过 bl_sdk::effects::register_d3d11_render_callback 调用
pub fn register_d3d11_render_callback(callback: BlD3D11RenderCallback) {
    if let Some(func) = get_bl_register_d3d11_render_callback() {
        unsafe { func(callback) };
    } else {
        // 无法获取函数指针，可能是加载器未完全初始化
        // 尝试直接使用空实现，避免崩溃
    }
}

pub fn render3d_ready() -> bool {
    get_bl_render3d_ready()
        .map(|func| unsafe { func() })
        .unwrap_or(false)
}

pub fn render3d_line(
    level_render: usize,
    screen_context: usize,
    start: [f32; 3],
    end: [f32; 3],
    color: [f32; 4],
) -> bool {
    get_bl_render3d_line()
        .map(|func| unsafe {
            func(
                level_render,
                screen_context,
                start[0],
                start[1],
                start[2],
                end[0],
                end[1],
                end[2],
                color[0],
                color[1],
                color[2],
                color[3],
            )
        })
        .unwrap_or(false)
}

pub fn register_mod_lang(locale: &str, content: &str) -> bool {
    get_bl_register_mod_lang()
        .map(|func| unsafe { func(str_view(locale), str_view(content)) })
        .unwrap_or(false)
}

pub fn translate(key: &str) -> String {
    let Some(func) = get_bl_i18n_tr() else {
        return key.to_string();
    };

    let len = unsafe { func(str_view(key), std::ptr::null_mut(), 0) };
    let mut buf = vec![0u8; len + 1];
    unsafe {
        func(str_view(key), buf.as_mut_ptr(), buf.len());
    }
    let nul = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..nul]).to_string()
}

pub fn i18n_current_locale() -> Option<String> {
    let func = get_bl_i18n_current_locale()?;
    let len = unsafe { func(std::ptr::null_mut(), 0) };
    let mut buf = vec![0u8; len + 1];
    unsafe {
        func(buf.as_mut_ptr(), buf.len());
    }
    let nul = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..nul]).to_string())
}

pub mod camera {
    pub fn zoom_set_enabled(enabled: bool) -> bool {
        super::get_bl_camera_zoom_set_enabled()
            .map(|func| unsafe { func(enabled) })
            .unwrap_or(false)
    }

    pub fn zoom_set_percent(percent: u32) -> bool {
        super::get_bl_camera_zoom_set_percent()
            .map(|func| unsafe { func(percent) })
            .unwrap_or(false)
    }

    pub fn zoom_enabled() -> bool {
        super::get_bl_camera_zoom_get_enabled()
            .map(|func| unsafe { func() })
            .unwrap_or(false)
    }

    pub fn zoom_percent() -> u32 {
        super::get_bl_camera_zoom_get_percent()
            .map(|func| unsafe { func() })
            .unwrap_or(0)
    }
}

pub mod gamma {
    pub fn set_enabled(enabled: bool) -> bool {
        super::get_bl_gamma_set_enabled()
            .map(|func| unsafe { func(enabled) })
            .unwrap_or(false)
    }

    pub fn set_value(value: f32) -> bool {
        super::get_bl_gamma_set_value()
            .map(|func| unsafe { func(value) })
            .unwrap_or(false)
    }

    pub fn enabled() -> bool {
        super::get_bl_gamma_get_enabled()
            .map(|func| unsafe { func() })
            .unwrap_or(false)
    }

    pub fn value() -> f32 {
        super::get_bl_gamma_get_value()
            .map(|func| unsafe { func() })
            .unwrap_or(0.0)
    }
}

#[doc(hidden)]
pub fn __bl_default_on_unload() {}

#![allow(dead_code)]

use std::ffi::{c_char, c_void};

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

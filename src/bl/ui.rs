use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use arcui_core::widget::{WindowVisuals, text_size};
use arcui_core::{
    ButtonColors, Color, Frame, InputSnapshot, Key, Memory, Rect, Ui, Vec2, WindowOptions,
    animate_scalar, ease_out_cubic,
};
use arcui_hook::dx12 as arcui_dx12_hook;
use lucide_arcui::{icon_button_in_rect, icons as lucide_icons};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
};

use crate::bl::abi::{BlStringView, BlUiCallback};
use crate::bl::{cursor_capture, events, host, text_overlay};
use crate::config::{Config, HotkeyConfig};
use crate::runtime::foundation::logging;
use crate::runtime::foundation::logging::write_bootstrap_marker;

const ITEM_SPACING: f32 = 8.0;
const BUTTON_HEIGHT: f32 = 30.0;
const CHECKBOX_HEIGHT: f32 = 30.0;
const SLIDER_HEIGHT: f32 = 44.0;
const SLIDER_TRACK_HEIGHT: f32 = 8.0;
const PROGRESS_HEIGHT: f32 = 28.0;
const SWITCH_WIDTH: f32 = 42.0;
const SWITCH_HEIGHT: f32 = 24.0;
const BULLET_SIZE: f32 = 7.0;
const TEXT_SCALE: f32 = 2.0;
const OVERLAY_PANEL_WIDTH: f32 = 940.0;
const OVERLAY_PANEL_HEIGHT: f32 = 580.0;
const OVERLAY_PANEL_MIN_MARGIN: f32 = 36.0;
const OVERLAY_SCALE_MIN: f32 = 0.94;
const OVERLAY_SIDEBAR_WIDTH: f32 = 232.0;
const OVERLAY_SECTION_SPACING: f32 = 18.0;
const OVERLAY_CARD_SPACING: f32 = 14.0;
const OVERLAY_NAV_HEIGHT: f32 = 60.0;
const OVERLAY_HEADER_HEIGHT: f32 = 54.0;
const OVERLAY_CLOSE_SIZE: f32 = 30.0;
const OVERLAY_TITLE_BAR_HEIGHT: f32 = 28.0;
const OVERLAY_BADGE_HEIGHT: f32 = 26.0;
const OVERLAY_BUTTON_HEIGHT: f32 = 34.0;
const OVERLAY_CARD_HEIGHT: f32 = 88.0;
const OVERLAY_SETTINGS_CARD_HEIGHT: f32 = 72.0;
const OVERLAY_SETTINGS_SLIDER_CARD_HEIGHT: f32 = 120.0;
const OVERLAY_SETTINGS_NETWORK_CARD_HEIGHT: f32 = 186.0;
const OVERLAY_EMPTY_CARD_HEIGHT: f32 = 132.0;
const OVERLAY_LIST_TOP_OFFSET: f32 = 74.0;
const OVERLAY_TOAST_WIDTH: f32 = 332.0;
const OVERLAY_TOAST_MIN_HEIGHT: f32 = 72.0;
const OVERLAY_TOAST_STACK_GAP: f32 = 12.0;
const OVERLAY_TOAST_MAX_COUNT: usize = 7;
const OVERLAY_TOAST_ANIMATION_SECONDS: f32 = 0.64;
const OVERLAY_TOAST_ICON_SIZE: f32 = 42.0;
const OVERLAY_TOAST_PROGRESS_HEIGHT: f32 = 4.0;
const OVERLAY_SCROLLBAR_WIDTH: f32 = 6.0;
const OVERLAY_SCROLLBAR_MIN_HEIGHT: f32 = 42.0;
const OVERLAY_SCROLLBAR_GUTTER: f32 = 14.0;
const OVERLAY_ICON_BUTTON_SIZE: f32 = 34.0;
const MOD_CARD_HEIGHT: f32 = 114.0;

const TEXT_PRIMARY: Color = Color::rgba(240, 243, 248, 255);
const TEXT_SECONDARY: Color = Color::rgba(156, 169, 192, 255);
const TEXT_MUTED: Color = Color::rgba(100, 114, 140, 255);
const TEXT_ACCENT: Color = Color::rgba(139, 92, 246, 255); // Rich Royal Purple Accent
const TEXT_WARNING: Color = Color::rgba(255, 184, 108, 255);
const TEXT_SUCCESS: Color = Color::rgba(80, 250, 123, 255);
const OVERLAY_BACKDROP: Color = Color::rgba(8, 7, 13, 190); // Darker velvet backdrop
const OVERLAY_SIDEBAR_BG: Color = Color::rgba(15, 12, 22, 180); // Translucent Dark Velvet Purple
const OVERLAY_MAIN_BG: Color = Color::rgba(10, 8, 14, 135); // Glassmorphic translucent body
const OVERLAY_CARD_BG: Color = Color::rgba(255, 255, 255, 6); // Extremely clean translucent card
const OVERLAY_CARD_BG_HOVER: Color = Color::rgba(255, 255, 255, 12);
const OVERLAY_BORDER: Color = Color::rgba(255, 255, 255, 10);
const OVERLAY_BORDER_STRONG: Color = Color::rgba(139, 92, 246, 60); // Glowing purple border
const OVERLAY_NAV_ACTIVE: Color = Color::rgba(139, 92, 246, 40); // Velvet active nav link
const OVERLAY_NAV_HOVER: Color = Color::rgba(255, 255, 255, 8);
const OVERLAY_CLOSE_BG: Color = Color::rgba(255, 255, 255, 12);
const OVERLAY_CLOSE_HOVER: Color = Color::rgba(239, 68, 68, 220); // Premium Soft Red
const OVERLAY_CLOSE_ACTIVE: Color = Color::rgba(185, 28, 28, 240);
const BUTTON_COLORS: ButtonColors = ButtonColors::new(
    Color::rgba(139, 92, 246, 255),  // Solid royal purple buttons
    Color::rgba(167, 139, 250, 255), // Vibrant hover
    Color::rgba(124, 58, 237, 255),  // Darker active state
    Color::WHITE,
);

static UI_FRAME_ACTIVE: AtomicBool = AtomicBool::new(false);
static SHOW_OVERLAY: AtomicBool = AtomicBool::new(false);
static HOOKS_INSTALLED: AtomicBool = AtomicBool::new(false);
static FIRST_FRAME_SENT: AtomicBool = AtomicBool::new(false);
static MODULE_HANDLE: AtomicUsize = AtomicUsize::new(0);
static RESOURCE_RELOAD_COUNT: AtomicU64 = AtomicU64::new(0);
static BACKEND_NAME: OnceLock<Mutex<String>> = OnceLock::new();
static INSTALL_RETRY_STARTED: AtomicBool = AtomicBool::new(false);
static HOTKEY_BOOTSTRAP_STARTED: AtomicBool = AtomicBool::new(false);
static OVERLAY_TOGGLE_HELD: AtomicBool = AtomicBool::new(false);
static RESOURCE_RELOAD_HELD: AtomicBool = AtomicBool::new(false);
static OVERLAY_RENDER_LOGGED: AtomicBool = AtomicBool::new(false);
static INPUT_BARRIER_LOGGED: AtomicBool = AtomicBool::new(false);
static ARCUI_MEMORY: OnceLock<Mutex<Memory>> = OnceLock::new();
static COMPAT_STATE: OnceLock<Mutex<CompatState>> = OnceLock::new();
static OVERLAY_RUNTIME: OnceLock<Mutex<OverlayRuntimeState>> = OnceLock::new();
static GLOBAL_TOASTS: OnceLock<Mutex<Vec<GlobalToast>>> = OnceLock::new();

thread_local! {
    static ACTIVE_FRAME: Cell<*mut ActiveFrameAccess> = const { Cell::new(ptr::null_mut()) };
}

#[derive(Default, Clone)]
struct CompatState {
    active_slider: Option<u64>,
}

#[derive(Default, Clone)]
struct LayoutState {
    window_open: bool,
    owns_window: bool,
    current_window_id: u64,
    last_item_rect: Option<Rect>,
    pending_same_line: bool,
    row_top: f32,
    row_bottom: f32,
    inline_content_rect: Option<Rect>,
}

struct ActiveFrameAccess {
    ui: *mut c_void,
    input: *const InputSnapshot,
    layout: *mut LayoutState,
    compat: *mut CompatState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OverlayTab {
    Features,
    Mods,
    Settings,
}

impl OverlayTab {
    const ALL: [OverlayTab; 3] = [OverlayTab::Features, OverlayTab::Mods, OverlayTab::Settings];

    fn index(self) -> usize {
        match self {
            OverlayTab::Features => 0,
            OverlayTab::Mods => 1,
            OverlayTab::Settings => 2,
        }
    }

    fn title_key(self) -> &'static str {
        match self {
            OverlayTab::Features => "overlay.page.features.title",
            OverlayTab::Mods => "overlay.page.mods.title",
            OverlayTab::Settings => "overlay.page.settings.title",
        }
    }

    fn nav_title_key(self) -> &'static str {
        match self {
            OverlayTab::Features => "overlay.nav.features.title",
            OverlayTab::Mods => "overlay.nav.mods.title",
            OverlayTab::Settings => "overlay.nav.settings.title",
        }
    }

    fn nav_subtitle_key(self) -> &'static str {
        match self {
            OverlayTab::Features => "overlay.nav.features.subtitle",
            OverlayTab::Mods => "overlay.nav.mods.subtitle",
            OverlayTab::Settings => "overlay.nav.settings.subtitle",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HotkeyField {
    Toggle,
    Reload,
}

struct OverlayToast {
    title: String,
    body: String,
    anchor: ToastAnchor,
    kind: ToastKind,
    lifetime_seconds: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastAnchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

struct GlobalToast {
    payload: OverlayToast,
    phase_age: f32,
    closing: bool,
}

struct OverlayRuntimeState {
    config: Config,
    active_tab: OverlayTab,
    recording_hotkey: Option<HotkeyField>,
    visibility: f32,
    indicator_y: f32,
    active_feature_panel: Option<(String, String)>,
    features_scroll: f32,
    mods_scroll: f32,
    settings_scroll: f32,
}

impl OverlayRuntimeState {
    fn new(config: Config) -> Self {
        Self {
            config,
            active_tab: OverlayTab::Features,
            recording_hotkey: None,
            visibility: 0.0,
            indicator_y: 0.0,
            active_feature_panel: None,
            features_scroll: 0.0,
            mods_scroll: 0.0,
            settings_scroll: 0.0,
        }
    }

    fn set_toast(&mut self, text: impl Into<String>, is_error: bool) {
        push_global_toast(OverlayToast {
            title: String::new(),
            body: text.into(),
            anchor: ToastAnchor::TopRight,
            kind: if is_error {
                ToastKind::Error
            } else {
                ToastKind::Success
            },
            lifetime_seconds: 2.4,
        });
    }
}

pub fn set_module_handle(raw_module: usize) {
    MODULE_HANDLE.store(raw_module, Ordering::SeqCst);
}

pub fn get_module_handle() -> usize {
    MODULE_HANDLE.load(Ordering::SeqCst)
}

pub fn install_hooks() -> bool {
    if HOOKS_INSTALLED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return true;
    }

    let config = crate::config::Config::load();
    crate::core::global_input::initialize_global_input();
    cursor_capture::set_allowed_hotkey(
        config.overlay.toggle_hotkey.key,
        config.overlay.toggle_hotkey.alt,
        config.overlay.toggle_hotkey.ctrl,
        config.overlay.toggle_hotkey.shift,
    );
    cursor_capture::initialize();
    {
        let mut runtime = overlay_runtime().lock().unwrap_or_else(|e| e.into_inner());
        runtime.config = config.clone();
    }
    logging::info_message("BL overlay installing ArcUI backend: dx12");
    set_backend_name("arcui-dx12-installing");

    if config.enable_dx11 {
        crate::core::d3d12_queue::initialize_d3d11_device();
    }

    match arcui_dx12_hook::install(build_draw_data) {
        Ok(()) => {
            write_bootstrap_marker("overlay.arcui.installed dx12");
            logging::info_message("BL overlay installed via ArcUI backend: dx12.");
            set_backend_name("arcui-dx12");
            true
        }
        Err(error) => {
            HOOKS_INSTALLED.store(false, Ordering::SeqCst);
            set_backend_name("arcui-dx12-error");
            logging::warn_message(&format!("ArcUI dx12 install failed: {error}"));
            false
        }
    }
}

pub fn spawn_install_retry() {
    if INSTALL_RETRY_STARTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    thread::spawn(|| {
        let start = Instant::now();
        let timeout = Duration::from_secs(120);
        let mut attempts = 0u32;

        while start.elapsed() < timeout {
            attempts += 1;
            if install_hooks() {
                write_bootstrap_marker(&format!(
                    "overlay.arcui.retry_installed attempt={attempts}"
                ));
                return;
            }

            if attempts == 1 || attempts % 5 == 0 {
                write_bootstrap_marker(&format!("overlay.arcui.retry_wait attempt={attempts}"));
            }
            thread::sleep(Duration::from_secs(2));
        }

        write_bootstrap_marker("overlay.arcui.retry_timeout");
        INSTALL_RETRY_STARTED.store(false, Ordering::SeqCst);
    });
}

pub fn spawn_hotkey_bootstrap() {
    if HOTKEY_BOOTSTRAP_STARTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    thread::spawn(|| {
        let mut toggle_held = false;

        loop {
            if HOOKS_INSTALLED.load(Ordering::Acquire) {
                return;
            }

            let toggle_hotkey = {
                let runtime = overlay_runtime().lock().unwrap_or_else(|e| e.into_inner());
                runtime.config.overlay.toggle_hotkey
            };

            let game_focused = crate::core::global_input::is_game_focused();
            let toggle_down = game_focused && is_hotkey_down(toggle_hotkey);
            if toggle_down && !toggle_held {
                logging::info_message(
                    "BLoader overlay hotkey pressed; installing ArcUI hook on demand.",
                );
                if install_hooks() {
                    set_overlay_visibility(true);
                    return;
                }
                logging::warn_message(
                    "BLoader overlay hotkey install failed; overlay remains unavailable.",
                );
            }
            toggle_held = toggle_down;

            thread::sleep(Duration::from_millis(16));
        }
    });
}

pub fn backend_name() -> String {
    BACKEND_NAME
        .get_or_init(|| Mutex::new("arcui-dx12-uninitialized".to_string()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub fn overlay_visibility() -> bool {
    SHOW_OVERLAY.load(Ordering::Relaxed)
}

fn memory() -> &'static Mutex<Memory> {
    ARCUI_MEMORY.get_or_init(|| Mutex::new(Memory::default()))
}

fn compat_state() -> &'static Mutex<CompatState> {
    COMPAT_STATE.get_or_init(|| Mutex::new(CompatState::default()))
}

fn overlay_runtime() -> &'static Mutex<OverlayRuntimeState> {
    OVERLAY_RUNTIME.get_or_init(|| Mutex::new(OverlayRuntimeState::new(Config::load())))
}

fn global_toasts() -> &'static Mutex<Vec<GlobalToast>> {
    GLOBAL_TOASTS.get_or_init(|| Mutex::new(Vec::new()))
}

fn has_pending_global_toasts() -> bool {
    !global_toasts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
}

fn push_global_toast(toast: OverlayToast) {
    let mut toasts = global_toasts().lock().unwrap_or_else(|e| e.into_inner());
    if toasts.len() >= OVERLAY_TOAST_MAX_COUNT {
        toasts.remove(0);
    }
    toasts.push(GlobalToast {
        payload: toast,
        phase_age: 0.0,
        closing: false,
    });
}

fn build_draw_data(input: &InputSnapshot) -> arcui_core::DrawData {
    poll_host_hotkeys();
    let overlay_visible = SHOW_OVERLAY.load(Ordering::Relaxed);
    let overlay_animating = {
        let runtime = overlay_runtime().lock().unwrap_or_else(|e| e.into_inner());
        runtime.visibility > 0.01
    };
    let has_toasts = has_pending_global_toasts();
    let has_text_panels = host::has_text_panels();
    if !overlay_visible && !overlay_animating {
        arcui_dx12_hook::set_loader_blur_region(None, 0.0);
        arcui_dx12_hook::set_capture_region(None);
        arcui_dx12_hook::set_capture_input(false);
        cursor_capture::set_overlay_active(false);
    }
    let overlay_blocks_host_ui = overlay_visible || overlay_animating;

    if !overlay_visible
        && !overlay_animating
        && !has_toasts
        && !has_text_panels
        && !crate::bl::loader_status::is_enabled()
    {
        host::dispatch_render_frame();
        events::emit_frame_tick();

        if !FIRST_FRAME_SENT.swap(true, Ordering::SeqCst) {
            write_bootstrap_marker("overlay.arcui.initialize arcui-dx12");
            logging::info_message("BL overlay attached via ArcUI backend arcui-dx12.");
        }

        let mut guard = memory().lock().unwrap_or_else(|e| e.into_inner());
        return Frame::begin(input.clone(), &mut guard).finish();
    }

    if !host::should_attach_overlay() {
        arcui_dx12_hook::set_loader_blur_region(None, 0.0);
        arcui_dx12_hook::set_capture_region(None);
        arcui_dx12_hook::set_capture_input(false);
        cursor_capture::set_overlay_active(false);
        let mut guard = memory().lock().unwrap_or_else(|e| e.into_inner());
        return Frame::begin(input.clone(), &mut guard).finish();
    }

    let mut memory_guard = memory().lock().unwrap_or_else(|e| e.into_inner());
    let mut compat_guard = compat_state().lock().unwrap_or_else(|e| e.into_inner());
    let mut frame = Frame::begin(input.clone(), &mut memory_guard);
    let mut layout = LayoutState::default();

    {
        let ui = frame.ui();
        let mut access = ActiveFrameAccess {
            ui: ui as *mut Ui<'_> as *mut c_void,
            input,
            layout: &mut layout,
            compat: &mut *compat_guard,
        };

        UI_FRAME_ACTIVE.store(true, Ordering::SeqCst);
        ACTIVE_FRAME.with(|slot| slot.set(&mut access as *mut ActiveFrameAccess));

        if !overlay_blocks_host_ui {
            host::dispatch_ui_frame();
        }
        if overlay_visible || overlay_animating {
            render_host_overlay(ui, input, &mut layout, &mut compat_guard);
        }
        text_overlay::render_in_arcui(ui);
        crate::bl::loader_status::render_in_arcui(ui);
        render_global_toasts(ui, input);

        ACTIVE_FRAME.with(|slot| slot.set(ptr::null_mut()));
        UI_FRAME_ACTIVE.store(false, Ordering::SeqCst);
    }

    host::dispatch_render_frame();
    events::emit_frame_tick();

    if !FIRST_FRAME_SENT.swap(true, Ordering::SeqCst) {
        write_bootstrap_marker("overlay.arcui.initialize arcui-dx12");
        logging::info_message("BL overlay attached via ArcUI backend arcui-dx12.");
    }

    set_backend_name("arcui-dx12");
    frame.finish()
}

fn key_to_virtual_key(key: arcui_core::Key) -> Option<u32> {
    Some(match key {
        arcui_core::Key::Insert => 0x2D,
        arcui_core::Key::Escape => 0x1B,
        arcui_core::Key::Enter => 0x0D,
        arcui_core::Key::Tab => 0x09,
        arcui_core::Key::Backspace => 0x08,
        arcui_core::Key::Delete => 0x2E,
        arcui_core::Key::Space => 0x20,
        arcui_core::Key::F1 => 0x70,
        arcui_core::Key::F2 => 0x71,
        arcui_core::Key::F3 => 0x72,
        arcui_core::Key::F4 => 0x73,
        arcui_core::Key::F5 => 0x74,
        arcui_core::Key::F6 => 0x75,
        arcui_core::Key::F7 => 0x76,
        arcui_core::Key::F8 => 0x77,
        arcui_core::Key::F9 => 0x78,
        arcui_core::Key::F10 => 0x79,
        arcui_core::Key::F11 => 0x7A,
        arcui_core::Key::F12 => 0x7B,
        arcui_core::Key::Left => 0x25,
        arcui_core::Key::Right => 0x27,
        arcui_core::Key::Up => 0x26,
        arcui_core::Key::Down => 0x28,
        arcui_core::Key::Home => 0x24,
        arcui_core::Key::End => 0x23,
        arcui_core::Key::Character(ch) => ch.to_ascii_uppercase() as u32,
    })
}

fn set_overlay_visibility(visible: bool) {
    if visible && !crate::core::global_input::is_game_focused() {
        logging::info_message(
            "BL overlay open request ignored because the Minecraft window is not foreground.",
        );
        return;
    }

    let previous = SHOW_OVERLAY.swap(visible, Ordering::SeqCst);
    if previous == visible {
        return;
    }

    if !visible {
        if let Ok(mut runtime) = overlay_runtime().lock() {
            runtime.recording_hotkey = None;
        }
    }

    write_bootstrap_marker(&format!("overlay.visibility toggled visible={visible}"));
    logging::info_message(&format!("BL overlay visibility toggled: {visible}"));
}

fn toggle_overlay_visibility() {
    set_overlay_visibility(!SHOW_OVERLAY.load(Ordering::SeqCst))
}

fn is_hotkey_down(hotkey: HotkeyConfig) -> bool {
    if hotkey.key == 0 {
        return false;
    }

    let alt_down = unsafe { (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0 };
    let ctrl_down = unsafe { (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0 };
    let shift_down = unsafe { (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0 };
    if alt_down != hotkey.alt || ctrl_down != hotkey.ctrl || shift_down != hotkey.shift {
        return false;
    }

    unsafe { (GetAsyncKeyState(hotkey.key as i32) as u16 & 0x8000) != 0 }
}

fn request_resource_reload() {
    let next = RESOURCE_RELOAD_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    write_bootstrap_marker("overlay.resource_reload.requested");
    crate::bl::bedrock_ui::request_screen("bloader:overlay");
    host::dispatch_resource_reload(next as u32);
}

fn poll_host_hotkeys() {
    if !crate::core::global_input::is_game_focused() {
        // GetAsyncKeyState is global to the desktop. Without this gate, pressing
        // the configured key in another application can open the injected panel.
        // Also close an already-open panel so it cannot retain cursor/input state
        // while another desktop application is foreground.
        OVERLAY_TOGGLE_HELD.store(false, Ordering::SeqCst);
        RESOURCE_RELOAD_HELD.store(false, Ordering::SeqCst);
        if SHOW_OVERLAY.load(Ordering::Acquire) {
            set_overlay_visibility(false);
        }
        return;
    }

    let (toggle_hotkey, reload_hotkey, recording_hotkey) = {
        let runtime = overlay_runtime().lock().unwrap_or_else(|e| e.into_inner());
        (
            runtime.config.overlay.toggle_hotkey,
            runtime.config.overlay.reload_hotkey,
            runtime.recording_hotkey,
        )
    };

    let overlay_down = recording_hotkey.is_none() && is_hotkey_down(toggle_hotkey);
    let overlay_was_held = OVERLAY_TOGGLE_HELD.swap(overlay_down, Ordering::SeqCst);
    if overlay_down && !overlay_was_held {
        toggle_overlay_visibility();
    }

    let reload_down = recording_hotkey.is_none() && is_hotkey_down(reload_hotkey);
    let reload_was_held = RESOURCE_RELOAD_HELD.swap(reload_down, Ordering::SeqCst);
    if reload_down && !reload_was_held {
        request_resource_reload();
    }
}

fn render_host_overlay(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    layout: &mut LayoutState,
    compat: &mut CompatState,
) {
    let mut runtime = overlay_runtime().lock().unwrap_or_else(|e| e.into_inner());
    let target_visible = SHOW_OVERLAY.load(Ordering::Relaxed);
    let speed = if target_visible { 10.0 } else { 18.0 };
    runtime.visibility = animate_scalar(
        runtime.visibility,
        if target_visible { 1.0 } else { 0.0 },
        speed,
        input.delta_seconds,
    );

    if runtime.visibility <= 0.01 && !target_visible {
        arcui_dx12_hook::set_loader_blur_region(None, 0.0);
        arcui_dx12_hook::set_capture_region(None);
        arcui_dx12_hook::set_capture_input(false);
        cursor_capture::set_overlay_active(false);
        OVERLAY_RENDER_LOGGED.store(false, Ordering::Release);
        INPUT_BARRIER_LOGGED.store(false, Ordering::Release);
        return;
    }

    let eased_visibility = if target_visible {
        ease_out_back(runtime.visibility)
    } else {
        ease_out_cubic(runtime.visibility) // Smooth start, accelerating exit
    };
    let options = centered_overlay_window_options(input.display_size, eased_visibility);
    let window_rect = Rect::from_min_size(options.position, options.size);
    let blur_rect = overlay_blur_rect(input.display_size, window_rect, eased_visibility);
    arcui_dx12_hook::set_loader_blur_strength(runtime.config.overlay.blur_strength());
    arcui_dx12_hook::set_loader_blur_region(Some(blur_rect), eased_visibility);
    draw_overlay_backdrop(
        ui,
        input.display_size,
        window_rect,
        blur_rect,
        eased_visibility,
    );

    let mut close_overlay = false;
    let mut request_reload = false;
    if target_visible && runtime.recording_hotkey.is_none() && input.key_pressed(Key::Escape) {
        close_overlay = true;
    }

    let title = crate::runtime::foundation::i18n::tr("overlay.window_title");
    if !compat_begin_window(ui, layout, &title, None, 0, Some(options)) {
        arcui_dx12_hook::set_capture_region(None);
        arcui_dx12_hook::set_capture_input(false);
        cursor_capture::set_overlay_active(false);
        if !OVERLAY_RENDER_LOGGED.swap(true, Ordering::AcqRel) {
            logging::warn_message("BL overlay failed to begin ArcUI window.");
        }
        return;
    }
    if !OVERLAY_RENDER_LOGGED.swap(true, Ordering::AcqRel) {
        logging::info_message(&format!(
            "BL overlay ArcUI window rendering rect=({}, {}) {}x{} display={}x{}",
            window_rect.min.x.round() as i32,
            window_rect.min.y.round() as i32,
            window_rect.width().round() as i32,
            window_rect.height().round() as i32,
            input.display_size.x.round() as i32,
            input.display_size.y.round() as i32
        ));
    }
    // Keep the capture geometry on the actual ArcUI panel. The input barrier
    // remains full-window while active, but the delayed 100 ms cursor hand-off
    // now lands in the visible panel rather than merely the game viewport.
    arcui_dx12_hook::set_capture_region(Some(window_rect));
    cursor_capture::set_allowed_hotkey(
        runtime.config.overlay.toggle_hotkey.key,
        runtime.config.overlay.toggle_hotkey.alt,
        runtime.config.overlay.toggle_hotkey.ctrl,
        runtime.config.overlay.toggle_hotkey.shift,
    );
    // Arm the in-process Raw Input/GameInput barriers before ArcUI exposes the
    // interactive cursor. This removes the one-frame first-click window.
    cursor_capture::set_overlay_active(true);
    arcui_dx12_hook::set_capture_input(true);
    if !INPUT_BARRIER_LOGGED.swap(true, Ordering::AcqRel) {
        let hwnd = arcui_dx12_hook::host_hwnd_raw();
        if hwnd == 0 {
            logging::warn_message(
                "[overlay-input] ArcUI is active but no Minecraft input HWND was resolved",
            );
        } else {
            logging::info_message(&format!(
                "[overlay-input] ArcUI input HWND resolved: 0x{hwnd:X}; WndProc/RawInput/GameInput barriers active"
            ));
        }
    }

    let body = ui.current_window_body_rect();
    let sidebar_rect = Rect::from_min_max(
        body.min,
        Vec2::new(
            body.min.x + OVERLAY_SIDEBAR_WIDTH.min(body.width() * 0.38),
            body.max.y,
        ),
    );
    let main_rect = Rect::from_min_max(Vec2::new(sidebar_rect.max.x + 14.0, body.min.y), body.max);
    draw_overlay_shell(ui, sidebar_rect, main_rect, eased_visibility);

    let sidebar_inner = sidebar_rect.shrink(18.0);
    let nav_top = sidebar_inner.min.y + 12.0;

    let nav_width = sidebar_inner.width();
    let mut active_target_y = nav_top;
    for tab in OverlayTab::ALL {
        let index = tab.index() as f32;
        let rect = Rect::from_min_size(
            Vec2::new(
                sidebar_inner.min.x,
                nav_top + index * (OVERLAY_NAV_HEIGHT + 10.0),
            ),
            Vec2::new(nav_width, OVERLAY_NAV_HEIGHT),
        );
        if tab == runtime.active_tab {
            active_target_y = rect.min.y;
        }
        if render_sidebar_tab(ui, input, rect, tab, tab == runtime.active_tab) {
            runtime.active_tab = tab;
        }
    }

    if runtime.indicator_y <= 0.0 {
        runtime.indicator_y = active_target_y;
    }
    runtime.indicator_y = animate_scalar(
        runtime.indicator_y,
        active_target_y,
        20.0,
        input.delta_seconds,
    );
    let indicator_rect = Rect::from_min_size(
        Vec2::new(sidebar_rect.min.x + 8.0, runtime.indicator_y + 8.0),
        Vec2::new(4.0, OVERLAY_NAV_HEIGHT - 16.0),
    );
    ui.rounded_rect(indicator_rect, TEXT_ACCENT, 2.0);

    let main_inner = main_rect.shrink(24.0);
    let title_close_size = 24.0;
    let close_rect = Rect::from_min_size(
        Vec2::new(
            options.position.x + options.size.x - title_close_size - 10.0,
            options.position.y + ((OVERLAY_TITLE_BAR_HEIGHT - title_close_size) * 0.5).floor(),
        ),
        Vec2::splat(title_close_size),
    );
    if render_close_button(ui, input, close_rect) {
        close_overlay = true;
    }

    draw_page_header(ui, main_inner, &runtime);
    let content_rect = Rect::from_min_max(
        Vec2::new(main_inner.min.x, main_inner.min.y + OVERLAY_LIST_TOP_OFFSET),
        main_inner.max,
    );

    match runtime.active_tab {
        OverlayTab::Features => {
            let (next_panel, next_scroll) = render_feature_toggle_page(
                ui,
                input,
                content_rect,
                runtime.active_feature_panel.clone(),
                runtime.features_scroll,
                eased_visibility,
            );
            runtime.active_feature_panel = next_panel;
            runtime.features_scroll = next_scroll;
        }
        OverlayTab::Mods => render_loaded_mods_page(
            ui,
            input,
            content_rect,
            &mut runtime.mods_scroll,
            eased_visibility,
        ),
        OverlayTab::Settings => {
            let next_scroll = render_settings_page(
                ui,
                input,
                content_rect,
                runtime.settings_scroll,
                eased_visibility,
                &mut runtime,
                &mut request_reload,
            );
            runtime.settings_scroll = next_scroll;
        }
    }

    if let Some(field) = runtime.recording_hotkey {
        if input.key_pressed(Key::Escape) {
            runtime.recording_hotkey = None;
            runtime.set_toast(
                crate::runtime::foundation::i18n::tr("overlay.settings.hotkey.cancelled"),
                false,
            );
        } else if let Some(hotkey) = capture_hotkey(input) {
            match field {
                HotkeyField::Toggle => runtime.config.overlay.toggle_hotkey = hotkey,
                HotkeyField::Reload => runtime.config.overlay.reload_hotkey = hotkey,
            }

            runtime.recording_hotkey = None;
            if runtime.config.save().is_ok() {
                runtime.set_toast(
                    format!(
                        "{} {}",
                        crate::runtime::foundation::i18n::tr("overlay.settings.hotkey.saved"),
                        hotkey_to_string(hotkey)
                    ),
                    false,
                );
            } else {
                runtime.set_toast(
                    crate::runtime::foundation::i18n::tr("overlay.settings.save_failed"),
                    true,
                );
            }
        }
    }

    compat_end_window(ui, layout, compat);
    drop(runtime);

    if request_reload {
        request_resource_reload();
    }
    if close_overlay {
        set_overlay_visibility(false);
    }
}

fn centered_overlay_window_options(display_size: Vec2, visibility: f32) -> WindowOptions {
    let max_width = (display_size.x - OVERLAY_PANEL_MIN_MARGIN * 2.0).max(320.0);
    let max_height = (display_size.y - OVERLAY_PANEL_MIN_MARGIN * 2.0).max(280.0);
    let base_width = OVERLAY_PANEL_WIDTH.min(max_width);
    let base_height = OVERLAY_PANEL_HEIGHT.min(max_height);
    let scale = 0.65 + (0.35 * visibility);
    let min_width = max_width.min(520.0) * scale;
    let min_height = max_height.min(340.0) * scale;
    let size = Vec2::new(
        (base_width * scale).max(min_width),
        (base_height * scale).max(min_height),
    );
    let position = Vec2::new(
        ((display_size.x - size.x) * 0.5).max(OVERLAY_PANEL_MIN_MARGIN),
        ((display_size.y - size.y) * 0.5).max(OVERLAY_PANEL_MIN_MARGIN),
    );

    WindowOptions {
        position,
        size,
        movable: false,
        resizable: false,
        visuals: WindowVisuals {
            body: with_alpha(Color::rgba(8, 12, 18, 6), visibility * 0.58),
            title_bar: with_alpha(Color::rgba(56, 123, 255, 244), 0.88 + visibility * 0.12),
            title_text: Color::WHITE,
            shadow: false,
            resize_grip: false,
        },
    }
}

fn overlay_blur_rect(display_size: Vec2, window_rect: Rect, visibility: f32) -> Rect {
    let _ = window_rect;
    let _ = visibility;
    Rect::from_min_size(Vec2::new(0.0, 0.0), display_size)
}

fn draw_overlay_backdrop(
    ui: &mut Ui<'_>,
    display_size: Vec2,
    window_rect: Rect,
    _blur_rect: Rect,
    visibility: f32,
) {
    let full_rect = Rect::from_min_size(Vec2::new(0.0, 0.0), display_size);
    let _ = ui.button_in_rect(
        "overlay-fullscreen-blocker",
        full_rect,
        "",
        ButtonColors::new(
            Color::TRANSPARENT,
            Color::TRANSPARENT,
            Color::TRANSPARENT,
            Color::TRANSPARENT,
        ),
    );
    ui.filled_rect(full_rect, with_alpha(OVERLAY_BACKDROP, visibility * 0.9));
    let halo_rect = Rect::from_min_max(
        Vec2::new(window_rect.min.x - 10.0, window_rect.min.y - 10.0),
        Vec2::new(window_rect.max.x + 10.0, window_rect.max.y + 10.0),
    );
    ui.rounded_rect(
        halo_rect,
        with_alpha(Color::rgba(255, 255, 255, 8), visibility * 0.65),
        18.0,
    );
}

fn draw_overlay_shell(ui: &mut Ui<'_>, sidebar_rect: Rect, main_rect: Rect, visibility: f32) {
    draw_panel_surface(
        ui,
        sidebar_rect,
        with_alpha(OVERLAY_SIDEBAR_BG, visibility),
        with_alpha(OVERLAY_BORDER, visibility),
        16.0,
    );
    draw_panel_surface(
        ui,
        main_rect,
        with_alpha(OVERLAY_MAIN_BG, visibility),
        with_alpha(OVERLAY_BORDER, visibility),
        16.0,
    );
}

fn render_sidebar_tab(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    rect: Rect,
    tab: OverlayTab,
    active: bool,
) -> bool {
    let hovered = rect.contains(input.mouse_position);
    draw_panel_surface(
        ui,
        rect,
        if active {
            OVERLAY_NAV_ACTIVE
        } else if hovered {
            OVERLAY_NAV_HOVER
        } else {
            Color::TRANSPARENT
        },
        if active {
            OVERLAY_BORDER_STRONG
        } else {
            OVERLAY_BORDER
        },
        12.0,
    );

    let clicked = ui
        .button_in_rect(
            &format!("overlay-tab-{}", tab.index()),
            rect,
            "",
            ButtonColors::new(
                Color::TRANSPARENT,
                Color::TRANSPARENT,
                Color::TRANSPARENT,
                Color::TRANSPARENT,
            ),
        )
        .clicked;

    ui.text_at(
        &crate::runtime::foundation::i18n::tr(tab.nav_title_key()),
        Vec2::new(rect.min.x + 18.0, rect.min.y + 12.0),
        if active { TEXT_PRIMARY } else { TEXT_SECONDARY },
    );
    ui.text_at(
        &crate::runtime::foundation::i18n::tr(tab.nav_subtitle_key()),
        Vec2::new(rect.min.x + 18.0, rect.min.y + 32.0),
        if active { TEXT_SECONDARY } else { TEXT_MUTED },
    );
    clicked
}

fn draw_page_header(ui: &mut Ui<'_>, main_inner: Rect, runtime: &OverlayRuntimeState) {
    ui.text_at(
        &crate::runtime::foundation::i18n::tr(runtime.active_tab.title_key()),
        main_inner.min,
        TEXT_PRIMARY,
    );
}

fn render_close_button(ui: &mut Ui<'_>, input: &InputSnapshot, rect: Rect) -> bool {
    let hovered = rect.contains(input.mouse_position);
    let response = ui.button_in_rect(
        "overlay-close",
        rect,
        "",
        ButtonColors::new(
            Color::TRANSPARENT,
            Color::TRANSPARENT,
            Color::TRANSPARENT,
            Color::TRANSPARENT,
        ),
    );
    draw_panel_surface(
        ui,
        rect,
        if response.active {
            OVERLAY_CLOSE_ACTIVE
        } else if hovered {
            OVERLAY_CLOSE_HOVER
        } else {
            OVERLAY_CLOSE_BG
        },
        OVERLAY_BORDER,
        10.0,
    );
    let glyph_size = text_size("X", 2.0);
    ui.text_at_clipped(
        "X",
        Vec2::new(
            rect.min.x + ((rect.width() - glyph_size.x) * 0.5).floor(),
            rect.min.y + ((rect.height() - glyph_size.y) * 0.5).floor(),
        ),
        Color::WHITE,
        rect,
    );
    response.clicked
}

fn draw_panel_surface(ui: &mut Ui<'_>, rect: Rect, fill: Color, border: Color, radius: f32) {
    if border != Color::TRANSPARENT {
        ui.rounded_rect(rect, border, radius);
    }
    let inset = if border == Color::TRANSPARENT {
        0.0
    } else {
        1.0
    };
    ui.rounded_rect(rect.shrink(inset), fill, (radius - inset).max(0.0));
}

fn draw_panel_surface_clipped(
    ui: &mut Ui<'_>,
    rect: Rect,
    fill: Color,
    border: Color,
    radius: f32,
    clip_rect: Rect,
) {
    if border != Color::TRANSPARENT {
        ui.rounded_rect_clipped(rect, border, radius, clip_rect);
    }
    let inset = if border == Color::TRANSPARENT {
        0.0
    } else {
        1.0
    };
    ui.rounded_rect_clipped(
        rect.shrink(inset),
        fill,
        (radius - inset).max(0.0),
        clip_rect,
    );
}

fn draw_badge(ui: &mut Ui<'_>, rect: Rect, text: &str, fill: Color, text_color: Color) {
    draw_panel_surface(ui, rect, fill, Color::TRANSPARENT, 11.0);
    ui.text_at(
        text,
        Vec2::new(rect.min.x + 10.0, rect.min.y + 5.0),
        text_color,
    );
}

fn draw_badge_clipped(
    ui: &mut Ui<'_>,
    rect: Rect,
    text: &str,
    fill: Color,
    text_color: Color,
    clip_rect: Rect,
) {
    draw_panel_surface_clipped(ui, rect, fill, Color::TRANSPARENT, 11.0, clip_rect);
    let metrics = text_size(text, TEXT_SCALE);
    let text_x = rect.min.x + ((rect.width() - metrics.x) * 0.5).floor();
    let text_y = rect.min.y + ((rect.height() - metrics.y) * 0.5).floor();
    ui.text_at_width_clipped(
        text,
        Vec2::new(text_x.max(rect.min.x + 8.0), text_y),
        (rect.width() - ((text_x - rect.min.x).max(8.0) * 2.0)).max(0.0),
        text_color,
        clip_rect,
    );
}

fn measure_badge_width(text: &str) -> f32 {
    (text_size(text, TEXT_SCALE).x + 28.0).max(92.0)
}

fn render_feature_toggle_page(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    content_rect: Rect,
    mut active_panel: Option<(String, String)>,
    current_scroll: f32,
    visibility: f32,
) -> (Option<(String, String)>, f32) {
    let toggles = host::feature_toggles();
    if toggles.is_empty() {
        draw_empty_state(
            ui,
            content_rect,
            crate::runtime::foundation::i18n::tr("overlay.page.features.empty_title"),
            crate::runtime::foundation::i18n::tr("overlay.page.features.empty_body"),
        );
        return (None, 0.0);
    }

    let expanded_panel_extra = if active_panel.is_some() {
        356.0 + OVERLAY_SECTION_SPACING
    } else {
        0.0
    };
    let content_height = toggles.len() as f32 * (OVERLAY_CARD_HEIGHT + OVERLAY_CARD_SPACING)
        - OVERLAY_CARD_SPACING
        + expanded_panel_extra;
    let mut scroll = current_scroll;
    let scroll_offset = update_overlay_scroll(input, content_rect, content_height, &mut scroll);
    let layout_rect = scroll_content_rect(content_rect, content_height, visibility);
    let clip_rect = layout_rect;
    let mut y = layout_rect.min.y - scroll_offset;
    for toggle in toggles {
        let rect = Rect::from_min_size(
            Vec2::new(layout_rect.min.x, y),
            Vec2::new(layout_rect.width(), OVERLAY_CARD_HEIGHT),
        );
        let feature_panel = host::feature_panel_for_owner(&toggle.owner_name);
        if !rect.intersects(clip_rect) {
            y += OVERLAY_CARD_HEIGHT + OVERLAY_CARD_SPACING;
            continue;
        }

        let hovered =
            clip_rect.contains(input.mouse_position) && rect.contains(input.mouse_position);
        let selected = active_panel
            .as_ref()
            .map(|(owner, id)| {
                feature_panel
                    .as_ref()
                    .map(|panel| owner == &panel.owner_name && id == &panel.id)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        draw_panel_surface_clipped(
            ui,
            rect,
            if selected {
                Color::rgba(42, 90, 170, 92)
            } else if hovered {
                OVERLAY_CARD_BG_HOVER
            } else {
                OVERLAY_CARD_BG
            },
            if selected {
                OVERLAY_BORDER_STRONG
            } else {
                OVERLAY_BORDER
            },
            14.0,
            clip_rect,
        );
        let mut enabled = toggle.enabled;
        let switch_rect = Rect::from_min_size(
            Vec2::new(rect.max.x - SWITCH_WIDTH - 18.0, rect.min.y + 32.0),
            Vec2::new(SWITCH_WIDTH, SWITCH_HEIGHT),
        );
        let badge = if enabled {
            crate::runtime::foundation::i18n::tr("overlay.feature.enabled")
        } else {
            crate::runtime::foundation::i18n::tr("overlay.feature.disabled")
        };
        let badge_width = measure_badge_width(&badge);
        let badge_rect = Rect::from_min_size(
            Vec2::new(
                switch_rect.min.x - badge_width - 12.0,
                rect.min.y + ((rect.height() - OVERLAY_BADGE_HEIGHT) * 0.5).floor(),
            ),
            Vec2::new(badge_width, OVERLAY_BADGE_HEIGHT),
        );
        let icon_rect = Rect::from_min_size(
            Vec2::new(
                badge_rect.min.x - OVERLAY_ICON_BUTTON_SIZE - 10.0,
                rect.min.y + ((rect.height() - OVERLAY_ICON_BUTTON_SIZE) * 0.5).floor(),
            ),
            Vec2::splat(OVERLAY_ICON_BUTTON_SIZE),
        );
        let text_max_width = (icon_rect.min.x - rect.min.x - 28.0).max(120.0);

        ui.text_at_width_clipped(
            &toggle.title,
            Vec2::new(rect.min.x + 18.0, rect.min.y + 16.0),
            text_max_width,
            TEXT_PRIMARY,
            clip_rect,
        );
        ui.text_at_width_clipped(
            &toggle.owner_name,
            Vec2::new(rect.min.x + 18.0, rect.min.y + 46.0),
            text_max_width,
            TEXT_MUTED,
            clip_rect,
        );

        if ui
            .switch_in_rect_clipped(
                &format!("overlay-toggle:{}:{}", toggle.owner_name, toggle.id),
                switch_rect,
                clip_rect,
                &mut enabled,
            )
            .changed
        {
            host::set_feature_toggle(&toggle.owner_name, &toggle.id, enabled);
        }
        if let Some(panel) = feature_panel.as_ref() {
            let icon_response = icon_button_in_rect(
                ui,
                &format!(
                    "overlay-feature-settings:{}:{}",
                    toggle.owner_name, panel.id
                ),
                icon_rect,
                lucide_icons::icon_sliders_horizontal(),
                clip_rect,
                ButtonColors::new(
                    Color::rgba(36, 66, 118, 196),
                    Color::rgba(45, 83, 146, 224),
                    Color::rgba(39, 72, 128, 255),
                    Color::WHITE,
                ),
                Color::rgba(226, 236, 255, 255),
            );
            if icon_response.clicked {
                let panel_key = (panel.owner_name.clone(), panel.id.clone());
                active_panel = if selected { None } else { Some(panel_key) };
            }
        }
        draw_badge_clipped(
            ui,
            badge_rect,
            &badge,
            if enabled {
                Color::rgba(46, 118, 85, 180)
            } else {
                Color::rgba(88, 95, 110, 160)
            },
            if enabled { TEXT_SUCCESS } else { TEXT_MUTED },
            clip_rect,
        );

        y += OVERLAY_CARD_HEIGHT + OVERLAY_CARD_SPACING;
    }

    if let Some((owner_name, panel_id)) = active_panel.as_ref() {
        if let Some(panel) = host::feature_panel_for_owner(owner_name) {
            let detail_rect = Rect::from_min_size(
                Vec2::new(layout_rect.min.x, y + OVERLAY_SECTION_SPACING),
                Vec2::new(layout_rect.width(), 356.0),
            );
            draw_panel_surface_clipped(
                ui,
                detail_rect,
                OVERLAY_CARD_BG,
                OVERLAY_BORDER_STRONG,
                16.0,
                clip_rect,
            );
            ui.text_at_width_clipped(
                &panel.title,
                Vec2::new(detail_rect.min.x + 18.0, detail_rect.min.y + 18.0),
                detail_rect.width() - 36.0,
                TEXT_PRIMARY,
                clip_rect,
            );
            ui.text_at_width_clipped(
                &panel.description,
                Vec2::new(detail_rect.min.x + 18.0, detail_rect.min.y + 44.0),
                detail_rect.width() - 36.0,
                TEXT_SECONDARY,
                clip_rect,
            );
            let inline_rect = Rect::from_min_max(
                Vec2::new(detail_rect.min.x + 18.0, detail_rect.min.y + 86.0),
                Vec2::new(detail_rect.max.x - 18.0, detail_rect.max.y - 18.0),
            );
            let _ = host::render_feature_panel_inline(
                owner_name,
                panel_id,
                &format!("feature-panel:{}:{}", owner_name, panel_id),
                inline_rect,
            );
        }
    }

    draw_scrollbar(ui, content_rect, scroll_offset, content_height, visibility);
    (active_panel, scroll)
}

fn render_loaded_mods_page(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    content_rect: Rect,
    scroll: &mut f32,
    visibility: f32,
) {
    let mods = host::loaded_mod_views();
    if mods.is_empty() {
        draw_empty_state(
            ui,
            content_rect,
            crate::runtime::foundation::i18n::tr("overlay.page.mods.empty_title"),
            crate::runtime::foundation::i18n::tr("overlay.page.mods.empty_body"),
        );
        return;
    }

    let content_height =
        mods.len() as f32 * (MOD_CARD_HEIGHT + OVERLAY_CARD_SPACING) - OVERLAY_CARD_SPACING;
    let scroll_offset = update_overlay_scroll(input, content_rect, content_height, scroll);
    let layout_rect = scroll_content_rect(content_rect, content_height, visibility);
    let clip_rect = layout_rect;
    let mut y = layout_rect.min.y - scroll_offset;
    for loaded in mods {
        let rect = Rect::from_min_size(
            Vec2::new(layout_rect.min.x, y),
            Vec2::new(layout_rect.width(), MOD_CARD_HEIGHT),
        );
        if !rect.intersects(clip_rect) {
            y += rect.height() + OVERLAY_CARD_SPACING;
            continue;
        }

        let hovered =
            clip_rect.contains(input.mouse_position) && rect.contains(input.mouse_position);
        draw_panel_surface_clipped(
            ui,
            rect,
            if hovered {
                OVERLAY_CARD_BG_HOVER
            } else {
                OVERLAY_CARD_BG
            },
            OVERLAY_BORDER,
            14.0,
            clip_rect,
        );

        ui.text_at_width_clipped(
            &loaded.name,
            Vec2::new(rect.min.x + 18.0, rect.min.y + 14.0),
            (rect.width() - 120.0).max(100.0),
            TEXT_PRIMARY,
            clip_rect,
        );
        let loaded_badge = crate::runtime::foundation::i18n::tr("overlay.mod.loaded");
        let loaded_badge_width = measure_badge_width(&loaded_badge);
        draw_badge_clipped(
            ui,
            Rect::from_min_size(
                Vec2::new(rect.max.x - loaded_badge_width - 18.0, rect.min.y + 14.0),
                Vec2::new(loaded_badge_width, OVERLAY_BADGE_HEIGHT),
            ),
            &loaded_badge,
            Color::rgba(40, 114, 76, 190),
            TEXT_SUCCESS,
            clip_rect,
        );

        ui.text_at_width_clipped(
            &format!(
                "{}  {}",
                crate::runtime::foundation::i18n::tr("overlay.mod.version"),
                loaded
                    .version
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .unwrap_or("-"),
            ),
            Vec2::new(rect.min.x + 18.0, rect.min.y + 38.0),
            rect.width() - 36.0,
            TEXT_SECONDARY,
            clip_rect,
        );
        ui.text_at_width_clipped(
            &format!(
                "{}  BL API v{}",
                crate::runtime::foundation::i18n::tr("overlay.mod.dependency"),
                loaded.api_version
            ),
            Vec2::new(rect.min.x + 18.0, rect.min.y + 62.0),
            rect.width() - 36.0,
            TEXT_MUTED,
            clip_rect,
        );
        ui.text_at_width_clipped(
            &format!(
                "{}  {}",
                crate::runtime::foundation::i18n::tr("overlay.mod.name"),
                loaded.id
            ),
            Vec2::new(rect.min.x + 18.0, rect.min.y + 86.0),
            rect.width() - 36.0,
            TEXT_MUTED,
            clip_rect,
        );

        y += rect.height() + OVERLAY_CARD_SPACING;
    }

    draw_scrollbar(ui, content_rect, scroll_offset, content_height, visibility);
}

fn render_settings_page(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    content_rect: Rect,
    current_scroll: f32,
    visibility: f32,
    runtime: &mut OverlayRuntimeState,
    request_reload: &mut bool,
) -> f32 {
    let content_height = OVERLAY_CARD_HEIGHT
        + OVERLAY_SECTION_SPACING
        + OVERLAY_SETTINGS_CARD_HEIGHT
        + OVERLAY_CARD_SPACING
        + OVERLAY_SETTINGS_CARD_HEIGHT
        + OVERLAY_CARD_SPACING
        + OVERLAY_SETTINGS_NETWORK_CARD_HEIGHT
        + OVERLAY_CARD_SPACING
        + OVERLAY_SETTINGS_SLIDER_CARD_HEIGHT
        + OVERLAY_CARD_SPACING
        + (OVERLAY_SETTINGS_CARD_HEIGHT - 10.0)
        + OVERLAY_SECTION_SPACING
        + OVERLAY_BUTTON_HEIGHT;
    let mut scroll = current_scroll;
    let scroll_offset = update_overlay_scroll(input, content_rect, content_height, &mut scroll);
    let layout_rect = scroll_content_rect(content_rect, content_height, visibility);
    let clip_rect = layout_rect;
    let origin = Vec2::new(layout_rect.min.x, layout_rect.min.y - scroll_offset);

    let summary_rect =
        Rect::from_min_size(origin, Vec2::new(layout_rect.width(), OVERLAY_CARD_HEIGHT));
    draw_panel_surface_clipped(
        ui,
        summary_rect,
        OVERLAY_CARD_BG,
        OVERLAY_BORDER,
        14.0,
        clip_rect,
    );
    ui.text_at_clipped(
        &crate::runtime::foundation::i18n::tr("overlay.settings.summary_title"),
        Vec2::new(summary_rect.min.x + 18.0, summary_rect.min.y + 14.0),
        TEXT_PRIMARY,
        clip_rect,
    );
    ui.text_at_width_clipped(
        &format!(
            "{} {}  |  {} {}",
            crate::runtime::foundation::i18n::tr("overlay.settings.locale"),
            runtime.config.default_locale,
            crate::runtime::foundation::i18n::tr("overlay.settings.backend"),
            backend_name()
        ),
        Vec2::new(summary_rect.min.x + 18.0, summary_rect.min.y + 40.0),
        summary_rect.width() - 36.0,
        TEXT_SECONDARY,
        clip_rect,
    );

    let toggle_rect = Rect::from_min_size(
        Vec2::new(origin.x, summary_rect.max.y + OVERLAY_SECTION_SPACING),
        Vec2::new(layout_rect.width(), OVERLAY_SETTINGS_CARD_HEIGHT),
    );
    render_hotkey_card(
        ui,
        input,
        toggle_rect,
        clip_rect,
        &crate::runtime::foundation::i18n::tr("overlay.settings.toggle.title"),
        &crate::runtime::foundation::i18n::tr("overlay.settings.toggle.body"),
        runtime.config.overlay.toggle_hotkey,
        runtime.recording_hotkey == Some(HotkeyField::Toggle),
        HotkeyField::Toggle,
        runtime,
    );

    let reload_rect = Rect::from_min_size(
        Vec2::new(origin.x, toggle_rect.max.y + OVERLAY_CARD_SPACING),
        Vec2::new(layout_rect.width(), OVERLAY_SETTINGS_CARD_HEIGHT),
    );
    render_hotkey_card(
        ui,
        input,
        reload_rect,
        clip_rect,
        &crate::runtime::foundation::i18n::tr("overlay.settings.reload.title"),
        &crate::runtime::foundation::i18n::tr("overlay.settings.reload.body"),
        runtime.config.overlay.reload_hotkey,
        runtime.recording_hotkey == Some(HotkeyField::Reload),
        HotkeyField::Reload,
        runtime,
    );

    let network_card_rect = Rect::from_min_size(
        Vec2::new(origin.x, reload_rect.max.y + OVERLAY_CARD_SPACING),
        Vec2::new(layout_rect.width(), OVERLAY_SETTINGS_NETWORK_CARD_HEIGHT),
    );
    render_network_settings_card(ui, input, network_card_rect, clip_rect, runtime);

    let blur_strength_rect = Rect::from_min_size(
        Vec2::new(origin.x, network_card_rect.max.y + OVERLAY_CARD_SPACING),
        Vec2::new(layout_rect.width(), OVERLAY_SETTINGS_SLIDER_CARD_HEIGHT),
    );
    render_blur_strength_card(ui, input, blur_strength_rect, clip_rect, runtime);

    let note_rect = Rect::from_min_size(
        Vec2::new(origin.x, blur_strength_rect.max.y + OVERLAY_CARD_SPACING),
        Vec2::new(layout_rect.width(), OVERLAY_SETTINGS_CARD_HEIGHT - 10.0),
    );

    draw_panel_surface_clipped(
        ui,
        note_rect,
        OVERLAY_CARD_BG,
        OVERLAY_BORDER,
        14.0,
        clip_rect,
    );
    ui.text_at_clipped(
        &crate::runtime::foundation::i18n::tr("overlay.settings.recording.note_title"),
        Vec2::new(note_rect.min.x + 18.0, note_rect.min.y + 14.0),
        TEXT_PRIMARY,
        clip_rect,
    );
    ui.text_at_width_clipped(
        &crate::runtime::foundation::i18n::tr("overlay.settings.recording.note_body"),
        Vec2::new(note_rect.min.x + 18.0, note_rect.min.y + 38.0),
        note_rect.width() - 36.0,
        TEXT_SECONDARY,
        clip_rect,
    );

    let button_top = note_rect.max.y + OVERLAY_SECTION_SPACING;
    let gap = OVERLAY_CARD_SPACING;
    let button_width = ((layout_rect.width() - gap * 2.0) / 3.0).max(0.0);
    let reset_hotkeys_rect = Rect::from_min_size(
        Vec2::new(origin.x, button_top),
        Vec2::new(button_width, OVERLAY_BUTTON_HEIGHT),
    );
    let reset_panel_rect = Rect::from_min_size(
        Vec2::new(origin.x + button_width + gap, button_top),
        Vec2::new(button_width, OVERLAY_BUTTON_HEIGHT),
    );
    let reload_button_rect = Rect::from_min_size(
        Vec2::new(origin.x + (button_width + gap) * 2.0, button_top),
        Vec2::new(button_width, OVERLAY_BUTTON_HEIGHT),
    );

    if render_action_button(
        ui,
        reset_hotkeys_rect,
        clip_rect,
        "overlay-settings-reset-hotkeys",
        &crate::runtime::foundation::i18n::tr("overlay.settings.reset_hotkeys"),
        BUTTON_COLORS,
    ) {
        runtime.config.overlay.reset_to_defaults();
        runtime.recording_hotkey = None;
        if runtime.config.save().is_ok() {
            runtime.set_toast(
                crate::runtime::foundation::i18n::tr("overlay.settings.reset_hotkeys_done"),
                false,
            );
        } else {
            runtime.set_toast(
                crate::runtime::foundation::i18n::tr("overlay.settings.save_failed"),
                true,
            );
        }
    }

    if render_action_button(
        ui,
        reset_panel_rect,
        clip_rect,
        "overlay-settings-reset-all",
        &crate::runtime::foundation::i18n::tr("overlay.settings.reset_all"),
        ButtonColors::new(
            Color::rgba(105, 72, 38, 220),
            Color::rgba(134, 88, 42, 235),
            Color::rgba(120, 78, 36, 255),
            Color::WHITE,
        ),
    ) {
        runtime.config.reset_to_defaults();
        runtime.active_tab = OverlayTab::Features;
        runtime.recording_hotkey = None;
        if runtime.config.save().is_ok() {
            runtime.set_toast(
                crate::runtime::foundation::i18n::tr("overlay.settings.reset_all_done"),
                false,
            );
        } else {
            runtime.set_toast(
                crate::runtime::foundation::i18n::tr("overlay.settings.save_failed"),
                true,
            );
        }
    }

    if render_action_button(
        ui,
        reload_button_rect,
        clip_rect,
        "overlay-settings-reload-now",
        &crate::runtime::foundation::i18n::tr("overlay.settings.reload_now"),
        ButtonColors::new(
            Color::rgba(31, 83, 67, 220),
            Color::rgba(42, 105, 87, 240),
            Color::rgba(36, 95, 76, 255),
            Color::WHITE,
        ),
    ) {
        *request_reload = true;
        runtime.set_toast(
            crate::runtime::foundation::i18n::tr("overlay.settings.reload_requested"),
            false,
        );
    }

    draw_scrollbar(ui, content_rect, scroll_offset, content_height, visibility);
    scroll
}

fn render_hotkey_card(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    rect: Rect,
    clip_rect: Rect,
    title: &str,
    body: &str,
    hotkey: HotkeyConfig,
    recording: bool,
    field: HotkeyField,
    runtime: &mut OverlayRuntimeState,
) {
    let hovered = clip_rect.contains(input.mouse_position) && rect.contains(input.mouse_position);
    draw_panel_surface_clipped(
        ui,
        rect,
        if hovered {
            OVERLAY_CARD_BG_HOVER
        } else {
            OVERLAY_CARD_BG
        },
        OVERLAY_BORDER,
        14.0,
        clip_rect,
    );

    ui.text_at_clipped(
        title,
        Vec2::new(rect.min.x + 18.0, rect.min.y + 14.0),
        TEXT_PRIMARY,
        clip_rect,
    );
    ui.text_at_width_clipped(
        body,
        Vec2::new(rect.min.x + 18.0, rect.min.y + 38.0),
        (rect.width() - 220.0).max(80.0),
        TEXT_SECONDARY,
        clip_rect,
    );

    let button_rect = Rect::from_min_size(
        Vec2::new(rect.max.x - 156.0, rect.min.y + 18.0),
        Vec2::new(138.0, OVERLAY_BUTTON_HEIGHT),
    );
    let colors = if recording {
        ButtonColors::new(
            Color::rgba(46, 118, 214, 220),
            Color::rgba(55, 131, 230, 240),
            Color::rgba(42, 112, 204, 255),
            Color::WHITE,
        )
    } else {
        ButtonColors::new(
            Color::rgba(18, 26, 40, 220),
            Color::rgba(26, 37, 55, 240),
            Color::rgba(22, 31, 47, 255),
            Color::WHITE,
        )
    };
    let label = if recording {
        crate::runtime::foundation::i18n::tr("overlay.settings.hotkey.recording")
    } else {
        hotkey_to_string(hotkey)
    };
    let button_id = match field {
        HotkeyField::Toggle => "overlay-settings-toggle-hotkey",
        HotkeyField::Reload => "overlay-settings-reload-hotkey",
    };
    if render_action_button(ui, button_rect, clip_rect, button_id, &label, colors) {
        runtime.recording_hotkey = Some(field);
    }
}

fn render_blur_strength_card(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    rect: Rect,
    clip_rect: Rect,
    runtime: &mut OverlayRuntimeState,
) {
    let hovered = clip_rect.contains(input.mouse_position) && rect.contains(input.mouse_position);
    draw_panel_surface_clipped(
        ui,
        rect,
        if hovered {
            OVERLAY_CARD_BG_HOVER
        } else {
            OVERLAY_CARD_BG
        },
        OVERLAY_BORDER,
        14.0,
        clip_rect,
    );

    ui.text_at_clipped(
        &crate::runtime::foundation::i18n::tr("overlay.settings.blur.title"),
        Vec2::new(rect.min.x + 18.0, rect.min.y + 14.0),
        TEXT_PRIMARY,
        clip_rect,
    );
    ui.text_at_width_clipped(
        &crate::runtime::foundation::i18n::tr("overlay.settings.blur.body"),
        Vec2::new(rect.min.x + 18.0, rect.min.y + 38.0),
        rect.width() - 36.0,
        TEXT_SECONDARY,
        clip_rect,
    );

    let slider_rect = Rect::from_min_size(
        Vec2::new(rect.min.x + 18.0, rect.min.y + 72.0),
        Vec2::new((rect.width() - 36.0).max(0.0), 34.0),
    );
    let mut blur_strength = runtime.config.overlay.blur_strength();
    let changed = ui
        .drag_float_in_rect(
            "overlay-settings-blur-strength",
            slider_rect,
            &crate::runtime::foundation::i18n::tr("overlay.settings.blur.label"),
            &mut blur_strength,
            0.0,
            2.4,
            clip_rect,
        )
        .changed;
    if changed {
        runtime.config.overlay.set_blur_strength(blur_strength);
        if runtime.config.save().is_err() {
            runtime.set_toast(
                crate::runtime::foundation::i18n::tr("overlay.settings.save_failed"),
                true,
            );
        }
    }
}

fn render_network_settings_card(
    ui: &mut Ui<'_>,
    _input: &InputSnapshot,
    rect: Rect,
    clip_rect: Rect,
    runtime: &mut OverlayRuntimeState,
) {
    if !rect.intersects(clip_rect) {
        return;
    }

    draw_panel_surface_clipped(
        ui,
        rect,
        OVERLAY_CARD_BG,
        OVERLAY_BORDER,
        14.0,
        clip_rect,
    );

    let mut changed = false;

    ui.text_at_clipped(
        &crate::runtime::foundation::i18n::tr("overlay.settings.network.title"),
        Vec2::new(rect.min.x + 18.0, rect.min.y + 14.0),
        TEXT_PRIMARY,
        clip_rect,
    );

    ui.text_at_width_clipped(
        &crate::runtime::foundation::i18n::tr("overlay.settings.network.body"),
        Vec2::new(rect.min.x + 18.0, rect.min.y + 36.0),
        rect.width() - 36.0,
        TEXT_SECONDARY,
        clip_rect,
    );

    let row1_y = rect.min.y + 68.0;

    let mut enable_hook = runtime.config.enable_network_hooks;
    let switch_rect = Rect::from_min_size(
        Vec2::new(rect.min.x + 18.0, row1_y),
        Vec2::new(SWITCH_WIDTH, SWITCH_HEIGHT),
    );
    if ui
        .switch_in_rect_clipped(
            "net-hook-enable-switch",
            switch_rect,
            clip_rect,
            &mut enable_hook,
        )
        .changed
    {
        runtime.config.enable_network_hooks = enable_hook;
        changed = true;
    }
    ui.text_at_clipped(
        &crate::runtime::foundation::i18n::tr("overlay.settings.network.enable"),
        Vec2::new(rect.min.x + 18.0 + SWITCH_WIDTH + 10.0, row1_y + 2.0),
        TEXT_PRIMARY,
        clip_rect,
    );

    let verbose_x = rect.min.x + 280.0;
    let mut verbose = runtime.config.network_verbose;
    let verbose_switch_rect = Rect::from_min_size(
        Vec2::new(verbose_x, row1_y),
        Vec2::new(SWITCH_WIDTH, SWITCH_HEIGHT),
    );
    if ui
        .switch_in_rect_clipped(
            "net-hook-verbose-switch",
            verbose_switch_rect,
            clip_rect,
            &mut verbose,
        )
        .changed
    {
        runtime.config.network_verbose = verbose;
        changed = true;
    }
    ui.text_at_clipped(
        &crate::runtime::foundation::i18n::tr("overlay.settings.network.verbose"),
        Vec2::new(verbose_x + SWITCH_WIDTH + 10.0, row1_y + 2.0),
        TEXT_PRIMARY,
        clip_rect,
    );

    let row2_y = rect.min.y + 106.0;
    let listen_label = format!(
        "{} {}",
        crate::runtime::foundation::i18n::tr("overlay.settings.network.listen_port"),
        runtime.config.network_listen_port
    );
    ui.text_at_clipped(
        &listen_label,
        Vec2::new(rect.min.x + 18.0, row2_y + 4.0),
        TEXT_PRIMARY,
        clip_rect,
    );

    let btn_width = 54.0;
    let btn_h = 28.0;
    let ports_preset = [19132, 25565, 8080, 19133];
    let mut btn_x = rect.min.x + 220.0;
    for &p in &ports_preset {
        let p_rect = Rect::from_min_size(Vec2::new(btn_x, row2_y), Vec2::new(btn_width, btn_h));
        let colors = if runtime.config.network_listen_port == p {
            BUTTON_COLORS
        } else {
            ButtonColors::new(
                Color::rgba(45, 55, 72, 200),
                Color::rgba(60, 75, 95, 220),
                Color::rgba(50, 65, 85, 240),
                Color::WHITE,
            )
        };
        if render_action_button(
            ui,
            p_rect,
            clip_rect,
            &format!("net-port-btn-{}", p),
            &p.to_string(),
            colors,
        ) {
            runtime.config.network_listen_port = p;
            changed = true;
        }
        btn_x += btn_width + 6.0;
    }

    let row3_y = rect.min.y + 144.0;
    let hex_label = format!(
        "{} {}",
        crate::runtime::foundation::i18n::tr("overlay.settings.network.hex_bytes"),
        runtime.config.network_log_hex_bytes
    );
    ui.text_at_clipped(
        &hex_label,
        Vec2::new(rect.min.x + 18.0, row3_y + 4.0),
        TEXT_PRIMARY,
        clip_rect,
    );

    let hex_presets = [0, 16, 32, 64];
    let mut hex_x = rect.min.x + 220.0;
    for &hb in &hex_presets {
        let h_rect = Rect::from_min_size(Vec2::new(hex_x, row3_y), Vec2::new(btn_width, btn_h));
        let label = if hb == 0 { "Off".to_string() } else { format!("{}B", hb) };
        let colors = if runtime.config.network_log_hex_bytes == hb {
            BUTTON_COLORS
        } else {
            ButtonColors::new(
                Color::rgba(45, 55, 72, 200),
                Color::rgba(60, 75, 95, 220),
                Color::rgba(50, 65, 85, 240),
                Color::WHITE,
            )
        };
        if render_action_button(
            ui,
            h_rect,
            clip_rect,
            &format!("net-hex-btn-{}", hb),
            &label,
            colors,
        ) {
            runtime.config.network_log_hex_bytes = hb;
            changed = true;
        }
        hex_x += btn_width + 6.0;
    }

    if changed {
        let _ = runtime.config.save();
        crate::core::network_hook::update_config(&runtime.config);
    }
}

fn render_action_button(
    ui: &mut Ui<'_>,
    rect: Rect,
    clip_rect: Rect,
    id_source: &str,
    label: &str,
    colors: ButtonColors,
) -> bool {
    ui.button_in_rect_clipped(id_source, rect, label, clip_rect, colors)
        .clicked
}

fn draw_empty_state(ui: &mut Ui<'_>, rect: Rect, title: String, body: String) {
    let empty_rect =
        Rect::from_min_size(rect.min, Vec2::new(rect.width(), OVERLAY_EMPTY_CARD_HEIGHT));
    draw_panel_surface(ui, empty_rect, OVERLAY_CARD_BG, OVERLAY_BORDER, 16.0);
    ui.text_at(
        &title,
        Vec2::new(empty_rect.min.x + 20.0, empty_rect.min.y + 24.0),
        TEXT_PRIMARY,
    );
    ui.text_at_width(
        &body,
        Vec2::new(empty_rect.min.x + 20.0, empty_rect.min.y + 54.0),
        empty_rect.width() - 40.0,
        TEXT_SECONDARY,
    );
}

fn update_overlay_scroll(
    input: &InputSnapshot,
    viewport: Rect,
    content_height: f32,
    scroll: &mut f32,
) -> f32 {
    let max_scroll = (content_height - viewport.height()).max(0.0);
    if viewport.contains(input.mouse_position) && input.mouse_wheel_delta.abs() > f32::EPSILON {
        *scroll = (*scroll - input.mouse_wheel_delta * 44.0).clamp(0.0, max_scroll);
    } else {
        *scroll = (*scroll).clamp(0.0, max_scroll);
    }
    *scroll
}

fn scroll_content_rect(viewport: Rect, content_height: f32, visibility: f32) -> Rect {
    let needs_scrollbar = visibility >= 0.98 && (content_height - viewport.height()) > f32::EPSILON;
    let reserved_width = if needs_scrollbar {
        OVERLAY_SCROLLBAR_WIDTH + OVERLAY_SCROLLBAR_GUTTER
    } else {
        0.0
    };
    Rect::from_min_max(
        viewport.min,
        Vec2::new(
            (viewport.max.x - reserved_width).max(viewport.min.x),
            viewport.max.y,
        ),
    )
}

fn draw_scrollbar(
    ui: &mut Ui<'_>,
    viewport: Rect,
    scroll_offset: f32,
    content_height: f32,
    visibility: f32,
) {
    if visibility < 0.98 {
        return;
    }

    let max_scroll = (content_height - viewport.height()).max(0.0);
    if max_scroll <= 0.0 {
        return;
    }

    let track_rect = Rect::from_min_size(
        Vec2::new(viewport.max.x - OVERLAY_SCROLLBAR_WIDTH, viewport.min.y),
        Vec2::new(OVERLAY_SCROLLBAR_WIDTH, viewport.height()),
    );
    let thumb_ratio = (viewport.height() / content_height).clamp(0.0, 1.0);
    let thumb_height = (track_rect.height() * thumb_ratio).max(OVERLAY_SCROLLBAR_MIN_HEIGHT);
    let travel = (track_rect.height() - thumb_height).max(0.0);
    let thumb_y = track_rect.min.y
        + if max_scroll <= f32::EPSILON {
            0.0
        } else {
            (scroll_offset / max_scroll) * travel
        };

    ui.rounded_rect(
        track_rect,
        Color::rgba(255, 255, 255, 10),
        OVERLAY_SCROLLBAR_WIDTH * 0.5,
    );
    ui.rounded_rect(
        Rect::from_min_size(
            Vec2::new(track_rect.min.x, thumb_y),
            Vec2::new(track_rect.width(), thumb_height),
        ),
        Color::rgba(107, 154, 255, 168),
        OVERLAY_SCROLLBAR_WIDTH * 0.5,
    );
}

fn render_global_toasts(ui: &mut Ui<'_>, input: &InputSnapshot) {
    let mut toasts = global_toasts().lock().unwrap_or_else(|e| e.into_inner());
    if toasts.is_empty() {
        return;
    }

    for toast in &mut *toasts {
        toast.phase_age += input.delta_seconds;
        if !toast.closing && toast.phase_age >= toast.payload.lifetime_seconds {
            toast.closing = true;
            toast.phase_age = 0.0;
        }
    }

    let display = input.display_size;
    let mut top_left = 18.0;
    let mut top_right = 18.0;
    let mut bottom_left = display.y - 18.0;
    let mut bottom_right = display.y - 18.0;

    for toast in &*toasts {
        let phase = (toast.phase_age / OVERLAY_TOAST_ANIMATION_SECONDS).clamp(0.0, 1.0);
        let progress = if toast.closing {
            1.0 - phase
        } else {
            ease_out_back(phase)
        };
        if progress <= 0.0 {
            continue;
        }

        let height = toast_height(&toast.payload);
        let rect = match toast.payload.anchor {
            ToastAnchor::TopLeft => {
                let rect = Rect::from_min_size(
                    Vec2::new(18.0 - (1.0 - progress) * 28.0, top_left),
                    Vec2::new(OVERLAY_TOAST_WIDTH, height),
                );
                top_left += height + OVERLAY_TOAST_STACK_GAP;
                rect
            }
            ToastAnchor::TopRight => {
                let rect = Rect::from_min_size(
                    Vec2::new(
                        display.x - OVERLAY_TOAST_WIDTH - 18.0 + (1.0 - progress) * 28.0,
                        top_right,
                    ),
                    Vec2::new(OVERLAY_TOAST_WIDTH, height),
                );
                top_right += height + OVERLAY_TOAST_STACK_GAP;
                rect
            }
            ToastAnchor::BottomLeft => {
                let rect = Rect::from_min_size(
                    Vec2::new(18.0 - (1.0 - progress) * 28.0, bottom_left - height),
                    Vec2::new(OVERLAY_TOAST_WIDTH, height),
                );
                bottom_left -= height + OVERLAY_TOAST_STACK_GAP;
                rect
            }
            ToastAnchor::BottomRight => {
                let rect = Rect::from_min_size(
                    Vec2::new(
                        display.x - OVERLAY_TOAST_WIDTH - 18.0 + (1.0 - progress) * 28.0,
                        bottom_right - height,
                    ),
                    Vec2::new(OVERLAY_TOAST_WIDTH, height),
                );
                bottom_right -= height + OVERLAY_TOAST_STACK_GAP;
                rect
            }
        };
        draw_global_toast(ui, rect, toast, progress);
    }

    toasts.retain(|toast| !(toast.closing && toast.phase_age >= OVERLAY_TOAST_ANIMATION_SECONDS));
}

fn toast_height(toast: &OverlayToast) -> f32 {
    let body_lines: f32 = if toast.body.is_empty() { 0.0 } else { 1.0 };
    let title_lines: f32 = if toast.title.is_empty() { 0.0 } else { 1.0 };
    (OVERLAY_TOAST_MIN_HEIGHT + (title_lines + body_lines - 1.0).max(0.0) * 18.0)
        .max(OVERLAY_TOAST_MIN_HEIGHT)
}

fn draw_global_toast(ui: &mut Ui<'_>, rect: Rect, toast: &GlobalToast, progress: f32) {
    let elapsed_ratio = if toast.payload.lifetime_seconds > f32::EPSILON {
        (toast.phase_age / toast.payload.lifetime_seconds).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let life_progress = if toast.closing {
        0.0
    } else {
        1.0 - elapsed_ratio
    };
    let (fill, accent, text_color, icon_bg) = match toast.payload.kind {
        ToastKind::Info => (
            Color::rgba(10, 16, 28, 226),
            Color::rgba(109, 203, 255, 255),
            TEXT_PRIMARY,
            Color::rgba(18, 33, 52, 228),
        ),
        ToastKind::Success => (
            Color::rgba(10, 16, 28, 226),
            Color::rgba(111, 246, 210, 255),
            Color::rgba(210, 250, 240, 255),
            Color::rgba(16, 38, 42, 228),
        ),
        ToastKind::Warning => (
            Color::rgba(10, 16, 28, 226),
            Color::rgba(255, 208, 108, 255),
            TEXT_WARNING,
            Color::rgba(42, 34, 20, 228),
        ),
        ToastKind::Error => (
            Color::rgba(10, 16, 28, 226),
            Color::rgba(255, 133, 133, 255),
            Color::WHITE,
            Color::rgba(46, 24, 30, 228),
        ),
    };
    let scale = if toast.closing {
        0.86 + progress * 0.14
    } else {
        0.9 + progress * 0.1
    };
    let centered = scaled_rect(rect, scale);
    let fill = with_alpha(fill, progress);
    let accent = with_alpha(accent, progress);
    let text_color = with_alpha(text_color, progress);
    let icon_bg = with_alpha(icon_bg, progress);
    let clip_rect = Rect::from_min_size(Vec2::new(0.0, 0.0), ui.display_size());

    draw_panel_surface_clipped(
        ui,
        centered,
        fill,
        with_alpha(Color::rgba(255, 255, 255, 18), progress),
        22.0,
        clip_rect,
    );
    ui.rounded_rect_clipped(
        centered.shrink(1.0),
        with_alpha(Color::rgba(255, 255, 255, 10), progress * 0.45),
        21.0,
        clip_rect,
    );
    ui.rounded_rect_clipped(
        Rect::from_min_size(
            Vec2::new(centered.min.x + 14.0, centered.min.y + 10.0),
            Vec2::new(centered.width() - 28.0, 1.0),
        ),
        with_alpha(Color::rgba(255, 255, 255, 30), progress),
        0.5,
        clip_rect,
    );
    ui.rounded_rect_clipped(
        Rect::from_min_size(
            Vec2::new(
                centered.min.x,
                centered.max.y - OVERLAY_TOAST_PROGRESS_HEIGHT,
            ),
            Vec2::new(centered.width(), OVERLAY_TOAST_PROGRESS_HEIGHT),
        ),
        with_alpha(Color::rgba(255, 255, 255, 18), progress),
        2.0,
        clip_rect,
    );
    ui.rounded_rect_clipped(
        Rect::from_min_size(
            Vec2::new(
                centered.min.x,
                centered.max.y - OVERLAY_TOAST_PROGRESS_HEIGHT,
            ),
            Vec2::new(
                centered.width() * life_progress,
                OVERLAY_TOAST_PROGRESS_HEIGHT,
            ),
        ),
        accent,
        2.0,
        clip_rect,
    );

    let icon_rect = Rect::from_min_size(
        Vec2::new(centered.min.x + 16.0, centered.min.y + 15.0),
        Vec2::splat(OVERLAY_TOAST_ICON_SIZE),
    );
    draw_panel_surface_clipped(
        ui,
        icon_rect,
        with_alpha(icon_bg, progress),
        with_alpha(Color::rgba(255, 255, 255, 12), progress),
        14.0,
        clip_rect,
    );
    draw_toast_status_chip(ui, icon_rect, accent, clip_rect);

    let content_x = icon_rect.max.x + 14.0;
    let content_width = (centered.max.x - content_x - 16.0).max(0.0);
    if toast.payload.title.is_empty() {
        ui.text_at_width_clipped(
            &toast.payload.body,
            Vec2::new(content_x, centered.min.y + 24.0),
            content_width,
            text_color,
            clip_rect,
        );
    } else {
        ui.text_at_width_clipped(
            &toast.payload.title,
            Vec2::new(content_x, centered.min.y + 14.0),
            content_width,
            text_color,
            clip_rect,
        );
        ui.text_at_width_clipped(
            &toast.payload.body,
            Vec2::new(content_x, centered.min.y + 35.0),
            content_width,
            with_alpha(TEXT_SECONDARY, progress),
            clip_rect,
        );
    }
}

fn ease_out_back(t: f32) -> f32 {
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn scaled_rect(rect: Rect, scale: f32) -> Rect {
    let size = Vec2::new(rect.width() * scale, rect.height() * scale);
    Rect::from_min_size(
        Vec2::new(
            rect.min.x + (rect.width() - size.x) * 0.5,
            rect.min.y + (rect.height() - size.y) * 0.5,
        ),
        size,
    )
}

fn draw_toast_status_chip(ui: &mut Ui<'_>, rect: Rect, accent: Color, clip_rect: Rect) {
    let dot_rect = Rect::from_min_size(
        Vec2::new(rect.min.x + 10.0, rect.min.y + 10.0),
        Vec2::new(8.0, 8.0),
    );
    ui.rounded_rect_clipped(dot_rect, accent, 4.0, clip_rect);
    let bar1 = Rect::from_min_size(
        Vec2::new(rect.min.x + 10.0, rect.min.y + 24.0),
        Vec2::new(22.0, 3.0),
    );
    let bar2 = Rect::from_min_size(
        Vec2::new(rect.min.x + 10.0, rect.min.y + 30.0),
        Vec2::new(16.0, 3.0),
    );
    let bar3 = Rect::from_min_size(
        Vec2::new(rect.min.x + 10.0, rect.min.y + 36.0),
        Vec2::new(10.0, 3.0),
    );
    ui.rounded_rect_clipped(bar1, with_alpha(accent, 0.95), 1.5, clip_rect);
    ui.rounded_rect_clipped(bar2, with_alpha(accent, 0.7), 1.5, clip_rect);
    ui.rounded_rect_clipped(bar3, with_alpha(accent, 0.45), 1.5, clip_rect);
}

fn capture_hotkey(input: &InputSnapshot) -> Option<HotkeyConfig> {
    input.pressed_keys.iter().find_map(|key| {
        key_to_virtual_key(*key).map(|virtual_key| HotkeyConfig {
            key: virtual_key,
            alt: input.alt_down,
            ctrl: input.ctrl_down,
            shift: input.shift_down,
        })
    })
}

fn hotkey_to_string(hotkey: HotkeyConfig) -> String {
    let mut parts = Vec::new();
    if hotkey.ctrl {
        parts.push("Ctrl".to_string());
    }
    if hotkey.alt {
        parts.push("Alt".to_string());
    }
    if hotkey.shift {
        parts.push("Shift".to_string());
    }
    parts.push(virtual_key_name(hotkey.key));
    parts.join(" + ")
}

fn virtual_key_name(key: u32) -> String {
    match key {
        0x08 => "Backspace".to_string(),
        0x09 => "Tab".to_string(),
        0x0D => "Enter".to_string(),
        0x1B => "Esc".to_string(),
        0x20 => "Space".to_string(),
        0x23 => "End".to_string(),
        0x24 => "Home".to_string(),
        0x25 => "Left".to_string(),
        0x26 => "Up".to_string(),
        0x27 => "Right".to_string(),
        0x28 => "Down".to_string(),
        0x2D => "Insert".to_string(),
        0x2E => "Delete".to_string(),
        0x70..=0x7B => format!("F{}", key - 0x6F),
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(key).unwrap_or('?').to_string(),
        value => format!("VK-{value:02X}"),
    }
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    let alpha = alpha.clamp(0.0, 1.0);
    let r = (color.0 & 0xFF) as u8;
    let g = ((color.0 >> 8) & 0xFF) as u8;
    let b = ((color.0 >> 16) & 0xFF) as u8;
    let a = ((color.0 >> 24) & 0xFF) as f32;
    Color::rgba(r, g, b, (a * alpha).round().clamp(0.0, 255.0) as u8)
}

fn compat_begin_window(
    ui: &mut Ui<'_>,
    layout: &mut LayoutState,
    title: &str,
    open: Option<&mut bool>,
    _flags: u32,
    options: Option<WindowOptions>,
) -> bool {
    if let Some(open) = open {
        if !*open {
            layout.end_window();
            return false;
        }
    }

    let options = options.unwrap_or_else(|| default_window_options(title));
    if ui.begin_window(title, options) {
        layout.begin_window(title);
        true
    } else {
        layout.end_window();
        false
    }
}

fn compat_end_window(ui: &mut Ui<'_>, layout: &mut LayoutState, compat: &mut CompatState) {
    if layout.window_open && layout.owns_window {
        ui.end_window();
    }
    compat.active_slider = None;
    layout.end_window();
}

fn compat_text(ui: &mut Ui<'_>, layout: &mut LayoutState, text: &str, color: Color) {
    if !layout.window_open {
        return;
    }

    let size = text_size(text, TEXT_SCALE);
    let rect = next_item_rect(ui, layout, 0.0, size.y.max(18.0));
    ui.text_at_width(text, rect.min, rect.width(), color);
    finish_item(ui, layout, rect);
}

fn compat_bullet_text(ui: &mut Ui<'_>, layout: &mut LayoutState, text: &str) {
    if !layout.window_open {
        return;
    }

    let size = text_size(text, TEXT_SCALE);
    let rect = next_item_rect(ui, layout, 0.0, size.y.max(18.0));
    let bullet_y = rect.min.y + ((rect.height() - BULLET_SIZE) * 0.5);
    let bullet_rect =
        Rect::from_min_size(Vec2::new(rect.min.x, bullet_y), Vec2::splat(BULLET_SIZE));
    ui.rounded_rect(bullet_rect, TEXT_ACCENT, BULLET_SIZE * 0.5);
    ui.text_at_width(
        text,
        Vec2::new(rect.min.x + BULLET_SIZE + 10.0, rect.min.y),
        (rect.width() - BULLET_SIZE - 10.0).max(0.0),
        TEXT_PRIMARY,
    );
    finish_item(ui, layout, rect);
}

fn compat_button(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    layout: &mut LayoutState,
    label: &str,
) -> bool {
    let _ = input;
    if !layout.window_open {
        return false;
    }

    let rect = next_item_rect(ui, layout, 0.0, BUTTON_HEIGHT);
    let clicked = ui
        .button_in_rect(
            &format!("button:{}:{}", layout.current_window_id, label),
            rect,
            label,
            BUTTON_COLORS,
        )
        .clicked;
    finish_item(ui, layout, rect);
    clicked
}

fn compat_checkbox(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    layout: &mut LayoutState,
    label: &str,
    value: &mut bool,
) -> bool {
    let _ = input;
    if !layout.window_open {
        return false;
    }

    let rect = next_item_rect(ui, layout, 0.0, CHECKBOX_HEIGHT);
    let switch_rect = Rect::from_min_size(
        Vec2::new(
            rect.max.x - SWITCH_WIDTH,
            rect.min.y + ((rect.height() - SWITCH_HEIGHT) * 0.5),
        ),
        Vec2::new(SWITCH_WIDTH, SWITCH_HEIGHT),
    );
    ui.text_at_width(
        label,
        rect.min,
        (switch_rect.min.x - rect.min.x - 12.0).max(0.0),
        TEXT_PRIMARY,
    );
    let response = ui.switch_in_rect(
        &format!("checkbox:{}:{}", layout.current_window_id, label),
        switch_rect,
        value,
    );
    finish_item(ui, layout, rect);
    response.changed
}

fn compat_slider_float(
    ui: &mut Ui<'_>,
    input: &InputSnapshot,
    layout: &mut LayoutState,
    compat: &mut CompatState,
    label: &str,
    value: &mut f32,
    min: f32,
    max: f32,
) -> bool {
    if !layout.window_open {
        return false;
    }

    let rect = next_item_rect(ui, layout, 0.0, SLIDER_HEIGHT);
    let changed = ui
        .drag_float_in_rect(
            &format!("slider:{}:{}", layout.current_window_id, label),
            rect,
            label,
            value,
            min,
            max,
            ui.current_window_body_rect(),
        )
        .changed;
    let _ = input;
    let _ = compat;
    finish_item(ui, layout, rect);
    changed
}

fn compat_progress_bar(
    ui: &mut Ui<'_>,
    layout: &mut LayoutState,
    label: &str,
    value: f32,
    min: f32,
    max: f32,
) {
    if !layout.window_open {
        return;
    }

    let rect = next_item_rect(ui, layout, 0.0, PROGRESS_HEIGHT);
    let label_height = text_size(label, TEXT_SCALE).y;
    ui.text_at_width(
        label,
        Vec2::new(
            rect.min.x,
            rect.min.y + ((rect.height() - label_height) * 0.5).floor(),
        ),
        (rect.width() * 0.32).max(64.0),
        TEXT_PRIMARY,
    );
    let progress_rect = Rect::from_min_size(
        Vec2::new(rect.min.x + rect.width() * 0.36, rect.min.y),
        Vec2::new(rect.width() * 0.64, rect.height()),
    );
    ui.progress_bar_in_rect(
        progress_rect,
        value,
        min,
        max,
        ui.current_window_body_rect(),
    );
    finish_item(ui, layout, rect);
}

fn compat_separator(ui: &mut Ui<'_>, layout: &mut LayoutState) {
    if !layout.window_open {
        return;
    }

    let rect = next_item_rect(ui, layout, 0.0, 1.0);
    ui.filled_rect(rect, Color::rgba(255, 255, 255, 22));
    finish_item(ui, layout, rect);
}

fn compat_same_line(layout: &mut LayoutState) {
    if layout.window_open && layout.last_item_rect.is_some() {
        layout.pending_same_line = true;
    }
}

fn next_item_rect(ui: &mut Ui<'_>, layout: &mut LayoutState, width: f32, height: f32) -> Rect {
    let content = layout
        .inline_content_rect
        .unwrap_or_else(|| ui.current_window_content_rect());
    let mut cursor = ui.cursor();

    if layout.pending_same_line {
        if let Some(last_rect) = layout.last_item_rect {
            cursor = Vec2::new(
                (last_rect.max.x + ITEM_SPACING).min(content.max.x),
                layout.row_top,
            );
            ui.set_cursor(cursor);
        }
        layout.pending_same_line = false;
    } else {
        layout.row_top = cursor.y;
        layout.row_bottom = cursor.y;
    }

    let remaining_width = (content.max.x - cursor.x).max(0.0);
    let resolved_width = if width <= 0.0 || width > remaining_width {
        remaining_width
    } else {
        width.max(0.0)
    };

    Rect::from_min_size(cursor, Vec2::new(resolved_width, height.max(0.0)))
}

fn finish_item(ui: &mut Ui<'_>, layout: &mut LayoutState, rect: Rect) {
    let content = layout
        .inline_content_rect
        .unwrap_or_else(|| ui.current_window_content_rect());
    layout.last_item_rect = Some(rect);
    layout.row_bottom = layout.row_bottom.max(rect.max.y);
    ui.set_cursor(Vec2::new(content.min.x, layout.row_bottom + ITEM_SPACING));
}

fn default_window_options(title: &str) -> WindowOptions {
    let seed = hash_id(("window", title));
    let col = (seed % 4) as f32;
    let row = ((seed / 4) % 4) as f32;
    WindowOptions {
        position: Vec2::new(48.0 + (col * 28.0), 84.0 + (row * 28.0)),
        size: Vec2::new(420.0, 320.0),
        movable: true,
        resizable: true,
        visuals: WindowVisuals::default(),
    }
}

fn hash_id<T: Hash>(value: T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn set_backend_name(name: &str) {
    let mut guard = BACKEND_NAME
        .get_or_init(|| Mutex::new("arcui-dx12-uninitialized".to_string()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = name.to_string();
}

impl LayoutState {
    fn begin_window(&mut self, title: &str) {
        self.window_open = true;
        self.owns_window = true;
        self.current_window_id = hash_id(("window", title));
        self.last_item_rect = None;
        self.pending_same_line = false;
        self.row_top = 0.0;
        self.row_bottom = 0.0;
        self.inline_content_rect = None;
    }

    fn begin_inline_panel(&mut self, id: &str, rect: Rect) {
        self.window_open = true;
        self.owns_window = false;
        self.current_window_id = hash_id(("inline-panel", id));
        self.last_item_rect = None;
        self.pending_same_line = false;
        self.row_top = rect.min.y;
        self.row_bottom = rect.min.y;
        self.inline_content_rect = Some(rect);
    }

    fn end_window(&mut self) {
        self.window_open = false;
        self.owns_window = false;
        self.current_window_id = 0;
        self.last_item_rect = None;
        self.pending_same_line = false;
        self.row_top = 0.0;
        self.row_bottom = 0.0;
        self.inline_content_rect = None;
    }
}

fn view_to_string(view: BlStringView) -> Option<String> {
    if view.ptr.is_null() {
        return None;
    }

    let bytes = unsafe { std::slice::from_raw_parts(view.ptr as *const u8, view.len) };
    Some(String::from_utf8_lossy(bytes).to_string())
}

fn with_active_frame<R>(f: impl FnOnce(&mut ActiveFrameAccess) -> R) -> Option<R> {
    ACTIVE_FRAME.with(|slot| {
        let ptr = slot.get();
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { f(&mut *ptr) })
        }
    })
}

pub fn show_global_toast(
    title: impl Into<String>,
    body: impl Into<String>,
    anchor: ToastAnchor,
    kind: ToastKind,
    lifetime_seconds: f32,
) {
    push_global_toast(OverlayToast {
        title: title.into(),
        body: body.into(),
        anchor,
        kind,
        lifetime_seconds: lifetime_seconds.max(0.8),
    });
}

fn toast_anchor_from_raw(raw: u32) -> ToastAnchor {
    match raw {
        1 => ToastAnchor::TopRight,
        2 => ToastAnchor::BottomLeft,
        3 => ToastAnchor::BottomRight,
        _ => ToastAnchor::TopLeft,
    }
}

fn toast_kind_from_raw(raw: u32) -> ToastKind {
    match raw {
        1 => ToastKind::Success,
        2 => ToastKind::Warning,
        3 => ToastKind::Error,
        _ => ToastKind::Info,
    }
}

pub unsafe extern "system" fn host_ui_show_toast(
    title: BlStringView,
    body: BlStringView,
    anchor: u32,
    kind: u32,
    lifetime_seconds: f32,
) -> bool {
    let title = view_to_string(title).unwrap_or_default();
    let body = view_to_string(body).unwrap_or_default();
    if title.is_empty() && body.is_empty() {
        return false;
    }

    show_global_toast(
        title,
        body,
        toast_anchor_from_raw(anchor),
        toast_kind_from_raw(kind),
        lifetime_seconds,
    );
    true
}

pub fn host_render_inline_panel(
    panel_key: &str,
    rect: Rect,
    callback: BlUiCallback,
    user_data: *mut c_void,
) {
    let _ = with_active_frame(|frame| {
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let layout = unsafe { &mut *frame.layout };
        let compat = unsafe { &mut *frame.compat };
        let saved_layout = layout.clone();
        let saved_active_slider = compat.active_slider;
        let saved_cursor = ui.cursor();
        layout.begin_inline_panel(panel_key, rect);
        ui.set_cursor(rect.min);
        let result = panic::catch_unwind(AssertUnwindSafe(|| unsafe { callback(user_data) }));
        *layout = saved_layout;
        compat.active_slider = saved_active_slider;
        ui.set_cursor(saved_cursor);
        if let Err(payload) = result {
            panic::resume_unwind(payload);
        }
    });
}

pub unsafe extern "system" fn host_ui_begin_window(
    title: BlStringView,
    open: *mut bool,
    flags: u32,
) -> bool {
    let Some(title) = view_to_string(title) else {
        return false;
    };

    with_active_frame(|frame| {
        let open_ref = if open.is_null() {
            None
        } else {
            Some(unsafe { &mut *open })
        };
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let layout = unsafe { &mut *frame.layout };
        compat_begin_window(ui, layout, &title, open_ref, flags, None)
    })
    .unwrap_or(false)
}

pub unsafe extern "system" fn host_ui_end_window() {
    let _ = with_active_frame(|frame| {
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let layout = unsafe { &mut *frame.layout };
        let compat = unsafe { &mut *frame.compat };
        compat_end_window(ui, layout, compat);
    });
}

pub unsafe extern "system" fn host_ui_text(text: BlStringView) {
    let Some(text) = view_to_string(text) else {
        return;
    };

    let _ = with_active_frame(|frame| {
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let layout = unsafe { &mut *frame.layout };
        compat_text(ui, layout, &text, TEXT_PRIMARY);
    });
}

pub unsafe extern "system" fn host_ui_bullet_text(text: BlStringView) {
    let Some(text) = view_to_string(text) else {
        return;
    };

    let _ = with_active_frame(|frame| {
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let layout = unsafe { &mut *frame.layout };
        compat_bullet_text(ui, layout, &text);
    });
}

pub unsafe extern "system" fn host_ui_button(label: BlStringView) -> bool {
    let Some(label) = view_to_string(label) else {
        return false;
    };

    with_active_frame(|frame| {
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let input = unsafe { &*frame.input };
        let layout = unsafe { &mut *frame.layout };
        compat_button(ui, input, layout, &label)
    })
    .unwrap_or(false)
}

pub unsafe extern "system" fn host_ui_checkbox(label: BlStringView, value: *mut bool) -> bool {
    if value.is_null() {
        return false;
    }
    let Some(label) = view_to_string(label) else {
        return false;
    };

    with_active_frame(|frame| {
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let input = unsafe { &*frame.input };
        let layout = unsafe { &mut *frame.layout };
        let value = unsafe { &mut *value };
        compat_checkbox(ui, input, layout, &label, value)
    })
    .unwrap_or(false)
}

pub unsafe extern "system" fn host_ui_slider_float(
    label: BlStringView,
    value: *mut f32,
    min: f32,
    max: f32,
) -> bool {
    if value.is_null() {
        return false;
    }
    let Some(label) = view_to_string(label) else {
        return false;
    };

    with_active_frame(|frame| {
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let input = unsafe { &*frame.input };
        let layout = unsafe { &mut *frame.layout };
        let compat = unsafe { &mut *frame.compat };
        let value = unsafe { &mut *value };
        compat_slider_float(ui, input, layout, compat, &label, value, min, max)
    })
    .unwrap_or(false)
}

pub unsafe extern "system" fn host_ui_drag_float(
    label: BlStringView,
    value: *mut f32,
    min: f32,
    max: f32,
) -> bool {
    host_ui_slider_float(label, value, min, max)
}

pub unsafe extern "system" fn host_ui_progress_bar(
    label: BlStringView,
    value: f32,
    min: f32,
    max: f32,
) {
    let Some(label) = view_to_string(label) else {
        return;
    };

    let _ = with_active_frame(|frame| {
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let layout = unsafe { &mut *frame.layout };
        compat_progress_bar(ui, layout, &label, value, min, max);
    });
}

pub unsafe extern "system" fn host_ui_separator() {
    let _ = with_active_frame(|frame| {
        let ui = unsafe { &mut *(frame.ui as *mut Ui<'static>) };
        let layout = unsafe { &mut *frame.layout };
        compat_separator(ui, layout);
    });
}

pub unsafe extern "system" fn host_ui_same_line() {
    let _ = with_active_frame(|frame| {
        let layout = unsafe { &mut *frame.layout };
        compat_same_line(layout);
    });
}

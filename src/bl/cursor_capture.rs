use std::ffi::c_void;
use std::mem;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use minhook::MinHook;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::Input::{HRAWINPUT, RAW_INPUT_DATA_COMMAND_FLAGS, RAWINPUT};
use windows::Win32::UI::WindowsAndMessaging::ClipCursor;
use windows::core::{BOOL, GUID, s};

use crate::runtime::foundation::logging;

#[link(name = "user32")]
unsafe extern "system" {
    fn ReleaseCapture() -> BOOL;
}

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static GAMEINPUT_PROBE_STARTED: AtomicBool = AtomicBool::new(false);
static GAMEINPUT_BARRIER_READY: AtomicBool = AtomicBool::new(false);
static GAMEINPUT_SUPPRESSION_LOGGED: AtomicBool = AtomicBool::new(false);
static GAMEINPUT_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static OVERLAY_ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOWED_HOTKEY: AtomicUsize = AtomicUsize::new(0x2D); // VK_INSERT
static ALLOWED_MODIFIERS: AtomicUsize = AtomicUsize::new(0);

static ORIGINAL_GET_RAW_INPUT_DATA: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_RAW_INPUT_BUFFER: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_ASYNC_KEY_STATE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_KEY_STATE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CLIP_CURSOR: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SET_CAPTURE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SET_CURSOR_POS: AtomicUsize = AtomicUsize::new(0);

static ORIGINAL_GI_V0_KEY_COUNT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GI_V0_KEY_STATE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GI_V0_MOUSE_STATE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GI_MODERN_KEY_COUNT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GI_MODERN_KEY_STATE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GI_MODERN_MOUSE_STATE: AtomicUsize = AtomicUsize::new(0);

type GetRawInputDataFn = unsafe extern "system" fn(
    HRAWINPUT,
    RAW_INPUT_DATA_COMMAND_FLAGS,
    *mut c_void,
    *mut u32,
    u32,
) -> u32;
type GetRawInputBufferFn = unsafe extern "system" fn(*mut RAWINPUT, *mut u32, u32) -> u32;
type GetKeyStateFn = unsafe extern "system" fn(i32) -> i16;
type ClipCursorFn = unsafe extern "system" fn(*const RECT) -> BOOL;
type SetCaptureFn = unsafe extern "system" fn(HWND) -> HWND;
type SetCursorPosFn = unsafe extern "system" fn(i32, i32) -> BOOL;
type GameInputCreateFn = unsafe extern "system" fn(*mut *mut c_void) -> i32;
type QueryInterfaceFn =
    unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32;
type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
type GameInputGetCurrentReadingFn =
    unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut *mut c_void) -> i32;
type GameInputGetKeyCountFn = unsafe extern "system" fn(*mut c_void) -> u32;
type GameInputGetKeyStateFn =
    unsafe extern "system" fn(*mut c_void, u32, *mut GameInputKeyState) -> u32;
type GameInputGetMouseStateFn =
    unsafe extern "system" fn(*mut c_void, *mut GameInputMouseState) -> bool;

#[repr(C)]
struct GameInputKeyState {
    scan_code: u32,
    code_point: u32,
    virtual_key: u8,
    is_dead_key: bool,
}

#[repr(C)]
struct GameInputMouseState {
    buttons: u32,
    positions: u32,
    position_x: i64,
    position_y: i64,
    absolute_position_x: i64,
    absolute_position_y: i64,
    wheel_x: i64,
    wheel_y: i64,
}

#[derive(Clone, Copy)]
enum GameInputAbi {
    V0,
    Modern,
}

const GAMEINPUT_KIND_KEYBOARD: u32 = 0x0000_0010;
const GAMEINPUT_KIND_MOUSE: u32 = 0x0000_0020;

const IID_GAMEINPUT_V0: GUID = GUID::from_u128(0x11be2a7e_4254_445a_9c09_ffc40f006918);
const IID_GAMEINPUT_V1: GUID = GUID::from_u128(0x40ffb7e4_6150_407a_b439_132badc08d2d);
const IID_GAMEINPUT_V2: GUID = GUID::from_u128(0xbbaa66d2_837a_40f7_a303_917d500955f4);
const IID_GAMEINPUT_V3: GUID = GUID::from_u128(0x20efc1c7_5d9a_43ba_b26f_b807fa48609c);

/// Installs only stable Win32 input hooks.
///
/// ChiyanMap does not depend on version-specific MouseDevice/GameCore
/// signatures for its primary mouse barrier. BLoader now follows the same
/// principle: ArcUI's game-window WndProc consumes messages, while these hooks
/// block Raw Input and polling paths that bypass the WndProc message chain.
pub fn initialize() {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }

    try_install_user32_hooks();
    if !try_install_gameinput_barrier() {
        spawn_gameinput_probe();
    }
    // ArcUI installs additional MinHook targets immediately afterwards. Treat
    // an already-enabled global hook state as benign and enable everything
    // again once ArcUI's DX12 targets have been created.
    let _ = unsafe { MinHook::enable_all_hooks() };

    logging::info_message(
        "[overlay-input] ArcUI WndProc/RawInput barrier initialized; GameInput probe armed",
    );
}

pub fn set_allowed_hotkey(virtual_key: u32, alt: bool, ctrl: bool, shift: bool) {
    ALLOWED_HOTKEY.store(virtual_key as usize, Ordering::Release);
    let mask = (alt as usize) | ((ctrl as usize) << 1) | ((shift as usize) << 2);
    ALLOWED_MODIFIERS.store(mask, Ordering::Release);
}

pub fn set_overlay_active(active: bool) {
    let previous = OVERLAY_ACTIVE.swap(active, Ordering::SeqCst);
    if active && !previous {
        // Close the first-click race: the runtime can load after initialize(),
        // so make one synchronous attempt immediately before input ownership is
        // transferred to ArcUI. The background probe remains a delayed-load
        // fallback when this attempt cannot resolve GameInput yet.
        if !GAMEINPUT_BARRIER_READY.load(Ordering::Acquire) {
            let _ = try_install_gameinput_barrier();
        }
        release_cursor_for_overlay();
    } else if !active && previous {
        GAMEINPUT_SUPPRESSION_LOGGED.store(false, Ordering::Release);
    }
}

pub fn note_client_instance(_client_instance: usize) {
    // Retained for ABI compatibility. No game-version input signature is used.
}

fn try_install_user32_hooks() {
    unsafe {
        let user32 = GetModuleHandleW(windows::core::w!("user32.dll")).unwrap_or_default();
        if user32.is_invalid() {
            logging::warn_message("[overlay-input] failed to get user32.dll handle");
            return;
        }

        install_hook(
            user32,
            s!("GetRawInputData"),
            "GetRawInputData",
            detour_get_raw_input_data as *mut c_void,
            &ORIGINAL_GET_RAW_INPUT_DATA,
        );
        install_hook(
            user32,
            s!("GetRawInputBuffer"),
            "GetRawInputBuffer",
            detour_get_raw_input_buffer as *mut c_void,
            &ORIGINAL_GET_RAW_INPUT_BUFFER,
        );
        install_hook(
            user32,
            s!("GetAsyncKeyState"),
            "GetAsyncKeyState",
            detour_get_async_key_state as *mut c_void,
            &ORIGINAL_GET_ASYNC_KEY_STATE,
        );
        install_hook(
            user32,
            s!("GetKeyState"),
            "GetKeyState",
            detour_get_key_state as *mut c_void,
            &ORIGINAL_GET_KEY_STATE,
        );
        install_hook(
            user32,
            s!("ClipCursor"),
            "ClipCursor",
            detour_clip_cursor as *mut c_void,
            &ORIGINAL_CLIP_CURSOR,
        );
        install_hook(
            user32,
            s!("SetCapture"),
            "SetCapture",
            detour_set_capture as *mut c_void,
            &ORIGINAL_SET_CAPTURE,
        );
        install_hook(
            user32,
            s!("SetCursorPos"),
            "SetCursorPos",
            detour_set_cursor_pos as *mut c_void,
            &ORIGINAL_SET_CURSOR_POS,
        );
    }
}

unsafe fn install_hook(
    module: windows::Win32::Foundation::HMODULE,
    name: windows::core::PCSTR,
    label: &str,
    detour: *mut c_void,
    original_slot: &AtomicUsize,
) {
    let Some(target) = GetProcAddress(module, name) else {
        logging::warn_message(&format!("[overlay-input] user32 export not found: {label}"));
        return;
    };

    match MinHook::create_hook(target as *mut c_void, detour) {
        Ok(original) => original_slot.store(original as usize, Ordering::Release),
        Err(error) => logging::warn_message(&format!(
            "[overlay-input] failed to hook {label}: {error:?}"
        )),
    }
}

unsafe extern "system" fn detour_get_raw_input_data(
    hrawinput: HRAWINPUT,
    uicommand: RAW_INPUT_DATA_COMMAND_FLAGS,
    pdata: *mut c_void,
    pcbsize: *mut u32,
    cbheadersize: u32,
) -> u32 {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) {
        // Match ChiyanMap exactly: fail every Raw Input query while the panel
        // owns input, including RID_HEADER probes and RID_INPUT reads.
        return u32::MAX;
    }

    let original = ORIGINAL_GET_RAW_INPUT_DATA.load(Ordering::Acquire);
    if original == 0 {
        return u32::MAX;
    }
    let original: GetRawInputDataFn = mem::transmute(original);
    original(hrawinput, uicommand, pdata, pcbsize, cbheadersize)
}

unsafe extern "system" fn detour_get_raw_input_buffer(
    pdata: *mut RAWINPUT,
    pcbsize: *mut u32,
    cbheadersize: u32,
) -> u32 {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) {
        return u32::MAX;
    }

    let original = ORIGINAL_GET_RAW_INPUT_BUFFER.load(Ordering::Acquire);
    if original == 0 {
        return u32::MAX;
    }
    let original: GetRawInputBufferFn = mem::transmute(original);
    original(pdata, pcbsize, cbheadersize)
}

unsafe extern "system" fn detour_get_async_key_state(vkey: i32) -> i16 {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) && !is_passthrough_virtual_key(vkey) {
        return 0;
    }

    let original = ORIGINAL_GET_ASYNC_KEY_STATE.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    let original: GetKeyStateFn = mem::transmute(original);
    original(vkey)
}

unsafe extern "system" fn detour_get_key_state(vkey: i32) -> i16 {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) && !is_passthrough_virtual_key(vkey) {
        return 0;
    }

    let original = ORIGINAL_GET_KEY_STATE.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    let original: GetKeyStateFn = mem::transmute(original);
    original(vkey)
}

fn is_passthrough_virtual_key(vkey: i32) -> bool {
    // ChiyanMap preserves F11. BLoader additionally preserves its configurable
    // overlay toggle key so the panel can still be closed from the polling
    // thread while every other game key/button reports as released.
    if vkey == 0x7A || vkey as usize == ALLOWED_HOTKEY.load(Ordering::Acquire) {
        return true;
    }

    let modifiers = ALLOWED_MODIFIERS.load(Ordering::Acquire);
    match vkey {
        0x12 => modifiers & 0b001 != 0, // VK_MENU
        0x11 => modifiers & 0b010 != 0, // VK_CONTROL
        0x10 => modifiers & 0b100 != 0, // VK_SHIFT
        _ => false,
    }
}

unsafe extern "system" fn detour_clip_cursor(rect: *const RECT) -> BOOL {
    let original = ORIGINAL_CLIP_CURSOR.load(Ordering::Acquire);
    if original == 0 {
        return BOOL(1);
    }
    let original: ClipCursorFn = mem::transmute(original);

    if OVERLAY_ACTIVE.load(Ordering::Acquire) {
        if rect.is_null() {
            // ArcUI calls ClipCursor(NULL) every active frame. It must reach the
            // real API; otherwise the old game clip rectangle remains active.
            return original(rect);
        }
        // Refuse attempts by Minecraft to bind the cursor again while ArcUI is
        // open, but report success so the game does not enter an error path.
        return BOOL(1);
    }

    original(rect)
}

unsafe extern "system" fn detour_set_capture(hwnd: HWND) -> HWND {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) {
        return HWND(std::ptr::null_mut());
    }

    let original = ORIGINAL_SET_CAPTURE.load(Ordering::Acquire);
    if original == 0 {
        return HWND(std::ptr::null_mut());
    }
    let original: SetCaptureFn = mem::transmute(original);
    original(hwnd)
}

unsafe extern "system" fn detour_set_cursor_pos(x: i32, y: i32) -> BOOL {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) && !arcui_hook::dx12::cursor_warp_bypass_active() {
        // Minecraft recenters the hardware cursor for camera look. Pretend the
        // request succeeded while ArcUI owns input, otherwise the pointer is
        // pulled back to the game centre every frame.
        return BOOL(1);
    }

    let original = ORIGINAL_SET_CURSOR_POS.load(Ordering::Acquire);
    if original == 0 {
        return BOOL(0);
    }
    let original: SetCursorPosFn = mem::transmute(original);
    original(x, y)
}

fn release_cursor_for_overlay() {
    unsafe {
        // ClipCursor(NULL) is allowed through the detour above.
        let _ = ClipCursor(None);
        let _ = ReleaseCapture();
    }
}

fn spawn_gameinput_probe() {
    if GAMEINPUT_PROBE_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    let _ = thread::Builder::new()
        .name("bloader-gameinput-barrier".to_string())
        .spawn(|| {
            // GameInput can be loaded after BLoader. Keep probing until a
            // reading vtable is available; once hooked, all polling and
            // callback-delivered reading objects use the neutral-state methods.
            for _ in 0..1200 {
                if try_install_gameinput_barrier() {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }

            logging::warn_message(
                "[overlay-input] GameInput runtime was not resolved; WndProc/RawInput barriers remain active",
            );
        });
}

fn mark_gameinput_barrier_ready() {
    if !GAMEINPUT_BARRIER_READY.swap(true, Ordering::AcqRel) {
        logging::info_message("[overlay-input] GameInput keyboard/mouse reading barrier active");
    }
}

fn try_install_gameinput_barrier() -> bool {
    let _guard = GAMEINPUT_INSTALL_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    if GAMEINPUT_BARRIER_READY.load(Ordering::Acquire) {
        return true;
    }

    unsafe {
        for module_name in [
            windows::core::w!("gameinput.dll"),
            windows::core::w!("xgameruntime.dll"),
        ] {
            let Ok(module) = GetModuleHandleW(module_name) else {
                continue;
            };
            if module.is_invalid() {
                continue;
            }

            let Some(factory) = GetProcAddress(module, s!("GameInputCreate")) else {
                continue;
            };
            let create: GameInputCreateFn = mem::transmute(factory);
            let mut legacy = std::ptr::null_mut::<c_void>();
            if create(&mut legacy) < 0 || legacy.is_null() {
                continue;
            }

            let installed = install_supported_gameinput_abis(legacy);
            release_com(legacy);
            if installed {
                let _ = MinHook::enable_all_hooks();
                mark_gameinput_barrier_ready();
                return true;
            }
        }
    }

    false
}

unsafe fn install_supported_gameinput_abis(legacy: *mut c_void) -> bool {
    let mut installed = false;

    // Minecraft GDK commonly consumes the v0 ABI. Install it first, then
    // cover the versioned PC interfaces when the runtime exposes them.
    if let Some(interface) = query_interface(legacy, &IID_GAMEINPUT_V0) {
        installed |= install_reading_barrier_from_interface(interface, GameInputAbi::V0);
        release_com(interface);
    }

    for iid in [&IID_GAMEINPUT_V3, &IID_GAMEINPUT_V2, &IID_GAMEINPUT_V1] {
        if let Some(interface) = query_interface(legacy, iid) {
            installed |= install_reading_barrier_from_interface(interface, GameInputAbi::Modern);
            release_com(interface);
        }
    }

    installed
}

unsafe fn query_interface(instance: *mut c_void, iid: &GUID) -> Option<*mut c_void> {
    if instance.is_null() {
        return None;
    }
    let target = vtable_entry(instance, 0);
    if target.is_null() {
        return None;
    }
    let query: QueryInterfaceFn = mem::transmute(target);
    let mut output = std::ptr::null_mut::<c_void>();
    if query(instance, iid, &mut output) >= 0 && !output.is_null() {
        Some(output)
    } else {
        None
    }
}

unsafe fn release_com(instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let target = vtable_entry(instance, 2);
    if !target.is_null() {
        let release: ReleaseFn = mem::transmute(target);
        let _ = release(instance);
    }
}

unsafe fn install_reading_barrier_from_interface(
    game_input: *mut c_void,
    abi: GameInputAbi,
) -> bool {
    let get_current_target = vtable_entry(game_input, 4);
    if get_current_target.is_null() {
        return false;
    }
    let get_current: GameInputGetCurrentReadingFn = mem::transmute(get_current_target);

    for kind in [
        GAMEINPUT_KIND_MOUSE,
        GAMEINPUT_KIND_KEYBOARD,
        GAMEINPUT_KIND_MOUSE | GAMEINPUT_KIND_KEYBOARD,
    ] {
        let mut reading = std::ptr::null_mut::<c_void>();
        let hr = get_current(game_input, kind, std::ptr::null_mut(), &mut reading);
        if hr >= 0 && !reading.is_null() {
            let installed = install_reading_vtable_hooks(reading, abi);
            release_com(reading);
            if installed {
                return true;
            }
        }
    }

    false
}

unsafe fn install_reading_vtable_hooks(reading: *mut c_void, abi: GameInputAbi) -> bool {
    let (key_count_index, key_state_index, mouse_state_index) = match abi {
        GameInputAbi::V0 => (14usize, 15usize, 16usize),
        GameInputAbi::Modern => (12usize, 13usize, 14usize),
    };

    let (
        key_count_detour,
        key_state_detour,
        mouse_state_detour,
        key_count_slot,
        key_state_slot,
        mouse_state_slot,
    ) = match abi {
        GameInputAbi::V0 => (
            detour_gi_v0_get_key_count as *mut c_void,
            detour_gi_v0_get_key_state as *mut c_void,
            detour_gi_v0_get_mouse_state as *mut c_void,
            &ORIGINAL_GI_V0_KEY_COUNT,
            &ORIGINAL_GI_V0_KEY_STATE,
            &ORIGINAL_GI_V0_MOUSE_STATE,
        ),
        GameInputAbi::Modern => (
            detour_gi_modern_get_key_count as *mut c_void,
            detour_gi_modern_get_key_state as *mut c_void,
            detour_gi_modern_get_mouse_state as *mut c_void,
            &ORIGINAL_GI_MODERN_KEY_COUNT,
            &ORIGINAL_GI_MODERN_KEY_STATE,
            &ORIGINAL_GI_MODERN_MOUSE_STATE,
        ),
    };

    let key_count = install_vtable_hook(
        vtable_entry(reading, key_count_index),
        key_count_detour,
        key_count_slot,
        "IGameInputReading::GetKeyCount",
    );
    let key_state = install_vtable_hook(
        vtable_entry(reading, key_state_index),
        key_state_detour,
        key_state_slot,
        "IGameInputReading::GetKeyState",
    );
    let mouse_state = install_vtable_hook(
        vtable_entry(reading, mouse_state_index),
        mouse_state_detour,
        mouse_state_slot,
        "IGameInputReading::GetMouseState",
    );

    if mouse_state {
        true
    } else {
        if key_count || key_state {
            logging::warn_message(
                "[overlay-input] GameInput keyboard methods were hooked, but GetMouseState was not",
            );
        }
        false
    }
}

unsafe fn install_vtable_hook(
    target: *mut c_void,
    detour: *mut c_void,
    original_slot: &AtomicUsize,
    label: &str,
) -> bool {
    if target.is_null() {
        return false;
    }

    match MinHook::create_hook(target, detour) {
        Ok(original) => {
            original_slot.store(original as usize, Ordering::Release);
            true
        }
        Err(error) => {
            // Different GameInput interface versions can share the same method
            // implementation. In that case the first compatible detour already
            // protects the target and MinHook reports an existing hook.
            let text = format!("{error:?}");
            if text.contains("ALREADY_CREATED") || text.contains("AlreadyCreated") {
                true
            } else {
                logging::warn_message(&format!(
                    "[overlay-input] failed to hook {label}: {error:?}"
                ));
                false
            }
        }
    }
}

unsafe fn vtable_entry(instance: *mut c_void, index: usize) -> *mut c_void {
    if instance.is_null() {
        return std::ptr::null_mut();
    }
    let vtable = *(instance as *mut *mut *mut c_void);
    if vtable.is_null() {
        return std::ptr::null_mut();
    }
    *vtable.add(index)
}

fn note_gameinput_suppressed() {
    if !GAMEINPUT_SUPPRESSION_LOGGED.swap(true, Ordering::AcqRel) {
        logging::info_message(
            "[overlay-input] GameInput mouse/keyboard state neutralized while ArcUI is active",
        );
    }
}

unsafe extern "system" fn detour_gi_v0_get_key_count(this: *mut c_void) -> u32 {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) {
        note_gameinput_suppressed();
        return 0;
    }
    call_key_count(&ORIGINAL_GI_V0_KEY_COUNT, this)
}

unsafe extern "system" fn detour_gi_modern_get_key_count(this: *mut c_void) -> u32 {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) {
        note_gameinput_suppressed();
        return 0;
    }
    call_key_count(&ORIGINAL_GI_MODERN_KEY_COUNT, this)
}

unsafe fn call_key_count(slot: &AtomicUsize, this: *mut c_void) -> u32 {
    let original = slot.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    let original: GameInputGetKeyCountFn = mem::transmute(original);
    original(this)
}

unsafe extern "system" fn detour_gi_v0_get_key_state(
    this: *mut c_void,
    count: u32,
    state: *mut GameInputKeyState,
) -> u32 {
    detour_gameinput_key_state(&ORIGINAL_GI_V0_KEY_STATE, this, count, state)
}

unsafe extern "system" fn detour_gi_modern_get_key_state(
    this: *mut c_void,
    count: u32,
    state: *mut GameInputKeyState,
) -> u32 {
    detour_gameinput_key_state(&ORIGINAL_GI_MODERN_KEY_STATE, this, count, state)
}

unsafe fn detour_gameinput_key_state(
    slot: &AtomicUsize,
    this: *mut c_void,
    count: u32,
    state: *mut GameInputKeyState,
) -> u32 {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) {
        note_gameinput_suppressed();
        if !state.is_null() && count != 0 {
            std::ptr::write_bytes(state, 0, count as usize);
        }
        return 0;
    }

    let original = slot.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    let original: GameInputGetKeyStateFn = mem::transmute(original);
    original(this, count, state)
}

unsafe extern "system" fn detour_gi_v0_get_mouse_state(
    this: *mut c_void,
    state: *mut GameInputMouseState,
) -> bool {
    detour_gameinput_mouse_state(&ORIGINAL_GI_V0_MOUSE_STATE, this, state)
}

unsafe extern "system" fn detour_gi_modern_get_mouse_state(
    this: *mut c_void,
    state: *mut GameInputMouseState,
) -> bool {
    detour_gameinput_mouse_state(&ORIGINAL_GI_MODERN_MOUSE_STATE, this, state)
}

unsafe fn detour_gameinput_mouse_state(
    slot: &AtomicUsize,
    this: *mut c_void,
    state: *mut GameInputMouseState,
) -> bool {
    if OVERLAY_ACTIVE.load(Ordering::Acquire) {
        note_gameinput_suppressed();
        if !state.is_null() {
            std::ptr::write_bytes(state, 0, 1);
        }
        // Return a valid neutral reading instead of failure. Returning false can
        // cause a caller to retain the previous pressed-button state.
        return true;
    }

    let original = slot.load(Ordering::Acquire);
    if original == 0 {
        return false;
    }
    let original: GameInputGetMouseStateFn = mem::transmute(original);
    original(this, state)
}

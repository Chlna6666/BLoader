#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleExW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetWindowThreadProcessId, HHOOK,
    KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PM_REMOVE, PeekMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::runtime::foundation::logging;

const WHEEL_DELTA: i16 = 120;
const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x0000_0004;

static STARTED: AtomicBool = AtomicBool::new(false);
static STOPPED: AtomicBool = AtomicBool::new(false);
static LAST_WHEEL_STEPS: AtomicI64 = AtomicI64::new(0);
static TOTAL_WHEEL_STEPS: AtomicI64 = AtomicI64::new(0);
static MC_LAST_WHEEL_STEPS: AtomicI64 = AtomicI64::new(0);
static MC_TOTAL_WHEEL_STEPS: AtomicI64 = AtomicI64::new(0);
static LEFT_DOWN: AtomicBool = AtomicBool::new(false);
static RIGHT_DOWN: AtomicBool = AtomicBool::new(false);
static MIDDLE_DOWN: AtomicBool = AtomicBool::new(false);
static MC_LEFT_DOWN: AtomicBool = AtomicBool::new(false);
static MC_RIGHT_DOWN: AtomicBool = AtomicBool::new(false);
static MC_MIDDLE_DOWN: AtomicBool = AtomicBool::new(false);
static KEYS_DOWN: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
static MC_KEYS_DOWN: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

fn keys_down() -> &'static Mutex<HashSet<u32>> {
    KEYS_DOWN.get_or_init(|| Mutex::new(HashSet::new()))
}

fn mc_keys_down() -> &'static Mutex<HashSet<u32>> {
    MC_KEYS_DOWN.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn initialize_global_input() {
    if STARTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let _ = thread::Builder::new()
        .name("bloader-global-input".to_string())
        .spawn(global_input_thread);
}

pub fn is_key_down(virtual_key: u32) -> bool {
    keys_down()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&virtual_key)
}

pub fn is_mc_key_down(virtual_key: u32) -> bool {
    mc_keys_down()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&virtual_key)
}

pub fn mouse_wheel_total_steps() -> i64 {
    TOTAL_WHEEL_STEPS.load(Ordering::Relaxed)
}

pub fn mouse_wheel_last_steps() -> i64 {
    LAST_WHEEL_STEPS.load(Ordering::Relaxed)
}

pub fn mc_mouse_wheel_total_steps() -> i64 {
    MC_TOTAL_WHEEL_STEPS.load(Ordering::Relaxed)
}

pub fn mc_mouse_wheel_last_steps() -> i64 {
    MC_LAST_WHEEL_STEPS.load(Ordering::Relaxed)
}

pub fn is_left_down() -> bool {
    LEFT_DOWN.load(Ordering::Relaxed)
}

pub fn is_right_down() -> bool {
    RIGHT_DOWN.load(Ordering::Relaxed)
}

pub fn is_middle_down() -> bool {
    MIDDLE_DOWN.load(Ordering::Relaxed)
}

pub fn is_mc_left_down() -> bool {
    MC_LEFT_DOWN.load(Ordering::Relaxed)
}

pub fn is_mc_right_down() -> bool {
    MC_RIGHT_DOWN.load(Ordering::Relaxed)
}

pub fn is_mc_middle_down() -> bool {
    MC_MIDDLE_DOWN.load(Ordering::Relaxed)
}

pub fn is_game_focused() -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd == HWND::default() {
        return false;
    }
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid != 0 && pid == unsafe { GetCurrentProcessId() }
}

pub fn runtime_value(key: &str) -> String {
    match key {
        "input.mouse_wheel_total_steps" => mouse_wheel_total_steps().to_string(),
        "input.mouse_wheel_last_steps" => mouse_wheel_last_steps().to_string(),
        "mc.input.focused" => is_game_focused().to_string(),
        "mc.input.mouse_wheel_total_steps" => mc_mouse_wheel_total_steps().to_string(),
        "mc.input.mouse_wheel_last_steps" => mc_mouse_wheel_last_steps().to_string(),
        "mc.input.c_down" => is_mc_key_down(0x43).to_string(),
        "mc.input.left_down" => is_mc_left_down().to_string(),
        "mc.input.right_down" => is_mc_right_down().to_string(),
        "mc.input.middle_down" => is_mc_middle_down().to_string(),
        "input.global.started" => STARTED.load(Ordering::Relaxed).to_string(),
        "input.global.space_down" => is_key_down(0x20).to_string(),
        "input.global.c_down" => is_key_down(0x43).to_string(),
        "input.global.left_down" => is_left_down().to_string(),
        "input.global.right_down" => is_right_down().to_string(),
        "input.global.middle_down" => is_middle_down().to_string(),
        _ => String::new(),
    }
}

fn global_input_thread() {
    let keyboard_hook = install_hook(WH_KEYBOARD_LL, keyboard_hook_proc as *const ());
    let mouse_hook = install_hook(WH_MOUSE_LL, mouse_hook_proc as *const ());

    if keyboard_hook.is_invalid() {
        logging::warn_message("[global-input] keyboard hook unavailable");
    }
    if mouse_hook.is_invalid() {
        logging::warn_message("[global-input] mouse hook unavailable");
    }
    if !keyboard_hook.is_invalid() || !mouse_hook.is_invalid() {
        logging::info_message("[global-input] low-level keyboard/mouse observers armed (non-blocking)");
    }

    let mut message = MSG::default();
    while !STOPPED.load(Ordering::Relaxed) {
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            if message.message == WM_QUIT {
                STOPPED.store(true, Ordering::Relaxed);
                break;
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        thread::sleep(Duration::from_millis(4));
    }

    if !keyboard_hook.is_invalid() {
        let _ = unsafe { UnhookWindowsHookEx(keyboard_hook) };
    }
    if !mouse_hook.is_invalid() {
        let _ = unsafe { UnhookWindowsHookEx(mouse_hook) };
    }
}

fn install_hook(
    kind: windows::Win32::UI::WindowsAndMessaging::WINDOWS_HOOK_ID,
    address: *const (),
) -> HHOOK {
    let mut module = HMODULE::default();
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            windows::core::PCWSTR(address as *const u16),
            &mut module,
        )
    };
    if ok.is_err() {
        return HHOOK::default();
    }

    unsafe {
        match kind {
            WH_KEYBOARD_LL => {
                SetWindowsHookExW(kind, Some(keyboard_hook_proc), Some(module.into()), 0)
            }
            WH_MOUSE_LL => SetWindowsHookExW(kind, Some(mouse_hook_proc), Some(module.into()), 0),
            _ => return HHOOK::default(),
        }
    }
    .unwrap_or_default()
}

unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let virtual_key = info.vkCode;
        let is_down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let is_up = matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP);
        if is_down || is_up {
            let was_down = {
                let mut keys = keys_down().lock().unwrap_or_else(|e| e.into_inner());
                let contained = keys.contains(&virtual_key);
                if is_down {
                    keys.insert(virtual_key);
                } else {
                    keys.remove(&virtual_key);
                }
                contained
            };
            let focused = is_game_focused();
            {
                let mut keys = mc_keys_down().lock().unwrap_or_else(|e| e.into_inner());
                if focused {
                    if is_down {
                        keys.insert(virtual_key);
                    } else {
                        keys.remove(&virtual_key);
                    }
                } else if is_up {
                    keys.remove(&virtual_key);
                }
            }

            crate::bl::events::emit_key_event_with_modifiers(
                virtual_key,
                is_down,
                is_down && was_down,
                is_modifier_down(0x12),
                is_modifier_down(0x11),
                is_modifier_down(0x10),
            );
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;

        // Do not consume WH_MOUSE_LL events here. Returning a non-zero value
        // prevents Windows from updating the hardware pointer, which was the
        // cause of the cursor remaining stuck at the delayed centre position.
        // ArcUI receives ordinary Win32 mouse messages from the game HWND; the
        // game itself is blocked inside the process by WndProc, Raw Input and
        // GameInput-reading barriers. This hook is telemetry only.

        let focused = is_game_focused();
        match msg {
            WM_MOUSEWHEEL => {
                let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
                let delta = ((info.mouseData >> 16) & 0xFFFF) as u16 as i16;
                let steps = (delta / WHEEL_DELTA) as i64;
                LAST_WHEEL_STEPS.store(steps, Ordering::Relaxed);
                if steps != 0 {
                    TOTAL_WHEEL_STEPS.fetch_add(steps, Ordering::Relaxed);
                    if focused {
                        MC_LAST_WHEEL_STEPS.store(steps, Ordering::Relaxed);
                        MC_TOTAL_WHEEL_STEPS.fetch_add(steps, Ordering::Relaxed);
                    }
                }
            }
            WM_LBUTTONDOWN => set_mouse_button_state(&LEFT_DOWN, &MC_LEFT_DOWN, true, focused),
            WM_LBUTTONUP => set_mouse_button_state(&LEFT_DOWN, &MC_LEFT_DOWN, false, focused),
            WM_RBUTTONDOWN => set_mouse_button_state(&RIGHT_DOWN, &MC_RIGHT_DOWN, true, focused),
            WM_RBUTTONUP => set_mouse_button_state(&RIGHT_DOWN, &MC_RIGHT_DOWN, false, focused),
            WM_MBUTTONDOWN => set_mouse_button_state(&MIDDLE_DOWN, &MC_MIDDLE_DOWN, true, focused),
            WM_MBUTTONUP => set_mouse_button_state(&MIDDLE_DOWN, &MC_MIDDLE_DOWN, false, focused),
            _ => {
                LAST_WHEEL_STEPS.store(0, Ordering::Relaxed);
                MC_LAST_WHEEL_STEPS.store(0, Ordering::Relaxed);
            }
        }
    } else {
        LAST_WHEEL_STEPS.store(0, Ordering::Relaxed);
        MC_LAST_WHEEL_STEPS.store(0, Ordering::Relaxed);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn set_mouse_button_state(global: &AtomicBool, mc: &AtomicBool, down: bool, focused: bool) {
    global.store(down, Ordering::Relaxed);
    if focused || !down {
        mc.store(down, Ordering::Relaxed);
    }
}

fn is_modifier_down(virtual_key: u32) -> bool {
    keys_down()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&virtual_key)
}

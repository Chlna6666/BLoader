//! Capture the game's OutputDebugStringA/W output and forward it to the
//! BLoader console. These are generic kernel32/kernelbase export hooks (no
//! game version specific code); the vanilla client ships its internal
//! ContentLog lines through the debugger channel, so they never reach stdout
//! or the console. The debug channel is process-wide, so every line is
//! attributed to its owning module: the game and its runtime render as
//! [Minecraft], DLLs loaded from mods/ keep their own name.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use minhook::MinHook;
use windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{s, w};

use crate::platform::logging;

static ORIGINAL_A: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_W: AtomicUsize = AtomicUsize::new(0);
static HOOK_SEEN: AtomicU64 = AtomicU64::new(0);
static CAPTURED: AtomicU64 = AtomicU64::new(0);

type OutputDebugStringAFn = unsafe extern "system" fn(*const u8);
type OutputDebugStringWFn = unsafe extern "system" fn(*const u16);

thread_local! {
    /// Suppresses capture reentry on the thread that is currently forwarding a
    /// message. Thread-local (not global) so concurrent game threads never
    /// silence each other.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

/// Forwards one captured debug-channel message, splitting embedded newlines
/// so the unified sinks keep their one-line-per-record shape.
fn forward_game_message(message: &str, scope: &str) {
    for part in message.split('\n') {
        let part = part.trim_end_matches('\r').trim_end();
        if !part.is_empty() {
            logging::game_console_message(scope, part);
        }
    }
}

/// Resolves the module behind the detour caller. `FramesToSkip=2` skips this
/// helper and the detour itself; extra frames let the CRT/Win32 shims be
/// walked past when a module logs through a wrapper.
#[inline(never)]
fn caller_scope() -> Option<String> {
    let mut frames = [std::ptr::null_mut::<c_void>(); 4];
    let count = unsafe { RtlCaptureStackBackTrace(2, &mut frames, None) } as usize;
    if count == 0 {
        return None;
    }
    let addresses = frames[..count]
        .iter()
        .map(|frame| *frame as usize)
        .collect::<Vec<_>>();
    logging::scope_for_frames(&addresses)
}

unsafe extern "system" fn detour_output_debug_string_a(text: *const u8) {
    HOOK_SEEN.fetch_add(1, Ordering::Relaxed);
    let scope = caller_scope();
    if let Some(scope) = scope {
        if !INSIDE.with(|cell| cell.replace(true)) {
            if !text.is_null() {
                let message = unsafe { std::ffi::CStr::from_ptr(text as *const i8) }
                    .to_string_lossy()
                    .trim_end()
                    .to_string();
                if !message.trim().is_empty() {
                    forward_game_message(&message, &scope);
                    CAPTURED.fetch_add(1, Ordering::Relaxed);
                }
            }
            INSIDE.with(|cell| cell.set(false));
        }
    }
    let original = ORIGINAL_A.load(Ordering::Acquire);
    if original != 0 {
        let call: OutputDebugStringAFn = unsafe { std::mem::transmute(original) };
        unsafe { call(text) };
    }
}

unsafe extern "system" fn detour_output_debug_string_w(text: *const u16) {
    HOOK_SEEN.fetch_add(1, Ordering::Relaxed);
    let scope = caller_scope();
    if let Some(scope) = scope {
        if !INSIDE.with(|cell| cell.replace(true)) {
            if !text.is_null() {
                let mut length = 0usize;
                while unsafe { *text.add(length) } != 0 {
                    length += 1;
                }
                let slice = unsafe { std::slice::from_raw_parts(text, length) };
                let message = String::from_utf16_lossy(slice).trim_end().to_string();
                if !message.trim().is_empty() {
                    forward_game_message(&message, &scope);
                    CAPTURED.fetch_add(1, Ordering::Relaxed);
                }
            }
            INSIDE.with(|cell| cell.set(false));
        }
    }
    let original = ORIGINAL_W.load(Ordering::Acquire);
    if original != 0 {
        let call: OutputDebugStringWFn = unsafe { std::mem::transmute(original) };
        unsafe { call(text) };
    }
}

/// Installs the OutputDebugStringA/W hooks once and enables them explicitly.
/// MinHook creates hooks in a disabled state; without an explicit enable the
/// detours only ever fire if some unrelated module happens to call
/// enable_all_hooks afterwards. Returns true when at least one hook is live.
pub unsafe fn install() -> bool {
    if ORIGINAL_A.load(Ordering::Acquire) != 0 || ORIGINAL_W.load(Ordering::Acquire) != 0 {
        return false;
    }
    let mut installed = 0usize;
    for module_name in [w!("kernelbase.dll"), w!("kernel32.dll")] {
        let Ok(module) = GetModuleHandleW(module_name) else {
            continue;
        };
        if module.is_invalid() {
            continue;
        }
        for (export_name, export, detour, slot) in [
            (
                "OutputDebugStringA",
                s!("OutputDebugStringA"),
                detour_output_debug_string_a as *mut c_void,
                &ORIGINAL_A,
            ),
            (
                "OutputDebugStringW",
                s!("OutputDebugStringW"),
                detour_output_debug_string_w as *mut c_void,
                &ORIGINAL_W,
            ),
        ] {
            let Some(proc) = GetProcAddress(module, export) else {
                continue;
            };
            match MinHook::create_hook(proc as *mut c_void, detour) {
                Ok(trampoline) => match MinHook::enable_hook(proc as *mut c_void) {
                    Ok(()) => {
                        slot.store(trampoline as usize, Ordering::Release);
                        installed += 1;
                    }
                    Err(error) => {
                        logging::warn_message(&format!(
                            "[game-log] {export_name} enable failed: {error:?}"
                        ));
                        let _ = MinHook::remove_hook(proc as *mut c_void);
                    }
                },
                Err(error) => {
                    logging::warn_message(&format!(
                        "[game-log] {export_name} hook failed: {error:?}"
                    ));
                }
            }
        }
        if installed != 0 {
            break;
        }
    }
    if installed == 0 {
        logging::warn_message("game-log: OutputDebugString exports not found");
        return false;
    }
    logging::debug_message(&format!(
        "game-log: OutputDebugStringA/W hooks installed and enabled (count={installed})"
    ));
    spawn_channel_report();
    true
}

/// One-shot observability for the capture chain: `hook_seen` counts every
/// OutputDebugStringA/W call in the process, `captured` counts messages that
/// passed the module attribution and reached the unified sinks. seen=0 means
/// the game never used the channel; seen>0 with captured=0 points at the
/// attribution rules instead.
fn spawn_channel_report() {
    let _ = thread::Builder::new()
        .name("bloader-game-log-report".to_string())
        .spawn(|| {
            for (delay_secs, total_secs) in [(60u64, 60u64), (240, 300)] {
                thread::sleep(Duration::from_secs(delay_secs));
                // Pure diagnostics: keep them at DEBUG so console users do not
                // see periodic channel checks in the normal INFO stream.
                logging::debug_message(&format!(
                    "game-log channel check (after {total_secs}s): hook_seen={} captured={}",
                    HOOK_SEEN.load(Ordering::Relaxed),
                    CAPTURED.load(Ordering::Relaxed)
                ));
            }
        });
}

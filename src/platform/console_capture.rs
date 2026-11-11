// SPDX-License-Identifier: GPL-3.0-only
//! Compatibility capture for native DLL console output without requiring an SDK.
//! Installed after loader lock, before PreLoader/native DLL initialization.
//!
//! Every module that writes to the process console is captured, not just the
//! ones importing from kernel32: the game and most modern DLLs reach
//! `WriteConsoleA/W`/`WriteFile` through `api-ms-win-core-*` API sets, which
//! resolve to kernelbase. kernel32 only forwards to the same bodies, so the
//! hooks live in kernelbase and cover both import styles.

use core::ffi::c_void;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use minhook::MinHook;
use windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{PCSTR, w};
use crate::platform::logging;

const MAX_WRITE: usize = 64 * 1024;
const MAX_FRAGMENT_BUFFER: usize = 128 * 1024;

/// Win32/LeviLamina loggers split a single coloured logical line across
/// multiple WriteConsole calls. Keep the fragments until the newline arrives.
#[derive(Default)]
struct PendingConsoleLine {
    text: String,
    scope: Option<String>,
    scope_checked: bool,
}
const CP_ACP: u32 = 0;
static ATTEMPTED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static OBSERVED_FRAGMENTS: AtomicU64 = AtomicU64::new(0);
static OBSERVED_LINES: AtomicU64 = AtomicU64::new(0);
static ORIGINAL_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_A: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_FILE: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static BYPASS: Cell<bool> = const { Cell::new(false) };
    // A hooked implementation may re-enter another hooked export (for example
    // WriteFile on a console handle); only the outermost detour captures.
    static IN_DETOUR: Cell<bool> = const { Cell::new(false) };
    static PENDING_CONSOLE_LINE: RefCell<PendingConsoleLine> =
        RefCell::new(PendingConsoleLine::default());
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
    fn GetFileType(handle: *mut c_void) -> u32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        input: *const u8,
        input_len: i32,
        output: *mut u16,
        output_len: i32,
    ) -> i32;
}
type WriteFn = unsafe extern "system" fn(*mut c_void, *const c_void, u32, *mut u32, *mut c_void) -> i32;

pub fn with_passthrough<T>(f: impl FnOnce() -> T) -> T {
    BYPASS.with(|cell| {
        let old = cell.replace(true);
        struct Restore<'a>(&'a Cell<bool>, bool);
        impl Drop for Restore<'_> { fn drop(&mut self) { self.0.set(self.1); } }
        let _restore = Restore(cell, old);
        f()
    })
}

/// Indicates whether at least one native console write export is hooked.
/// This does not imply the external loader uses those APIs; the structured
/// file relay remains the fallback for other output transports.
pub fn is_active() -> bool {
    READY.load(Ordering::Acquire)
}

/// Actual native console traffic, not the presence of the MinHook trampolines.
pub fn capture_counts() -> (u64, u64) {
    (
        OBSERVED_FRAGMENTS.load(Ordering::Relaxed),
        OBSERVED_LINES.load(Ordering::Relaxed),
    )
}

pub fn install() {
    if !logging::console_is_ready() { return; }
    if ATTEMPTED.swap(true, Ordering::AcqRel) { return; }
    let mut installed = 0usize;
    for (name, replacement, original) in [
        (b"WriteConsoleW\0".as_slice(), intercept_w as *mut c_void, &ORIGINAL_W),
        (b"WriteConsoleA\0".as_slice(), intercept_a as *mut c_void, &ORIGINAL_A),
        (b"WriteFile\0".as_slice(), intercept_file as *mut c_void, &ORIGINAL_FILE),
    ] {
        // kernel32 re-exports these as 6-byte jumps into kernelbase
        // (`ff 25 ..`), so hooking kernelbase's implementation covers both
        // kernel32 importers (LeviLamina) and api-ms-win-core-* importers
        // (the game) with a single hook and a single trampoline.
        for module_name in [w!("kernelbase.dll"), w!("kernel32.dll")] {
            let Ok(kernel) = (unsafe { GetModuleHandleW(module_name) }) else { continue };
            if kernel.is_invalid() { continue; }
            let Some(target) = (unsafe { GetProcAddress(kernel, PCSTR(name.as_ptr())) }) else { continue };
            let target = target as *const () as *mut c_void;
            match unsafe { MinHook::create_hook(target, replacement) } {
                Ok(trampoline) => {
                    // Publish the trampoline BEFORE enabling the detour.
                    // Other native loader/logger threads may already emit
                    // WriteConsole/WriteFile concurrently at startup.
                    original.store(trampoline as usize, Ordering::Release);
                    match unsafe { MinHook::enable_hook(target) } {
                        Ok(()) => {
                            installed += 1;
                            break;
                        }
                        Err(error) => {
                            let _ = unsafe { MinHook::remove_hook(target) };
                            original.store(0, Ordering::Release);
                            logging::scoped_warn_message(
                                "console-capture",
                                &format!("enable hook failed: {error:?}"),
                            );
                        }
                    }
                },
                Err(error) => logging::scoped_warn_message(
                    "console-capture",
                    &format!("create hook failed: {error:?}"),
                ),
            }
        }
    }
    if installed == 0 {
        // A later console initialization may succeed where this attempt did
        // not (for example when the console handle arrives after OEP).
        ATTEMPTED.store(false, Ordering::Release);
        logging::scoped_warn_message("console-capture", "no console write export could be hooked");
        return;
    }
    READY.store(true, Ordering::Release);
    logging::scoped_debug_message("console-capture", &format!(
        "native console compatibility active | hooks={installed} | modules=kernelbase | no_sdk=true"
    ));
}

unsafe fn call_original(original: &AtomicUsize, h: *mut c_void, data: *const c_void,
                        size: u32, count: *mut u32, extra: *mut c_void) -> i32 {
    let addr = original.load(Ordering::Acquire);
    if addr == 0 { return 0; }
    let function: WriteFn = unsafe { std::mem::transmute(addr) };
    unsafe { function(h, data, size, count, extra) }
}

fn eligible(handle: *mut c_void) -> bool {
    if !READY.load(Ordering::Acquire) || !logging::console_is_ready()
        || BYPASS.with(Cell::get) || handle.is_null() || handle as usize == usize::MAX {
        return false;
    }
    let mut mode = 0;
    unsafe { GetConsoleMode(handle, &mut mode) != 0 }
}

/// Resolves the module behind the detour caller. `FramesToSkip=2` skips this
/// helper and the detour itself; several frames are captured because native
/// Mods often log through the CRT (`fwrite` -> ucrtbase -> WriteFile), and the
/// CRT frame must be walked past to reach the Mod.
#[inline(never)]
fn caller_scope() -> Option<String> {
    let mut frames = [std::ptr::null_mut::<c_void>(); 8];
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

/// ANSI console writes are not necessarily UTF-8; decode with the system code
/// page so native loaders printing localized text do not turn into mojibake.
fn decode_ansi(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    unsafe {
        let needed = MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if needed <= 0 {
            return String::from_utf8_lossy(bytes).to_string();
        }
        let mut buffer = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            buffer.as_mut_ptr(),
            needed,
        );
        if written <= 0 {
            return String::from_utf8_lossy(bytes).to_string();
        }
        String::from_utf16_lossy(&buffer[..written as usize])
    }
}

fn capture(_handle: *mut c_void, s: &str, resolve_scope: impl FnOnce() -> Option<String>) -> bool {
    if s.contains('\0') {
        return false;
    }
    if s.is_empty() {
        return true;
    }
    OBSERVED_FRAGMENTS.fetch_add(1, Ordering::Relaxed);
    PENDING_CONSOLE_LINE.with(|cell| {
        let mut pending = cell.borrow_mut();
        // A single colourful log line may use dozens of WriteConsole calls.
        // Stack walking and module attribution are only needed for its first
        // fragment, not for every subsequent ANSI/text write.
        if !pending.scope_checked {
            pending.scope = resolve_scope();
            pending.scope_checked = true;
        }
        pending.text.push_str(s);

        // Never let a logger that does not terminate a line grow memory forever.
        if pending.text.len() > MAX_FRAGMENT_BUFFER {
            pending.text.clear();
            pending.scope = None;
            pending.scope_checked = false;
            return;
        }

        while let Some(end) = pending.text.find('\n') {
            let line = pending.text[..end].trim_end_matches('\r').to_string();
            pending.text.drain(..=end);
            let attribution = pending.scope.clone();
            if pending.text.is_empty() {
                pending.scope = None;
                pending.scope_checked = false;
            }
            if !line.is_empty() {
                OBSERVED_LINES.fetch_add(1, Ordering::Relaxed);
                logging::captured_console_output_from(attribution.as_deref(), &line);
            }
        }
    });
    true
}

/// Clears the per-thread detour marker when the outermost detour returns.
struct DetourGuard;

impl Drop for DetourGuard {
    fn drop(&mut self) {
        IN_DETOUR.with(|cell| cell.set(false));
    }
}

unsafe extern "system" fn intercept_w(h: *mut c_void, data: *const c_void, len: u32,
                                        count: *mut u32, reserved: *mut c_void) -> i32 {
    if IN_DETOUR.with(Cell::get) {
        return unsafe { call_original(&ORIGINAL_W, h, data, len, count, reserved) };
    }
    IN_DETOUR.with(|cell| cell.set(true));
    let _guard = DetourGuard;
    if !data.is_null() && len > 0 && (len as usize) <= MAX_WRITE && eligible(h) {
        let text = {
            let slice = unsafe { std::slice::from_raw_parts(data as *const u16, len as usize) };
            String::from_utf16_lossy(slice)
        };
        // Attribution must be captured here, not inside the closure: the
        // backtrace walk skips exactly two frames (helper + detour).
        if std::panic::catch_unwind(|| capture(h, &text, caller_scope)).unwrap_or(false) {
            if !count.is_null() { unsafe { *count = len }; }
            return 1;
        }
    }
    unsafe { call_original(&ORIGINAL_W, h, data, len, count, reserved) }
}

unsafe extern "system" fn intercept_a(h: *mut c_void, data: *const c_void, len: u32,
                                        count: *mut u32, reserved: *mut c_void) -> i32 {
    if IN_DETOUR.with(Cell::get) {
        return unsafe { call_original(&ORIGINAL_A, h, data, len, count, reserved) };
    }
    IN_DETOUR.with(|cell| cell.set(true));
    let _guard = DetourGuard;
    if !data.is_null() && len > 0 && (len as usize) <= MAX_WRITE && eligible(h) {
        let text = {
            let slice = unsafe { std::slice::from_raw_parts(data as *const u8, len as usize) };
            decode_ansi(slice)
        };
        if std::panic::catch_unwind(|| capture(h, &text, caller_scope)).unwrap_or(false) {
            if !count.is_null() { unsafe { *count = len }; }
            return 1;
        }
    }
    unsafe { call_original(&ORIGINAL_A, h, data, len, count, reserved) }
}

unsafe extern "system" fn intercept_file(h: *mut c_void, data: *const c_void, len: u32,
                                           count: *mut u32, overlapped: *mut c_void) -> i32 {
    // FILE_TYPE_CHAR=2; the common file/pipe path falls through unchanged.
    if IN_DETOUR.with(Cell::get) {
        return unsafe { call_original(&ORIGINAL_FILE, h, data, len, count, overlapped) };
    }
    IN_DETOUR.with(|cell| cell.set(true));
    let _guard = DetourGuard;
    if !data.is_null() && len > 0 && (len as usize) <= MAX_WRITE && overlapped.is_null()
        && READY.load(Ordering::Acquire) && !BYPASS.with(Cell::get)
        && unsafe { GetFileType(h) } == 2 && eligible(h)
    {
        let slice = unsafe { std::slice::from_raw_parts(data as *const u8, len as usize) };
        // Binary console output (or an ANSI escape fragment) is not a log
        // line; hand it back to the original writer untouched.
        if slice.iter().all(|&b| b >= 0x20 || matches!(b, b'\r' | b'\n' | b'\t')) {
            let text = decode_ansi(slice);
            if std::panic::catch_unwind(|| capture(h, &text, caller_scope)).unwrap_or(false) {
                if !count.is_null() { unsafe { *count = len }; }
                return 1;
            }
        }
    }
    unsafe { call_original(&ORIGINAL_FILE, h, data, len, count, overlapped) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_colored_fragments_with_single_attribution() {
        let calls = std::cell::Cell::new(0usize);
        // Callback called only when opening a new logical line.
        let resolve = || {
            calls.set(calls.get() + 1);
            Some("mod:LeviLamina".to_string())
        };
        assert!(capture(std::ptr::null_mut(), "[19:03:25 INFO] ", resolve));
        assert!(capture(std::ptr::null_mut(), "[LeviLamina] line", resolve));
        assert!(capture(std::ptr::null_mut(), "\n", resolve));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn unresolved_mod_attribution_is_not_retried_for_each_fragment() {
        let calls = std::cell::Cell::new(0usize);
        let unknown = || {
            calls.set(calls.get() + 1);
            None
        };
        assert!(capture(std::ptr::null_mut(), "prefix", unknown));
        assert!(capture(std::ptr::null_mut(), " body", unknown));
        assert!(capture(std::ptr::null_mut(), "\n", unknown));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn guard_restores_original_mode() {
        assert!(!BYPASS.with(Cell::get));
        with_passthrough(|| {
            assert!(BYPASS.with(Cell::get));
            with_passthrough(|| assert!(BYPASS.with(Cell::get)));
        });
        assert!(!BYPASS.with(Cell::get));
    }

    #[test]
    fn ansi_decoding_keeps_utf8_and_never_panics_on_invalid_bytes() {
        // Valid UTF-8 round-trips unchanged; non-UTF-8 bytes fall back to the
        // system code page instead of panicking.
        assert_eq!(decode_ansi("版本".as_bytes()), "版本");
        let decoded = decode_ansi(&[0xB0, 0xE6, 0xB1, 0xBE]);
        assert!(!decoded.is_empty());
    }
}

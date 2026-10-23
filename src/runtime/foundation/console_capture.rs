// SPDX-License-Identifier: GPL-3.0-only
//! Compatibility capture for native DLL console output without requiring an SDK.
//! Installed after loader lock, before PreLoader/native DLL initialization.

use core::ffi::c_void;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use minhook::MinHook;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{PCSTR, w};
use crate::runtime::foundation::logging;

const MAX_WRITE: usize = 64 * 1024;
static ATTEMPTED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static ORIGINAL_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_A: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_FILE: AtomicUsize = AtomicUsize::new(0);
thread_local! { static BYPASS: Cell<bool> = const { Cell::new(false) }; }

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
    fn GetFileType(handle: *mut c_void) -> u32;
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

pub fn install() {
    if !logging::console_is_ready() { return; }
    if ATTEMPTED.swap(true, Ordering::AcqRel) { return; }
    let Ok(kernel) = (unsafe { GetModuleHandleW(w!("kernel32.dll")) }) else { return };
    let count = [
        hook(kernel, b"WriteConsoleW\0", intercept_w as *mut c_void, &ORIGINAL_W),
        hook(kernel, b"WriteConsoleA\0", intercept_a as *mut c_void, &ORIGINAL_A),
        hook(kernel, b"WriteFile\0", intercept_file as *mut c_void, &ORIGINAL_FILE),
    ].iter().filter(|&&ok| ok).count();
    if count != 0 {
        match unsafe { MinHook::enable_all_hooks() } {
            Ok(()) => {
                READY.store(true, Ordering::Release);
                logging::scoped_debug_message("console-capture", &format!(
                    "native console compatibility active | hooks={count} | no_sdk=true"
                ));
            }
            Err(e) => logging::scoped_warn_message("console-capture", &format!("enable hooks failed: {e:?}")),
        }
    }
}

fn hook(module: windows::Win32::Foundation::HMODULE, name: &'static [u8],
        replacement: *mut c_void, original: &AtomicUsize) -> bool {
    let Some(target) = (unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) }) else { return false };
    match unsafe { MinHook::create_hook(target as *const () as *mut c_void, replacement) } {
        Ok(trampoline) => { original.store(trampoline as usize, Ordering::Release); true }
        Err(_) => false,
    }
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

fn capture(s: &str) -> bool {
    if s.contains('\0') { return false; }
    let mut seen = false;
    for piece in s.split('\n') {
        let part = piece.trim_end_matches('\r');
        if part.is_empty() { continue; }
        logging::captured_console_output(part);
        seen = true;
    }
    seen
}

unsafe extern "system" fn intercept_w(h: *mut c_void, data: *const c_void, len: u32,
                                        count: *mut u32, reserved: *mut c_void) -> i32 {
    if !data.is_null() && len > 0 && (len as usize) <= MAX_WRITE && eligible(h) {
        let handled = std::panic::catch_unwind(|| {
            let slice = unsafe { std::slice::from_raw_parts(data as *const u16, len as usize) };
            capture(&String::from_utf16_lossy(slice))
        }).unwrap_or(false);
        if handled {
            if !count.is_null() { unsafe { *count = len }; }
            return 1;
        }
    }
    unsafe { call_original(&ORIGINAL_W, h, data, len, count, reserved) }
}

unsafe extern "system" fn intercept_a(h: *mut c_void, data: *const c_void, len: u32,
                                        count: *mut u32, reserved: *mut c_void) -> i32 {
    if !data.is_null() && len > 0 && (len as usize) <= MAX_WRITE && eligible(h) {
        let handled = std::panic::catch_unwind(|| {
            let slice = unsafe { std::slice::from_raw_parts(data as *const u8, len as usize) };
            capture(&String::from_utf8_lossy(slice))
        }).unwrap_or(false);
        if handled {
            if !count.is_null() { unsafe { *count = len }; }
            return 1;
        }
    }
    unsafe { call_original(&ORIGINAL_A, h, data, len, count, reserved) }
}

unsafe extern "system" fn intercept_file(h: *mut c_void, data: *const c_void, len: u32,
                                           count: *mut u32, overlapped: *mut c_void) -> i32 {
    // FILE_TYPE_CHAR=2; the common file/pipe path falls through unchanged.
    if !data.is_null() && len > 0 && (len as usize) <= MAX_WRITE && overlapped.is_null()
        && READY.load(Ordering::Acquire) && !BYPASS.with(Cell::get)
        && unsafe { GetFileType(h) } == 2 && eligible(h) {
        let handled = std::panic::catch_unwind(|| {
            let slice = unsafe { std::slice::from_raw_parts(data as *const u8, len as usize) };
            if !slice.iter().all(|&b| b >= 0x20 || matches!(b, b'\r' | b'\n' | b'\t')) {
                return false;
            }
            capture(&String::from_utf8_lossy(slice))
        }).unwrap_or(false);
        if handled {
            if !count.is_null() { unsafe { *count = len }; }
            return 1;
        }
    }
    unsafe { call_original(&ORIGINAL_FILE, h, data, len, count, overlapped) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_restores_original_mode() {
        assert!(!BYPASS.with(Cell::get));
        with_passthrough(|| {
            assert!(BYPASS.with(Cell::get));
            with_passthrough(|| assert!(BYPASS.with(Cell::get)));
        });
        assert!(!BYPASS.with(Cell::get));
    }
}

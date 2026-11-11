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
use windows::Win32::Foundation::{GetLastError, SetLastError};
use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

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

// LeviLamina's ConsoleSink falls back to fwrite(stdout) when its CRT stdout
// is not a terminal. Its FileSink buffers INFO without an immediate flush,
// so we must observe the live CRT streams directly. Intercept only
// the CRT's standard output streams; never rebind or flush its FILE* objects.
static CRT_FWRITE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static CRT_STDOUT: AtomicUsize = AtomicUsize::new(0);
static CRT_STDERR: AtomicUsize = AtomicUsize::new(0);
static CRT_FWRITE_READY: AtomicBool = AtomicBool::new(false);
static CRT_FWRITE_INSTALLING: AtomicBool = AtomicBool::new(false);
static UCRT_FILENO: AtomicUsize = AtomicUsize::new(0);
static UCRT_OSFHANDLE: AtomicUsize = AtomicUsize::new(0);
static MSVCRT_OSFHANDLE: AtomicUsize = AtomicUsize::new(0);
static UCRT_WRITE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static MSVCRT_WRITE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static UCRT_WRITE_READY: AtomicBool = AtomicBool::new(false);
static MSVCRT_WRITE_READY: AtomicBool = AtomicBool::new(false);
static CRT_WRITE_INSTALLING: AtomicBool = AtomicBool::new(false);
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
type FwriteFn = unsafe extern "C" fn(*const c_void, usize, usize, *mut c_void) -> usize;
type CrtWriteFn = unsafe extern "C" fn(i32, *const c_void, u32) -> i32;
type CrtOsfHandleFn = unsafe extern "C" fn(i32) -> isize;
type CrtFilenoFn = unsafe extern "C" fn(*mut c_void) -> i32;
type CrtIobFn = unsafe extern "C" fn(i32) -> *mut c_void;

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
/// Hook installation does not prove that any external writes were observed.
pub fn is_active() -> bool {
    READY.load(Ordering::Acquire)
}

/// True only when the ucrtbase fwrite fallback has an active trampoline.
/// This is independent of Win32 WriteConsole/WriteFile capture activation.
pub fn crt_fwrite_capture_active() -> bool {
    CRT_FWRITE_READY.load(Ordering::Acquire)
}

/// Installs a passive write observer for LeviLamina's documented
/// fwrite(stdout) fallback. No CRT FILE*, file descriptor, or return value
/// is modified, and non-stdio fwrite calls are forwarded without inspection.
pub fn install_crt_fwrite_capture() {
    if CRT_FWRITE_READY.load(Ordering::Acquire)
        || CRT_FWRITE_INSTALLING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    struct ResetInstalling;
    impl Drop for ResetInstalling {
        fn drop(&mut self) {
            CRT_FWRITE_INSTALLING.store(false, Ordering::Release);
        }
    }
    let _reset = ResetInstalling;
    if !logging::console_is_ready() {
        return;
    }
    let Ok(module) = (unsafe { GetModuleHandleW(w!("ucrtbase.dll")) }) else {
        // Some versions load ucrtbase only with PreLoader's dependency graph.
        // The caller retries once after loading PreLoader.
        return;
    };
    let Some(iob_address) = (unsafe { GetProcAddress(module, PCSTR(b"__acrt_iob_func\0".as_ptr())) }) else {
        return;
    };
    let Some(fwrite_address) = (unsafe { GetProcAddress(module, PCSTR(b"fwrite\0".as_ptr())) }) else {
        return;
    };
    let Some(fileno_address) = (unsafe { GetProcAddress(module, PCSTR(b"_fileno\0".as_ptr())) }) else {
        return;
    };
    let Some(osfhandle_address) = (unsafe { GetProcAddress(module, PCSTR(b"_get_osfhandle\0".as_ptr())) }) else {
        return;
    };
    let get_iob: CrtIobFn = unsafe { std::mem::transmute(iob_address) };
    let stdout_stream = unsafe { get_iob(1) };
    let stderr_stream = unsafe { get_iob(2) };
    if stdout_stream.is_null() || stderr_stream.is_null() {
        return;
    }
    let target = fwrite_address as *const () as *mut c_void;
    match unsafe { MinHook::create_hook(target, intercept_crt_fwrite as *mut c_void) } {
        Ok(original) => {
            // Store the original and the exact ucrtbase FILE* addresses
            // before enabling the detour for other native logger threads.
            CRT_FWRITE_ORIGINAL.store(original as usize, Ordering::Release);
            CRT_STDOUT.store(stdout_stream as usize, Ordering::Release);
            CRT_STDERR.store(stderr_stream as usize, Ordering::Release);
            UCRT_FILENO.store(fileno_address as usize, Ordering::Release);
            UCRT_OSFHANDLE.store(osfhandle_address as usize, Ordering::Release);
            match unsafe { MinHook::enable_hook(target) } {
                Ok(()) => {
                    CRT_FWRITE_READY.store(true, Ordering::Release);
                    logging::scoped_debug_message(
                        "console-capture",
                        "CRT stdout/stderr fallback capture active (ucrtbase!fwrite, streams=readonly, output=passthrough)",
                    );
                }
                Err(error) => {
                    let _ = unsafe { MinHook::remove_hook(target) };
                    CRT_FWRITE_ORIGINAL.store(0, Ordering::Release);
                    CRT_STDOUT.store(0, Ordering::Release);
                    CRT_STDERR.store(0, Ordering::Release);
                    UCRT_FILENO.store(0, Ordering::Release);
                    UCRT_OSFHANDLE.store(0, Ordering::Release);
                    logging::scoped_debug_message(
                        "console-capture",
                        &format!("optional CRT fwrite capture unavailable: {error:?}"),
                    );
                }
            }
        }
        Err(error) => {
            logging::scoped_debug_message(
                "console-capture",
                &format!("optional CRT fwrite capture not installed: {error:?}"),
            );
        }
    }
}


pub fn crt_write_capture_active() -> bool {
    UCRT_WRITE_READY.load(Ordering::Acquire)
        || MSVCRT_WRITE_READY.load(Ordering::Acquire)
}

/// Observe stdout and stderr from native CRTs without rebinding their FILE*
/// or redirecting any file descriptors. A Mod may use _write rather than fwrite.
pub fn install_crt_write_capture() {
    if !logging::console_is_ready() || CRT_WRITE_INSTALLING.swap(true, Ordering::AcqRel) {
        return;
    }
    struct ResetInstalling;
    impl Drop for ResetInstalling {
        fn drop(&mut self) {
            CRT_WRITE_INSTALLING.store(false, Ordering::Release);
        }
    }
    let _reset = ResetInstalling;
    for (dll, original, ready, detour) in [
        (w!("ucrtbase.dll"), &UCRT_WRITE_ORIGINAL, &UCRT_WRITE_READY, intercept_ucrt_write as *mut c_void),
        (w!("msvcrt.dll"), &MSVCRT_WRITE_ORIGINAL, &MSVCRT_WRITE_READY, intercept_msvcrt_write as *mut c_void),
    ] {
        if ready.load(Ordering::Acquire) {
            continue;
        }
        let Ok(module) = (unsafe { GetModuleHandleW(dll) }) else { continue };
        let Some(address) = (unsafe { GetProcAddress(module, PCSTR(b"_write\0".as_ptr())) }) else { continue };
        let Some(osfhandle_address) = (unsafe { GetProcAddress(module, PCSTR(b"_get_osfhandle\0".as_ptr())) }) else { continue };
        let osfhandle = if std::ptr::eq(original, &UCRT_WRITE_ORIGINAL) {
            &UCRT_OSFHANDLE
        } else {
            &MSVCRT_OSFHANDLE
        };
        let target = address as *const () as *mut c_void;
        match unsafe { MinHook::create_hook(target, detour) } {
            Ok(trampoline) => {
                original.store(trampoline as usize, Ordering::Release);
                osfhandle.store(osfhandle_address as usize, Ordering::Release);
                match unsafe { MinHook::enable_hook(target) } {
                    Ok(()) => { ready.store(true, Ordering::Release); }
                    Err(error) => {
                        let _ = unsafe { MinHook::remove_hook(target) };
                        original.store(0, Ordering::Release);
                        osfhandle.store(0, Ordering::Release);
                        logging::scoped_debug_message("console-capture",
                            &format!("CRT _write observer enable failed: {error:?}"));
                    }
                }
            }
            Err(error) => logging::scoped_debug_message("console-capture",
                &format!("CRT _write observer creation failed: {error:?}")),
        }
    }
}

unsafe extern "C" fn intercept_ucrt_write(fd: i32, data: *const c_void, len: u32) -> i32 {
    unsafe { observe_crt_write(&UCRT_WRITE_ORIGINAL, &UCRT_WRITE_READY, &UCRT_OSFHANDLE, fd, data, len) }
}

unsafe extern "C" fn intercept_msvcrt_write(fd: i32, data: *const c_void, len: u32) -> i32 {
    unsafe { observe_crt_write(&MSVCRT_WRITE_ORIGINAL, &MSVCRT_WRITE_READY, &MSVCRT_OSFHANDLE, fd, data, len) }
}

unsafe fn observe_crt_write(
    trampoline: &AtomicUsize, ready: &AtomicBool, osfhandle: &AtomicUsize, fd: i32,
    data: *const c_void, len: u32,
) -> i32 {
    let address = trampoline.load(Ordering::Acquire);
    if address == 0 { return -1; }
    let original: CrtWriteFn = unsafe { std::mem::transmute(address) };
    if !ready.load(Ordering::Acquire) || !matches!(fd, 1 | 2)
        || data.is_null() || BYPASS.with(Cell::get) || IN_DETOUR.with(Cell::get)
    {
        return unsafe { original(fd, data, len) };
    }
    IN_DETOUR.with(|state| state.set(true));
    let _guard = DetourGuard;
    let written = unsafe { original(fd, data, len) };
    let last_error = unsafe { GetLastError() };
    if written > 0 && (written as usize) <= MAX_WRITE
        && crt_fd_has_console_transport(fd, osfhandle)
    {
        let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, written as usize) };
        if !bytes.contains(&0) {
            let _ = std::panic::catch_unwind(|| {
                let decoded = decode_ansi(bytes);
                capture(std::ptr::null_mut(), &decoded, caller_scope);
            });
        }
    }
    unsafe { SetLastError(last_error) };
    written
}

/// Only CONOUT$/console handles and anonymous or named pipes are eligible.
/// FILE_TYPE_DISK is never an output capture source, even when stdout was
/// explicitly redirected to a .log file.
fn supported_console_file_type(file_type: u32, is_console: bool) -> bool {
    // FILE_TYPE_CHAR without GetConsoleMode may be NUL or another character
    // device, not a console. FILE_TYPE_DISK is always a private file.
    file_type == 3 || (file_type == 2 && is_console)
}

fn live_console_transport(handle: *mut c_void) -> bool {
    if handle.is_null() || handle as usize == usize::MAX { return false; }
    let file_type = unsafe { GetFileType(handle) };
    let is_console = if file_type == 2 {
        let mut mode = 0;
        unsafe { GetConsoleMode(handle, &mut mode) != 0 }
    } else {
        false
    };
    supported_console_file_type(file_type, is_console)
}

fn crt_fd_has_console_transport(fd: i32, osfhandle: &AtomicUsize) -> bool {
    let address = osfhandle.load(Ordering::Acquire);
    if address == 0 || !matches!(fd, 1 | 2) { return false; }
    let get_osfhandle: CrtOsfHandleFn = unsafe { std::mem::transmute(address) };
    let handle = unsafe { get_osfhandle(fd) };
    if handle == -1 || handle == -2 { return false; }
    live_console_transport(handle as *mut c_void)
}

fn ucrt_stream_has_console_transport(stream: *mut c_void) -> bool {
    let address = UCRT_FILENO.load(Ordering::Acquire);
    if address == 0 { return false; }
    let fileno: CrtFilenoFn = unsafe { std::mem::transmute(address) };
    let fd = unsafe { fileno(stream) };
    crt_fd_has_console_transport(fd, &UCRT_OSFHANDLE)
}

fn bounded_crt_write_length(size: usize, count: usize) -> Option<usize> {
    size.checked_mul(count).filter(|length| *length > 0 && *length <= MAX_WRITE)
}

unsafe extern "C" fn intercept_crt_fwrite(
    data: *const c_void,
    size: usize,
    count: usize,
    stream: *mut c_void,
) -> usize {
    let original_address = CRT_FWRITE_ORIGINAL.load(Ordering::Acquire);
    if original_address == 0 {
        return 0;
    }
    let original: FwriteFn = unsafe { std::mem::transmute(original_address) };
    let stdout = CRT_STDOUT.load(Ordering::Acquire);
    let stderr = CRT_STDERR.load(Ordering::Acquire);
    if !CRT_FWRITE_READY.load(Ordering::Acquire)
        || (stream as usize != stdout && stream as usize != stderr)
        || BYPASS.with(Cell::get)
        || IN_DETOUR.with(Cell::get)
    {
        return unsafe { original(data, size, count, stream) };
    }

    IN_DETOUR.with(|in_detour| in_detour.set(true));
    let _guard = DetourGuard;
    // Call through unchanged. In particular do not lie about fwrite's return
    // when the external CRT stdout is invalid or points to a closed pipe.
    let result = unsafe { original(data, size, count, stream) };
    let last_error = unsafe { GetLastError() };
    if !data.is_null() && result > 0 && ucrt_stream_has_console_transport(stream) {
        if let Some(length) = bounded_crt_write_length(size, result) {
            let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, length) };
            // No attempt is made to parse binary output or arbitrary FILE*.
            if !bytes.contains(&0) {
                let _ = std::panic::catch_unwind(|| {
                    let decoded = decode_ansi(bytes);
                    let _ = capture(std::ptr::null_mut(), &decoded, caller_scope);
                });
            }
        }
    }
    unsafe { SetLastError(last_error) };
    result
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

/// Only observe redirected stdout/stderr, never arbitrary file handles. This
/// captures native pipes while avoiding Mod files and on-disk logs.
fn redirected_std_handle(handle: *mut c_void) -> bool {
    if !READY.load(Ordering::Acquire) || !logging::console_is_ready()
        || BYPASS.with(Cell::get) || handle.is_null() || handle as usize == usize::MAX
    { return false; }
    let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE).ok() };
    let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE).ok() };
    if !stdout.is_some_and(|value| value.0 == handle)
        && !stderr.is_some_and(|value| value.0 == handle) { return false; }
    let mut mode = 0;
    unsafe { GetConsoleMode(handle, &mut mode) == 0 } && live_console_transport(handle)
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
    if IN_DETOUR.with(Cell::get) {
        return unsafe { call_original(&ORIGINAL_FILE, h, data, len, count, overlapped) };
    }
    IN_DETOUR.with(|cell| cell.set(true));
    let _guard = DetourGuard;
    if data.is_null() || len == 0 || (len as usize) > MAX_WRITE || !overlapped.is_null() {
        return unsafe { call_original(&ORIGINAL_FILE, h, data, len, count, overlapped) };
    }
    let is_console = eligible(h);
    let is_redirected_stdout = !is_console && redirected_std_handle(h);
    if !is_console && !is_redirected_stdout {
        return unsafe { call_original(&ORIGINAL_FILE, h, data, len, count, overlapped) };
    }
    let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, len as usize) };
    // Keep VT colour fragments until a full line is available for sanitizing.
    if bytes.contains(&0) {
        return unsafe { call_original(&ORIGINAL_FILE, h, data, len, count, overlapped) };
    }
    if is_console {
        let decoded = decode_ansi(bytes);
        if std::panic::catch_unwind(|| capture(h, &decoded, caller_scope)).unwrap_or(false) {
            if !count.is_null() { unsafe { *count = len }; }
            return 1;
        }
        return unsafe { call_original(&ORIGINAL_FILE, h, data, len, count, overlapped) };
    }

    // Pipes must receive their original bytes. Capture only actual successful
    // writes and preserve the native return value and last-error state.
    let result = unsafe { call_original(&ORIGINAL_FILE, h, data, len, count, overlapped) };
    let last_error = unsafe { GetLastError() };
    if result != 0 {
        let written = if count.is_null() { len } else { unsafe { *count }.min(len) };
        if written > 0 {
            let output = &bytes[..written as usize];
            let _ = std::panic::catch_unwind(|| {
                let decoded = decode_ansi(output);
                capture(h, &decoded, caller_scope);
            });
        }
    }
    unsafe { SetLastError(last_error) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn never_observe_disk_file_log_handles() {
        assert!(!supported_console_file_type(1, false)); // FILE_TYPE_DISK
        assert!(!supported_console_file_type(1, true));
        assert!(!supported_console_file_type(0, false)); // FILE_TYPE_UNKNOWN
        assert!(!supported_console_file_type(u32::MAX, false));
        assert!(!supported_console_file_type(2, false)); // NUL / non-console char device
        assert!(supported_console_file_type(2, true)); // console
        assert!(supported_console_file_type(3, false)); // redirected pipe
    }

    #[test]
    fn crt_write_observation_never_overflows_or_decodes_huge_buffers() {
        assert_eq!(bounded_crt_write_length(1, 8), Some(8));
        assert_eq!(bounded_crt_write_length(2, 3), Some(6));
        assert_eq!(bounded_crt_write_length(0, 9), None);
        assert_eq!(bounded_crt_write_length(usize::MAX, 2), None);
        assert_eq!(bounded_crt_write_length(MAX_WRITE + 1, 1), None);
    }

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

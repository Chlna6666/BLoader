use std::ffi::c_void;
use std::fs::File;
use std::io::Read;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::thread;

use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentThreadId};
use windows::core::{PCSTR, PCWSTR};

use crate::runtime::foundation::{logging, mod_diagnostics};
use mod_diagnostics::ModIdentity;

const O_TEXT: i32 = 0x4000;
const STDOUT_FILENO: i32 = 1;
const STDERR_FILENO: i32 = 2;
const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreatePipe(
        read_pipe: *mut HANDLE,
        write_pipe: *mut HANDLE,
        security_attributes: *const c_void,
        size: u32,
    ) -> i32;
    fn SetHandleInformation(handle: HANDLE, mask: u32, flags: u32) -> i32;
}

#[derive(Clone)]
struct CapturedLine {
    identity: Option<ModIdentity>,
    stream: String,
    line: String,
}

#[derive(Clone, Debug)]
pub struct ActiveCaptureSnapshot {
    pub thread_id: u32,
    pub identity: ModIdentity,
    pub phase: String,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

struct ActiveCaptureGuard {
    thread_id: u32,
    identity_id: String,
    phase: String,
}

#[derive(Clone, Copy)]
struct CrtApi {
    dup: unsafe extern "C" fn(i32) -> i32,
    dup2: unsafe extern "C" fn(i32, i32) -> i32,
    close: unsafe extern "C" fn(i32) -> i32,
    open_osfhandle: unsafe extern "C" fn(isize, i32) -> i32,
    flushall: unsafe extern "C" fn() -> i32,
    iob_func: Option<unsafe extern "C" fn(u32) -> *mut c_void>,
    setvbuf: Option<unsafe extern "C" fn(*mut c_void, *mut i8, i32, usize) -> i32>,
}

struct CrtBinding {
    _module_name: String,
    api: CrtApi,
    _saved_stdout: i32,
    _saved_stderr: i32,
}

struct CrtRedirect {
    _bindings: Vec<CrtBinding>,
}

static PENDING_LINES: OnceLock<Mutex<Vec<CapturedLine>>> = OnceLock::new();
static PROCESS_CAPTURE_INSTALLED: OnceLock<()> = OnceLock::new();
static ACTIVE_CAPTURES: OnceLock<Mutex<Vec<ActiveCaptureSnapshot>>> = OnceLock::new();
static KEPT_WRITE_FILES: OnceLock<Mutex<Vec<File>>> = OnceLock::new();

fn pending_lines() -> &'static Mutex<Vec<CapturedLine>> {
    PENDING_LINES.get_or_init(|| Mutex::new(Vec::new()))
}

fn active_captures() -> &'static Mutex<Vec<ActiveCaptureSnapshot>> {
    ACTIVE_CAPTURES.get_or_init(|| Mutex::new(Vec::new()))
}

fn kept_write_files() -> &'static Mutex<Vec<File>> {
    KEPT_WRITE_FILES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Per-Mod load capture is now metadata-only. Process stdout/stderr is already
/// routed through anonymous pipes after OEP, so creating temporary files is not
/// required and would defeat the no-disk isolation experiment.
pub unsafe fn capture_library_load<T>(
    identity: &ModIdentity,
    phase: &str,
    f: impl FnOnce() -> T,
) -> T {
    let _active = ActiveCaptureGuard::push(identity, phase);
    mod_diagnostics::record_lifecycle(
        identity,
        "stdio_capture_begin",
        &format!("phase={phase} mode=memory-pipe"),
    );
    let result = f();
    mod_diagnostics::record_lifecycle(identity, "stdio_capture_ready", phase);
    result
}

pub unsafe fn install_process_capture() {
    if PROCESS_CAPTURE_INSTALLED.set(()).is_err() {
        return;
    }

    let Some((stdout_read, stdout_write)) = create_memory_pipe() else {
        logging::scoped_error_message("stdio-capture", "failed to create stdout memory pipe");
        return;
    };
    let Some((stderr_read, stderr_write)) = create_memory_pipe() else {
        logging::scoped_error_message("stdio-capture", "failed to create stderr memory pipe");
        return;
    };

    let stdout_handle = HANDLE(stdout_write.as_raw_handle() as *mut c_void);
    let stderr_handle = HANDLE(stderr_write.as_raw_handle() as *mut c_void);
    let _ = SetStdHandle(STD_OUTPUT_HANDLE, stdout_handle);
    let _ = SetStdHandle(STD_ERROR_HANDLE, stderr_handle);

    let redirect = CrtRedirect::install(stdout_handle, stderr_handle);
    std::mem::forget(redirect);

    spawn_pipe_reader(stdout_read, None, "stdout");
    spawn_pipe_reader(stderr_read, None, "stderr");
    {
        let mut kept = kept_write_files()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        kept.push(stdout_write);
        kept.push(stderr_write);
    }

    logging::scoped_info_message(
        "stdio-capture",
        "process stdio capture installed | transport=anonymous-memory-pipe | disk_files=none | supports=Win32+ucrt+msvcrt stdout/stderr",
    );
}

pub fn flush_pending() {
    if !logging::is_ready() {
        return;
    }
    let captured = {
        let mut pending = pending_lines()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        std::mem::take(&mut *pending)
    };
    for line in captured {
        emit_line(line.identity, &line.stream, &line.line);
    }
}

fn create_memory_pipe() -> Option<(File, File)> {
    unsafe {
        let mut read = HANDLE::default();
        let mut write = HANDLE::default();
        if CreatePipe(&mut read, &mut write, std::ptr::null(), 0) == 0 {
            return None;
        }
        let _ = SetHandleInformation(read, HANDLE_FLAG_INHERIT, 0);
        let read_file = File::from_raw_handle(read.0 as _);
        let write_file = File::from_raw_handle(write.0 as _);
        Some((read_file, write_file))
    }
}

fn spawn_pipe_reader(mut file: File, identity: Option<ModIdentity>, stream: &'static str) {
    let _ = thread::Builder::new()
        .name(format!("bloader-stdio-pipe-{stream}"))
        .spawn(move || {
            let mut pending = Vec::<u8>::new();
            let mut buffer = [0u8; 8192];
            loop {
                match file.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        pending.extend_from_slice(&buffer[..read]);
                        drain_lines(&mut pending, identity.clone(), stream);
                        if pending.len() > 64 * 1024 {
                            let line = String::from_utf8_lossy(&pending)
                                .trim_matches(char::from(0))
                                .trim()
                                .to_string();
                            if !line.is_empty() {
                                queue_or_emit(identity.clone(), stream, &line);
                            }
                            pending.clear();
                        }
                    }
                    Err(_) => break,
                }
            }
            if !pending.is_empty() {
                let line = String::from_utf8_lossy(&pending)
                    .trim_matches(char::from(0))
                    .trim()
                    .to_string();
                if !line.is_empty() {
                    queue_or_emit(identity, stream, &line);
                }
            }
        });
}

fn drain_lines(pending: &mut Vec<u8>, identity: Option<ModIdentity>, stream: &str) {
    let mut consumed = 0usize;
    for (index, value) in pending.iter().enumerate() {
        if *value != b'\n' {
            continue;
        }
        let line = String::from_utf8_lossy(&pending[consumed..index])
            .trim_end_matches('\r')
            .trim_matches(char::from(0))
            .to_string();
        if !line.trim().is_empty() {
            queue_or_emit(identity.clone(), stream, &line);
        }
        consumed = index + 1;
    }
    if consumed > 0 {
        pending.drain(..consumed);
    }
}

fn queue_or_emit(identity: Option<ModIdentity>, stream: &str, line: &str) {
    if logging::is_ready() {
        emit_line(identity, stream, line);
    } else {
        pending_lines()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(CapturedLine {
                identity,
                stream: stream.to_string(),
                line: line.to_string(),
            });
    }
}

fn emit_line(identity: Option<ModIdentity>, stream: &str, line: &str) {
    let owner = identity.or_else(|| mod_diagnostics::resolve_output_owner(line));
    match owner {
        Some(identity) => logging::captured_mod_output(&identity.name, &identity.id, stream, line),
        None => logging::captured_process_output(stream, line),
    }
}

impl ActiveCaptureGuard {
    fn push(identity: &ModIdentity, phase: &str) -> Self {
        let thread_id = unsafe { GetCurrentThreadId() };
        active_captures()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(ActiveCaptureSnapshot {
                thread_id,
                identity: identity.clone(),
                phase: phase.to_string(),
                stdout_path: PathBuf::from("<memory-pipe>"),
                stderr_path: PathBuf::from("<memory-pipe>"),
            });
        Self {
            thread_id,
            identity_id: identity.id.clone(),
            phase: phase.to_string(),
        }
    }
}

impl Drop for ActiveCaptureGuard {
    fn drop(&mut self) {
        let mut captures = active_captures()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = captures.iter().rposition(|capture| {
            capture.thread_id == self.thread_id
                && capture.identity.id == self.identity_id
                && capture.phase == self.phase
        }) {
            captures.remove(index);
        }
    }
}

pub fn active_capture_for_thread(thread_id: u32) -> Option<ActiveCaptureSnapshot> {
    let captures = active_captures().try_lock().ok()?;
    captures
        .iter()
        .rev()
        .find(|capture| capture.thread_id == thread_id)
        .cloned()
        .or_else(|| (captures.len() == 1).then(|| captures[0].clone()))
}

pub fn active_capture_output(thread_id: u32, _max_bytes: usize) -> String {
    let Some(capture) = active_capture_for_thread(thread_id) else {
        return "<none>".to_string();
    };
    format!(
        "mod={} ({}) phase={} transport=memory-pipe persisted=false",
        capture.identity.name, capture.identity.id, capture.phase
    )
}

impl CrtRedirect {
    unsafe fn install(stdout: HANDLE, stderr: HANDLE) -> Self {
        let mut bindings = Vec::new();
        for (module_name, api) in resolve_crt_apis() {
            let saved_stdout = (api.dup)(STDOUT_FILENO);
            let saved_stderr = (api.dup)(STDERR_FILENO);
            bind_crt_fd(api, stdout, STDOUT_FILENO);
            bind_crt_fd(api, stderr, STDERR_FILENO);
            configure_unbuffered_stdio(api);
            let _ = (api.flushall)();
            bindings.push(CrtBinding {
                _module_name: module_name,
                api,
                _saved_stdout: saved_stdout,
                _saved_stderr: saved_stderr,
            });
        }
        Self {
            _bindings: bindings,
        }
    }
}

unsafe fn configure_unbuffered_stdio(api: CrtApi) {
    const IONBF: i32 = 0x0004;
    let (Some(iob_func), Some(setvbuf)) = (api.iob_func, api.setvbuf) else {
        return;
    };
    for index in [1u32, 2u32] {
        let stream = iob_func(index);
        if !stream.is_null() {
            let _ = setvbuf(stream, std::ptr::null_mut(), IONBF, 0);
        }
    }
}

unsafe fn bind_crt_fd(api: CrtApi, source: HANDLE, target_fd: i32) {
    let Some(duplicated) = duplicate_handle(source) else {
        return;
    };
    let fd = (api.open_osfhandle)(duplicated.0 as isize, O_TEXT);
    if fd < 0 {
        let _ = CloseHandle(duplicated);
        return;
    }
    let _ = (api.dup2)(fd, target_fd);
    let _ = (api.close)(fd);
}

unsafe fn duplicate_handle(source: HANDLE) -> Option<HANDLE> {
    if source.is_invalid() {
        return None;
    }
    let process = GetCurrentProcess();
    let mut duplicated = HANDLE::default();
    DuplicateHandle(
        process,
        source,
        process,
        &mut duplicated,
        0,
        false,
        DUPLICATE_SAME_ACCESS,
    )
    .ok()?;
    Some(duplicated)
}

unsafe fn resolve_crt_apis() -> Vec<(String, CrtApi)> {
    let mut result = Vec::new();
    for module_name in ["ucrtbase.dll", "msvcrt.dll"] {
        let wide: Vec<u16> = module_name.encode_utf16().chain(Some(0)).collect();
        let Ok(module) = GetModuleHandleW(PCWSTR(wide.as_ptr())) else {
            continue;
        };
        let Some(dup) = proc(module, b"_dup\0") else {
            continue;
        };
        let Some(dup2) = proc(module, b"_dup2\0") else {
            continue;
        };
        let Some(close) = proc(module, b"_close\0") else {
            continue;
        };
        let Some(open_osfhandle) = proc(module, b"_open_osfhandle\0") else {
            continue;
        };
        let Some(flushall) = proc(module, b"_flushall\0") else {
            continue;
        };
        let iob_func = proc(module, b"__acrt_iob_func\0").map(|value| std::mem::transmute(value));
        let setvbuf = proc(module, b"setvbuf\0").map(|value| std::mem::transmute(value));
        result.push((
            module_name.to_string(),
            CrtApi {
                dup: std::mem::transmute(dup),
                dup2: std::mem::transmute(dup2),
                close: std::mem::transmute(close),
                open_osfhandle: std::mem::transmute(open_osfhandle),
                flushall: std::mem::transmute(flushall),
                iob_func,
                setvbuf,
            },
        ));
    }
    result
}

unsafe fn proc(
    module: windows::Win32::Foundation::HMODULE,
    name: &'static [u8],
) -> Option<unsafe extern "system" fn() -> isize> {
    GetProcAddress(module, PCSTR(name.as_ptr()))
}

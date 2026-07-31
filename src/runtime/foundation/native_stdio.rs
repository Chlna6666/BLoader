use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThreadId,
};
use windows::core::{PCSTR, PCWSTR};

use crate::runtime::foundation::{logging, mod_diagnostics};
use mod_diagnostics::ModIdentity;

const O_TEXT: i32 = 0x4000;
const STDOUT_FILENO: i32 = 1;
const STDERR_FILENO: i32 = 2;

#[derive(Clone)]
struct CapturedLine {
    identity: Option<ModIdentity>,
    stream: String,
    line: String,
}

struct KeptCaptureFiles {
    _stdout: File,
    _stderr: File,
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
    module_name: String,
    api: CrtApi,
    saved_stdout: i32,
    saved_stderr: i32,
}

struct CrtRedirect {
    bindings: Vec<CrtBinding>,
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

static PENDING_LINES: OnceLock<Mutex<Vec<CapturedLine>>> = OnceLock::new();
static KEPT_FILES: OnceLock<Mutex<Vec<KeptCaptureFiles>>> = OnceLock::new();
static PROCESS_CAPTURE_INSTALLED: OnceLock<()> = OnceLock::new();
static ACTIVE_CAPTURES: OnceLock<Mutex<Vec<ActiveCaptureSnapshot>>> = OnceLock::new();

fn pending_lines() -> &'static Mutex<Vec<CapturedLine>> {
    PENDING_LINES.get_or_init(|| Mutex::new(Vec::new()))
}

fn kept_files() -> &'static Mutex<Vec<KeptCaptureFiles>> {
    KEPT_FILES.get_or_init(|| Mutex::new(Vec::new()))
}

fn active_captures() -> &'static Mutex<Vec<ActiveCaptureSnapshot>> {
    ACTIVE_CAPTURES.get_or_init(|| Mutex::new(Vec::new()))
}

pub unsafe fn capture_library_load<T>(identity: &ModIdentity, phase: &str, f: impl FnOnce() -> T) -> T {
    let capture_dir = PathBuf::from("logs").join("mod-output");
    let _ = fs::create_dir_all(&capture_dir);
    let base = format!("{}-{}", sanitize(&identity.id), sanitize(phase));
    let stdout_path = capture_dir.join(format!("{base}-stdout.log"));
    let stderr_path = capture_dir.join(format!("{base}-stderr.log"));

    let stdout_file = open_capture_file(&stdout_path);
    let stderr_file = open_capture_file(&stderr_path);
    let (Some(stdout_file), Some(stderr_file)) = (stdout_file, stderr_file) else {
        queue_or_emit(
            Some(identity.clone()),
            "capture",
            &format!("failed to create native stdio capture files for phase={phase}"),
        );
        return f();
    };

    let stdout_offset = stdout_file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let stderr_offset = stderr_file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let original_stdout = GetStdHandle(STD_OUTPUT_HANDLE).unwrap_or_default();
    let original_stderr = GetStdHandle(STD_ERROR_HANDLE).unwrap_or_default();
    let stdout_handle = HANDLE(stdout_file.as_raw_handle() as *mut c_void);
    let stderr_handle = HANDLE(stderr_file.as_raw_handle() as *mut c_void);

    let _ = SetStdHandle(STD_OUTPUT_HANDLE, stdout_handle);
    let _ = SetStdHandle(STD_ERROR_HANDLE, stderr_handle);
    let redirect = CrtRedirect::install(stdout_handle, stderr_handle);

    mod_diagnostics::record_lifecycle(
        identity,
        "stdio_capture_begin",
        &format!("phase={phase} stdout={} stderr={}", stdout_path.display(), stderr_path.display()),
    );
    let _active_capture = ActiveCaptureGuard::push(
        identity,
        phase,
        stdout_path.clone(),
        stderr_path.clone(),
    );
    let result = f();
    redirect.flush();
    redirect.restore();
    let _ = SetStdHandle(STD_OUTPUT_HANDLE, original_stdout);
    let _ = SetStdHandle(STD_ERROR_HANDLE, original_stderr);

    spawn_tail_once(stdout_path, stdout_offset, Some(identity.clone()), "stdout");
    spawn_tail_once(stderr_path, stderr_offset, Some(identity.clone()), "stderr");
    kept_files()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(KeptCaptureFiles {
            _stdout: stdout_file,
            _stderr: stderr_file,
        });
    mod_diagnostics::record_lifecycle(identity, "stdio_capture_ready", phase);
    result
}

pub unsafe fn install_process_capture() {
    if PROCESS_CAPTURE_INSTALLED.set(()).is_err() {
        return;
    }

    let capture_dir = PathBuf::from("logs").join("captured-stdio");
    let _ = fs::create_dir_all(&capture_dir);
    let stdout_path = capture_dir.join("process-stdout.raw.log");
    let stderr_path = capture_dir.join("process-stderr.raw.log");
    let Some(stdout_file) = open_capture_file_truncated(&stdout_path) else {
        logging::scoped_error_message("stdio-capture", "failed to open process stdout capture");
        return;
    };
    let Some(stderr_file) = open_capture_file_truncated(&stderr_path) else {
        logging::scoped_error_message("stdio-capture", "failed to open process stderr capture");
        return;
    };

    let stdout_handle = HANDLE(stdout_file.as_raw_handle() as *mut c_void);
    let stderr_handle = HANDLE(stderr_file.as_raw_handle() as *mut c_void);
    let _ = SetStdHandle(STD_OUTPUT_HANDLE, stdout_handle);
    let _ = SetStdHandle(STD_ERROR_HANDLE, stderr_handle);
    let redirect = CrtRedirect::install(stdout_handle, stderr_handle);
    std::mem::forget(redirect);

    spawn_tail_follow(stdout_path.clone(), 0, None, "stdout");
    spawn_tail_follow(stderr_path.clone(), 0, None, "stderr");
    kept_files()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(KeptCaptureFiles {
            _stdout: stdout_file,
            _stderr: stderr_file,
        });
    logging::scoped_info_message(
        "stdio-capture",
        &format!(
            "process stdio capture installed | stdout={} | stderr={} | supports=puts,printf,fputs,fprintf,std::cout,Rust-print,Win32-stdout",
            stdout_path.display(),
            stderr_path.display()
        ),
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

fn spawn_tail_once(
    path: PathBuf,
    start_offset: u64,
    identity: Option<ModIdentity>,
    stream: &'static str,
) {
    let thread_name = format!(
        "bloader-stdio-once-{}-{}",
        stream,
        identity
            .as_ref()
            .map(|value| sanitize(&value.id))
            .unwrap_or_else(|| "process".to_string())
    );
    let _ = thread::Builder::new().name(thread_name).spawn(move || {
        read_available(&path, start_offset, identity, stream);
    });
}

fn spawn_tail_follow(
    path: PathBuf,
    start_offset: u64,
    identity: Option<ModIdentity>,
    stream: &'static str,
) {
    let thread_name = format!(
        "bloader-stdio-follow-{}-{}",
        stream,
        identity
            .as_ref()
            .map(|value| sanitize(&value.id))
            .unwrap_or_else(|| "process".to_string())
    );
    let _ = thread::Builder::new().name(thread_name).spawn(move || {
        tail_file(&path, start_offset, identity, stream);
    });
}

fn read_available(path: &Path, mut offset: u64, identity: Option<ModIdentity>, stream: &str) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let _ = file.seek(SeekFrom::Start(offset));
    let mut pending = Vec::<u8>::new();
    let mut buffer = [0u8; 8192];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                offset += read as u64;
                pending.extend_from_slice(&buffer[..read]);
                drain_lines(&mut pending, identity.clone(), stream);
            }
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        let line = String::from_utf8_lossy(&pending)
            .trim_end_matches(|value| matches!(value, '\r' | '\n' | '\0'))
            .to_string();
        if !line.trim().is_empty() {
            queue_or_emit(identity, stream, &line);
        }
    }
    let _ = offset;
}

fn tail_file(path: &Path, mut offset: u64, identity: Option<ModIdentity>, stream: &str) {
    let mut pending = Vec::<u8>::new();
    loop {
        match File::open(path) {
            Ok(mut file) => {
                let _ = file.seek(SeekFrom::Start(offset));
                let mut buffer = [0u8; 8192];
                loop {
                    match file.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            offset += read as u64;
                            pending.extend_from_slice(&buffer[..read]);
                            drain_lines(&mut pending, identity.clone(), stream);
                        }
                        Err(_) => break,
                    }
                }
            }
            Err(_) => {}
        }

        if pending.len() > 64 * 1024 {
            let line = String::from_utf8_lossy(&pending).trim().to_string();
            if !line.is_empty() {
                queue_or_emit(identity.clone(), stream, &line);
            }
            pending.clear();
        }
        thread::sleep(Duration::from_millis(50));
    }
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

fn open_capture_file(path: &Path) -> Option<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .ok()
}

fn open_capture_file_truncated(path: &Path) -> Option<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(true)
        .open(path)
        .ok()
}

impl ActiveCaptureGuard {
    fn push(
        identity: &ModIdentity,
        phase: &str,
        stdout_path: PathBuf,
        stderr_path: PathBuf,
    ) -> Self {
        let thread_id = unsafe { GetCurrentThreadId() };
        active_captures()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(ActiveCaptureSnapshot {
                thread_id,
                identity: identity.clone(),
                phase: phase.to_string(),
                stdout_path,
                stderr_path,
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

pub fn active_capture_output(thread_id: u32, max_bytes: usize) -> String {
    let Some(capture) = active_capture_for_thread(thread_id) else {
        return "<none>".to_string();
    };
    format!(
        "mod={} ({}) phase={}\r\nstdout={}\r\n{}\r\nstderr={}\r\n{}",
        capture.identity.name,
        capture.identity.id,
        capture.phase,
        capture.stdout_path.display(),
        read_tail(&capture.stdout_path, max_bytes),
        capture.stderr_path.display(),
        read_tail(&capture.stderr_path, max_bytes),
    )
}

fn read_tail(path: &Path, max_bytes: usize) -> String {
    let Ok(mut file) = File::open(path) else {
        return "<unavailable>".to_string();
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes as u64);
    let _ = file.seek(SeekFrom::Start(start));
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).replace('\0', "");
    if text.trim().is_empty() {
        "<empty-or-buffered>".to_string()
    } else {
        text
    }
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
                module_name,
                api,
                saved_stdout,
                saved_stderr,
            });
        }
        if bindings.is_empty() {
            logging::write_bootstrap_marker(
                "stdio-capture.crt.none supports=Win32-stdout only; static CRT puts may remain unavailable",
            );
        } else {
            logging::write_bootstrap_marker(&format!(
                "stdio-capture.crt.bound modules={}",
                bindings
                    .iter()
                    .map(|binding| binding.module_name.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        Self { bindings }
    }

    unsafe fn flush(&self) {
        for binding in &self.bindings {
            let _ = (binding.api.flushall)();
        }
    }

    unsafe fn restore(self) {
        for binding in self.bindings.into_iter().rev() {
            let _ = (binding.api.flushall)();
            if binding.saved_stdout >= 0 {
                let _ = (binding.api.dup2)(binding.saved_stdout, STDOUT_FILENO);
                let _ = (binding.api.close)(binding.saved_stdout);
            }
            if binding.saved_stderr >= 0 {
                let _ = (binding.api.dup2)(binding.saved_stderr, STDERR_FILENO);
                let _ = (binding.api.close)(binding.saved_stderr);
            }
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
        let Some(dup) = proc(module, b"_dup\0") else { continue; };
        let Some(dup2) = proc(module, b"_dup2\0") else { continue; };
        let Some(close) = proc(module, b"_close\0") else { continue; };
        let Some(open_osfhandle) = proc(module, b"_open_osfhandle\0") else { continue; };
        let Some(flushall) = proc(module, b"_flushall\0") else { continue; };
        let iob_func = proc(module, b"__acrt_iob_func\0")
            .map(|value| std::mem::transmute(value));
        let setvbuf = proc(module, b"setvbuf\0")
            .map(|value| std::mem::transmute(value));
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

fn sanitize(value: &str) -> String {
    let mut result = value
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.') {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    if result.is_empty() {
        result.push_str("unknown-mod");
    }
    result
}

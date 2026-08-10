use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::slice;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use chrono::Local;
use serde::Serialize;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{
    ADDRESS64, AddVectoredExceptionHandler, AddrModeFlat, EXCEPTION_CONTINUE_SEARCH,
    EXCEPTION_EXECUTE_HANDLER, EXCEPTION_POINTERS, IMAGEHLP_LINEW64, KDHELP64,
    MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE, MiniDumpNormal,
    MiniDumpWithIndirectlyReferencedMemory, MiniDumpWithThreadInfo, MiniDumpWriteDump,
    RtlCaptureStackBackTrace, STACKFRAME64, SYMBOL_INFO_PACKAGEW, SYMOPT_DEFERRED_LOADS,
    SYMOPT_LOAD_LINES, SYMOPT_UNDNAME, SetUnhandledExceptionFilter, StackWalk64, SymFromAddrW,
    SymFunctionTableAccess64, SymGetLineFromAddrW64, SymGetModuleBase64, SymInitializeW,
    SymSetOptions,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_AMD64;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::core::PCWSTR;

use crate::runtime::foundation::{
    build_info, error_dialog, file_io_policy, logging, mod_diagnostics, native_stdio,
};

static CRASH_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);
static CRASH_DIALOG_SHOWN: AtomicBool = AtomicBool::new(false);
static CAPTURE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static LAST_REPORT: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
static LAST_EXCEPTION_SIGNATURE: AtomicU64 = AtomicU64::new(0);
static SYMBOLS_READY: OnceLock<bool> = OnceLock::new();
static EXTERNAL_LOGGER_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Serialize)]
struct CrashAttribution {
    owner_type: String,
    mod_id: Option<String>,
    mod_name: Option<String>,
    mod_version: Option<String>,
    mod_kind: Option<String>,
    mod_path: Option<String>,
    active_phase: Option<String>,
    confidence: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct CrashJsonReport {
    timestamp: String,
    loader_version: String,
    phase: String,
    process_id: u32,
    thread_id: u32,
    exception_code: String,
    exception_flags: String,
    exception_address: String,
    exception_module: String,
    exception_symbol: String,
    exception_parameters: String,
    attribution: CrashAttribution,
    active_context: String,
    active_stdio: String,
    mod_inventory: String,
    recent_mod_events: String,
    registers: String,
    stack: String,
    text_report: String,
    minidump: String,
}

struct CaptureGuard;

impl CaptureGuard {
    fn enter() -> Option<Self> {
        CAPTURE_IN_PROGRESS
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURE_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

type InvalidParameterHandler = unsafe extern "C" fn(*const u16, *const u16, *const u16, u32, usize);
type PurecallHandler = unsafe extern "C" fn();

unsafe extern "C" {
    fn _set_invalid_parameter_handler(
        handler: Option<InvalidParameterHandler>,
    ) -> Option<InvalidParameterHandler>;
    fn _set_purecall_handler(handler: Option<PurecallHandler>) -> Option<PurecallHandler>;
}

unsafe extern "system" fn stackwalk_function_table_access(
    process: windows::Win32::Foundation::HANDLE,
    address: u64,
) -> *mut core::ffi::c_void {
    SymFunctionTableAccess64(process, address)
}

unsafe extern "system" fn stackwalk_get_module_base(
    process: windows::Win32::Foundation::HANDLE,
    address: u64,
) -> u64 {
    SymGetModuleBase64(process, address)
}

pub fn install_early() {
    install_handlers("dllmain-before-native-preload");
}

pub fn install() {
    install_handlers("bootstrap");
}

fn install_handlers(source: &str) {
    if CRASH_HANDLER_INSTALLED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        unsafe {
            let _ = AddVectoredExceptionHandler(1, Some(vectored_exception_handler));
            let _ = _set_invalid_parameter_handler(Some(invalid_parameter_handler));
            let _ = _set_purecall_handler(Some(purecall_handler));
        }
        logging::write_bootstrap_marker(&format!(
            "crash_report.handlers.installed source={source} version={}",
            build_info::VERSION
        ));
    }
    rearm_unhandled_filter(source);
}

pub fn rearm_unhandled_filter(reason: &str) {
    unsafe {
        let _ = SetUnhandledExceptionFilter(Some(top_level_exception_filter));
    }
    logging::write_bootstrap_marker(&format!(
        "crash_report.unhandled_filter.armed reason={reason}"
    ));
}

pub fn spawn_external_logger(module_handle: usize) {
    if file_io_policy::legacy_uwp_no_write() {
        logging::write_bootstrap_marker(
            "crash_report.external_logger.skipped reason=legacy-uwp-no-file-write",
        );
        return;
    }

    if EXTERNAL_LOGGER_STARTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let Some(logger_path) = external_logger_path(module_handle) else {
        logging::write_bootstrap_marker("crash_report.external_logger.missing");
        return;
    };

    let out_dir = prepare_crash_dir();
    let pid = unsafe { GetCurrentProcessId() };
    match Command::new(&logger_path)
        .arg("--pid")
        .arg(pid.to_string())
        .arg("--out")
        .arg(out_dir.as_os_str())
        .spawn()
    {
        Ok(_) => logging::write_bootstrap_marker(&format!(
            "crash_report.external_logger.started pid={} path={}",
            pid,
            logger_path.display()
        )),
        Err(error) => {
            EXTERNAL_LOGGER_STARTED.store(false, Ordering::SeqCst);
            logging::warn_message(&format!(
                "Failed to start external crash logger: {} ({})",
                logger_path.display(),
                error
            ));
        }
    }
}

unsafe extern "system" fn vectored_exception_handler(exception: *mut EXCEPTION_POINTERS) -> i32 {
    if should_capture_veh_exception(exception) {
        capture_exception(exception, "veh", false);
    }
    EXCEPTION_CONTINUE_SEARCH
}

unsafe extern "system" fn top_level_exception_filter(exception: *const EXCEPTION_POINTERS) -> i32 {
    capture_exception(exception, "seh", true);
    EXCEPTION_EXECUTE_HANDLER
}

unsafe extern "C" fn invalid_parameter_handler(
    expression: *const u16,
    function: *const u16,
    file: *const u16,
    line: u32,
    _reserved: usize,
) {
    let details = format!(
        "expression={}\r\nfunction={}\r\nfile={}\r\nline={}",
        wide_ptr_to_string(expression),
        wide_ptr_to_string(function),
        wide_ptr_to_string(file),
        line
    );
    capture_manual_failure("invalid-parameter", "CRT invalid parameter", &details, true);
}

unsafe extern "C" fn purecall_handler() {
    capture_manual_failure("purecall", "CRT pure virtual function call", "", true);
}

unsafe fn capture_exception(exception: *const EXCEPTION_POINTERS, phase: &str, show_dialog: bool) {
    let Some(_capture_guard) = CaptureGuard::enter() else {
        return;
    };

    if file_io_policy::legacy_uwp_no_write() {
        let code = exception_code(exception);
        let address = exception_address(exception);
        let thread_id = GetCurrentThreadId();
        let module = module_path_from_address(address);
        logging::emergency_error_message(
            "loader",
            &format!(
                "CRASH_CAPTURED_NO_FILE | phase={phase} | code=0x{code:08X} | address=0x{address:X} | module={module} | thread={thread_id} | file_io=disabled"
            ),
        );
        if show_dialog
            && CRASH_DIALOG_SHOWN
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            error_dialog::show_fatal_error(
                "BLoader Mod Crash",
                "A crash was captured, but file crash reports are disabled for Minecraft 1.17/1.18/1.19 legacy UWP hosts.",
            );
        }
        return;
    }

    let report_dir = prepare_crash_dir();
    let timestamp = Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
    let exception_code = exception_code(exception);
    let exception_flags = exception_flags(exception);
    let exception_address = exception_address(exception);
    let thread_id = GetCurrentThreadId();
    let signature = crash_signature(exception_code, exception_address, thread_id);
    if phase == "seh" && LAST_EXCEPTION_SIGNATURE.load(Ordering::SeqCst) == signature {
        if show_dialog
            && CRASH_DIALOG_SHOWN
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            let last_report = LAST_REPORT
                .get_or_init(|| Mutex::new(None))
                .try_lock()
                .ok()
                .and_then(|value| (*value).clone())
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<report-path-unavailable>".to_string());
            error_dialog::show_fatal_error(
                "BLoader Mod Crash",
                &format!(
                    "The exception was captured by the early vectored handler.\n\nReport: {last_report}"
                ),
            );
        }
        return;
    }
    let exception_module = module_path_from_address(exception_address);
    let loader_lock_sensitive = mod_diagnostics::active_scope_for_thread(thread_id).is_some();
    let exception_symbol = if loader_lock_sensitive {
        raw_address_description(exception_address)
    } else {
        symbolize_address(exception_address)
    };
    let attribution = determine_attribution(thread_id, exception_address, &exception_module);
    let owner_slug = attribution
        .mod_id
        .as_deref()
        .map(sanitize_file_component)
        .unwrap_or_else(|| "unresolved".to_string());
    let report_path = report_dir.join(format!("crash-{phase}-{owner_slug}-{timestamp}.txt"));
    let json_path = report_dir.join(format!("crash-{phase}-{owner_slug}-{timestamp}.json"));
    let dump_path = report_dir.join(format!("crash-{phase}-{owner_slug}-{timestamp}.dmp"));

    let register_dump = register_dump(exception);
    let stack_trace = if loader_lock_sensitive {
        loader_safe_exception_stack(exception)
    } else {
        exception_stack_trace(exception)
    };
    let parameters = exception_parameters(exception);
    let active_context = mod_diagnostics::active_context_text();
    let active_stdio = native_stdio::active_capture_output(thread_id, 128 * 1024);
    let mod_inventory = mod_diagnostics::inventory_text();
    let recent_mod_events = mod_diagnostics::recent_events_text(96);
    let owner_text = attribution_text(&attribution);
    let message = format!(
        "Unhandled exception 0x{exception_code:08X} at 0x{exception_address:X}\nowner={}\nphase={phase}\nreport={}\ndump={}",
        attribution
            .mod_name
            .as_deref()
            .unwrap_or("unresolved"),
        report_path.display(),
        dump_path.display()
    );

    let text = format!(
        "timestamp={}\r\nloader_version={}\r\nloader_profile={}\r\nphase={}\r\nprocess_id={}\r\nthread_id={}\r\nexception_code=0x{exception_code:08X}\r\nexception_flags=0x{exception_flags:08X}\r\nexception_address=0x{exception_address:X}\r\nexception_module={}\r\nexception_symbol={}\r\nreport={}\r\njson={}\r\ndump={}\r\n\r\n[attribution]\r\n{}\r\n\r\n[exception_parameters]\r\n{}\r\n\r\n[active_mod_scopes]\r\n{}\r\n\r\n[active_preload_stdio]\r\n{}\r\n\r\n[mod_inventory]\r\n{}\r\n\r\n[recent_mod_lifecycle]\r\n{}\r\n\r\n[registers]\r\n{}\r\n\r\n[stack]\r\n{}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        build_info::VERSION,
        build_info::PROFILE,
        phase,
        GetCurrentProcessId(),
        thread_id,
        exception_module,
        exception_symbol,
        report_path.display(),
        json_path.display(),
        dump_path.display(),
        owner_text,
        parameters,
        active_context,
        active_stdio,
        mod_inventory,
        recent_mod_events,
        register_dump,
        stack_trace,
    );
    write_report(&report_path, &text);

    let json_report = CrashJsonReport {
        timestamp: Local::now().to_rfc3339(),
        loader_version: build_info::VERSION.to_string(),
        phase: phase.to_string(),
        process_id: GetCurrentProcessId(),
        thread_id,
        exception_code: format!("0x{exception_code:08X}"),
        exception_flags: format!("0x{exception_flags:08X}"),
        exception_address: format!("0x{exception_address:X}"),
        exception_module: exception_module.clone(),
        exception_symbol: exception_symbol.clone(),
        exception_parameters: parameters.clone(),
        attribution: attribution.clone(),
        active_context: active_context.clone(),
        active_stdio: active_stdio.clone(),
        mod_inventory: mod_inventory.clone(),
        recent_mod_events: recent_mod_events.clone(),
        registers: register_dump.clone(),
        stack: stack_trace.clone(),
        text_report: report_path.display().to_string(),
        minidump: dump_path.display().to_string(),
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&json_report) {
        let _ = fs::write(&json_path, bytes);
    }

    let dump_result = write_minidump(&dump_path, exception);
    LAST_EXCEPTION_SIGNATURE.store(signature, Ordering::SeqCst);
    if let Ok(mut last_report) = LAST_REPORT.get_or_init(|| Mutex::new(None)).try_lock() {
        *last_report = Some(report_path.clone());
    }

    if let Some(identity) = attribution_identity(&attribution) {
        mod_diagnostics::mark_crashed(
            &identity,
            phase,
            &format!(
                "code=0x{exception_code:08X} address=0x{exception_address:X} module={exception_module} report={}",
                report_path.display()
            ),
        );
    }

    let scope = attribution
        .mod_name
        .as_deref()
        .map(|name| format!("mod:{name}"))
        .unwrap_or_else(|| "loader".to_string());
    logging::emergency_error_message(
        &scope,
        &format!(
            "CRASH_CAPTURED | owner={} | confidence={} | reason={} | phase={phase} | code=0x{exception_code:08X} | address=0x{exception_address:X} | module={exception_module} | thread={thread_id} | report={} | dump={} | dump_ok={}",
            attribution.mod_name.as_deref().unwrap_or("unresolved"),
            attribution.confidence,
            attribution.reason,
            report_path.display(),
            dump_path.display(),
            dump_result.is_ok(),
        ),
    );
    logging::write_bootstrap_marker(&format!(
        "crash_report.captured phase={phase} owner={} code=0x{exception_code:08X} address=0x{exception_address:X} report={}",
        attribution.mod_name.as_deref().unwrap_or("unresolved"),
        report_path.display(),
    ));

    if show_dialog
        && CRASH_DIALOG_SHOWN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        error_dialog::show_fatal_error("BLoader Mod Crash", &message);
    }
}

unsafe fn should_capture_veh_exception(exception: *const EXCEPTION_POINTERS) -> bool {
    if exception.is_null() || (*exception).ExceptionRecord.is_null() {
        return false;
    }

    let record = &*(*exception).ExceptionRecord;
    let code = record.ExceptionCode.0 as u32;
    let flags = record.ExceptionFlags;
    let address = record.ExceptionAddress as usize;
    let module = module_path_from_address(address).to_ascii_lowercase();
    let active_mod = mod_diagnostics::active_scope_for_thread(GetCurrentThreadId()).is_some();

    if matches!(code, 0xE0000001 | 0xE06D7363) {
        return false;
    }

    let fatal = matches!(
        code,
        0x40000015
            | 0x80000003
            | 0xC0000005
            | 0xC000001D
            | 0xC0000025
            | 0xC0000094
            | 0xC0000096
            | 0xC00000FD
            | 0xC0000409
            | 0xC000041D
            | 0xC0000602
    );

    if !fatal {
        return false;
    }

    if active_mod {
        return true;
    }

    if flags & 0x1 != 0 {
        return is_loader_or_mod_module(&module)
            || mod_diagnostics::identify_address(address).is_some();
    }

    is_loader_or_mod_module(&module) || mod_diagnostics::identify_address(address).is_some()
}

fn crash_signature(code: u32, address: usize, thread_id: u32) -> u64 {
    (address as u64).rotate_left(17) ^ ((code as u64) << 32) ^ thread_id as u64
}

fn determine_attribution(
    thread_id: u32,
    exception_address: usize,
    exception_module: &str,
) -> CrashAttribution {
    if let Some(scope) = mod_diagnostics::active_scope_for_thread(thread_id) {
        return CrashAttribution {
            owner_type: "mod".to_string(),
            mod_id: Some(scope.identity.id.clone()),
            mod_name: Some(scope.identity.name.clone()),
            mod_version: scope.identity.version.clone(),
            mod_kind: Some(scope.identity.kind.clone()),
            mod_path: Some(scope.identity.dll_path.clone()),
            active_phase: Some(scope.phase.clone()),
            confidence: "high".to_string(),
            reason: format!(
                "exception occurred while BLoader was executing the Mod scope on thread {}",
                scope.thread_id
            ),
        };
    }

    if let Some(identity) = mod_diagnostics::identify_address(exception_address) {
        return CrashAttribution {
            owner_type: "mod".to_string(),
            mod_id: Some(identity.id.clone()),
            mod_name: Some(identity.name.clone()),
            mod_version: identity.version.clone(),
            mod_kind: Some(identity.kind.clone()),
            mod_path: Some(identity.dll_path.clone()),
            active_phase: None,
            confidence: "high".to_string(),
            reason: "faulting instruction address belongs to the registered Mod module".to_string(),
        };
    }

    CrashAttribution {
        owner_type: if is_loader_or_mod_module(&exception_module.to_ascii_lowercase()) {
            "loader-or-unregistered-mod".to_string()
        } else {
            "unresolved".to_string()
        },
        mod_id: None,
        mod_name: None,
        mod_version: None,
        mod_kind: None,
        mod_path: None,
        active_phase: None,
        confidence: "low".to_string(),
        reason: "no active Mod scope and the faulting address did not match a registered Mod".to_string(),
    }
}

fn attribution_text(attribution: &CrashAttribution) -> String {
    format!(
        "owner_type={}\r\nmod_id={}\r\nmod_name={}\r\nmod_version={}\r\nmod_kind={}\r\nmod_path={}\r\nactive_phase={}\r\nconfidence={}\r\nreason={}",
        attribution.owner_type,
        attribution.mod_id.as_deref().unwrap_or("none"),
        attribution.mod_name.as_deref().unwrap_or("none"),
        attribution.mod_version.as_deref().unwrap_or("unknown"),
        attribution.mod_kind.as_deref().unwrap_or("unknown"),
        attribution.mod_path.as_deref().unwrap_or("none"),
        attribution.active_phase.as_deref().unwrap_or("none"),
        attribution.confidence,
        attribution.reason,
    )
}

fn attribution_identity(attribution: &CrashAttribution) -> Option<mod_diagnostics::ModIdentity> {
    let id = attribution.mod_id.as_deref()?;
    mod_diagnostics::find_by_name(id)
}

fn sanitize_file_component(value: &str) -> String {
    let result = value
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
        "unknown".to_string()
    } else {
        result
    }
}

fn is_loader_or_mod_module(module_path: &str) -> bool {
    module_path.ends_with("\\bloader.dll")
        || module_path.ends_with("\\bloader.pdb")
        || module_path.contains("\\mods\\")
        || module_path
            .rsplit('\\')
            .next()
            .map(|name| name.starts_with("bl_") && name.ends_with(".dll"))
            .unwrap_or(false)
}

pub fn capture_rust_panic(details: &str, show_dialog: bool) {
    capture_manual_failure("rust-panic", "Rust panic", details, show_dialog);
}

fn capture_manual_failure(phase: &str, title: &str, details: &str, show_dialog: bool) {
    let Some(_capture_guard) = CaptureGuard::enter() else {
        return;
    };

    if file_io_policy::legacy_uwp_no_write() {
        logging::emergency_error_message(
            "loader",
            &format!(
                "MANUAL_CRASH_CAPTURED_NO_FILE | phase={phase} | title={title} | details={} | file_io=disabled",
                if details.is_empty() { "<none>" } else { details }
            ),
        );
        if show_dialog
            && CRASH_DIALOG_SHOWN
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            error_dialog::show_fatal_error(
                "BLoader Mod Crash",
                &format!(
                    "{title}\n\n{details}\n\nFile crash reports are disabled for Minecraft 1.17/1.18/1.19 legacy UWP hosts."
                ),
            );
        }
        return;
    }

    let report_dir = prepare_crash_dir();
    let timestamp = Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
    let thread_id = unsafe { GetCurrentThreadId() };
    let active_scope = mod_diagnostics::active_scope_for_thread(thread_id);
    let owner_slug = active_scope
        .as_ref()
        .map(|scope| sanitize_file_component(&scope.identity.id))
        .unwrap_or_else(|| "unresolved".to_string());
    let report_path = report_dir.join(format!("crash-{phase}-{owner_slug}-{timestamp}.txt"));
    let stack_trace = current_stack_trace();
    let symbol_mapping = "enabled when DbgHelp is safe; minidump is authoritative".to_string();
    let active_context = mod_diagnostics::active_context_text();
    let active_stdio = native_stdio::active_capture_output(thread_id, 128 * 1024);
    let mod_inventory = mod_diagnostics::inventory_text();
    let recent_events = mod_diagnostics::recent_events_text(96);
    let text = format!(
        "timestamp={}\r\nloader_version={}\r\nphase={}\r\ntitle={}\r\nprocess_id={}\r\nthread_id={}\r\nowner={}\r\nreport={}\r\n\r\n[symbol_mapping]\r\n{}\r\n\r\n[active_mod_scopes]\r\n{}\r\n\r\n[active_preload_stdio]\r\n{}\r\n\r\n[mod_inventory]\r\n{}\r\n\r\n[recent_mod_lifecycle]\r\n{}\r\n\r\n[details]\r\n{}\r\n\r\n[stack]\r\n{}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        build_info::VERSION,
        phase,
        title,
        unsafe { GetCurrentProcessId() },
        thread_id,
        active_scope
            .as_ref()
            .map(|scope| format!("{} ({}) phase={}", scope.identity.name, scope.identity.id, scope.phase))
            .unwrap_or_else(|| "unresolved".to_string()),
        report_path.display(),
        symbol_mapping,
        active_context,
        active_stdio,
        mod_inventory,
        recent_events,
        if details.is_empty() { "<none>" } else { details },
        stack_trace
    );
    write_report(&report_path, &text);

    if let Some(scope) = active_scope.as_ref() {
        mod_diagnostics::mark_crashed(
            &scope.identity,
            phase,
            &format!("{title}: {details} report={}", report_path.display()),
        );
    }
    let log_scope = active_scope
        .as_ref()
        .map(|scope| format!("mod:{}", scope.identity.name))
        .unwrap_or_else(|| "loader".to_string());
    logging::emergency_error_message(
        &log_scope,
        &format!(
            "MANUAL_CRASH_CAPTURED | phase={phase} | title={title} | report={}",
            report_path.display()
        ),
    );
    logging::write_bootstrap_marker(&format!(
        "crash_report.manual phase={phase} title={title} report={}",
        report_path.display()
    ));

    if show_dialog
        && CRASH_DIALOG_SHOWN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        let message = format!("{title}\n\nReport: {}\n\n{details}", report_path.display());
        error_dialog::show_fatal_error("BLoader Mod Crash", &message);
    }
}

unsafe fn write_minidump(
    path: &PathBuf,
    exception: *const EXCEPTION_POINTERS,
) -> windows::core::Result<()> {
    if !file_io_policy::writes_allowed() {
        return Err(windows::core::Error::new(
            windows::core::HRESULT(0x80070005u32 as i32),
            "file writes disabled by legacy UWP policy",
        ));
    }

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| {
            windows::core::Error::new(windows::core::HRESULT(0x80004005u32 as i32), e.to_string())
        })?;
    let process = GetCurrentProcess();
    let exception_info = MINIDUMP_EXCEPTION_INFORMATION {
        ThreadId: windows::Win32::System::Threading::GetCurrentThreadId(),
        ExceptionPointers: exception as *mut _,
        ClientPointers: false.into(),
    };
    let dump_type = MINIDUMP_TYPE(
        MiniDumpNormal.0 | MiniDumpWithIndirectlyReferencedMemory.0 | MiniDumpWithThreadInfo.0,
    );
    MiniDumpWriteDump(
        process,
        GetCurrentProcessId(),
        windows::Win32::Foundation::HANDLE(file.as_raw_handle() as *mut _),
        dump_type,
        Some(&exception_info),
        None,
        None,
    )?;
    Ok(())
}

fn prepare_crash_dir() -> PathBuf {
    let dir = PathBuf::from("logs").join("crash-reports");
    if file_io_policy::writes_allowed() {
        let _ = fs::create_dir_all(&dir);
    }
    dir
}

fn write_report(path: &PathBuf, text: &str) {
    if !file_io_policy::writes_allowed() {
        return;
    }

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        let _ = file.write_all(text.as_bytes());
        let _ = file.flush();
    }
}

unsafe fn exception_flags(exception: *const EXCEPTION_POINTERS) -> u32 {
    if exception.is_null() || (*exception).ExceptionRecord.is_null() {
        return 0;
    }
    (*(*exception).ExceptionRecord).ExceptionFlags
}

unsafe fn exception_parameters(exception: *const EXCEPTION_POINTERS) -> String {
    if exception.is_null() || (*exception).ExceptionRecord.is_null() {
        return "<unavailable>".to_string();
    }
    let record = &*(*exception).ExceptionRecord;
    let count = (record.NumberParameters as usize).min(record.ExceptionInformation.len());
    let raw = record.ExceptionInformation[..count]
        .iter()
        .enumerate()
        .map(|(index, value)| format!("param[{index}]=0x{value:X}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    if record.ExceptionCode.0 as u32 == 0xC0000005 && count >= 2 {
        let operation = match record.ExceptionInformation[0] {
            0 => "read",
            1 => "write",
            8 => "execute",
            _ => "unknown",
        };
        format!(
            "access_violation.operation={}\r\naccess_violation.target=0x{:X}\r\n{}",
            operation,
            record.ExceptionInformation[1],
            if raw.is_empty() { "<none>" } else { &raw }
        )
    } else if raw.is_empty() {
        "<none>".to_string()
    } else {
        raw
    }
}

unsafe fn exception_code(exception: *const EXCEPTION_POINTERS) -> u32 {
    if exception.is_null() || (*exception).ExceptionRecord.is_null() {
        return 0;
    }
    (*(*exception).ExceptionRecord).ExceptionCode.0 as u32
}

unsafe fn exception_address(exception: *const EXCEPTION_POINTERS) -> usize {
    if exception.is_null() || (*exception).ExceptionRecord.is_null() {
        return 0;
    }
    (*(*exception).ExceptionRecord).ExceptionAddress as usize
}

fn module_path_from_address(address: usize) -> String {
    if address == 0 {
        return "<unknown>".to_string();
    }

    unsafe {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        if VirtualQuery(
            Some(address as *const _),
            &mut mbi,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        ) == 0
        {
            return "<unknown>".to_string();
        }

        let module = HMODULE(mbi.AllocationBase);
        if module.is_invalid() {
            return "<unknown>".to_string();
        }

        let mut buffer = vec![0u16; 1024];
        let len = GetModuleFileNameW(Some(module), &mut buffer) as usize;
        if len == 0 {
            return format!("0x{:X}", module.0 as usize);
        }

        String::from_utf16_lossy(&buffer[..len])
    }
}

fn raw_address_description(address: usize) -> String {
    if address == 0 {
        return "0x0 <unknown>".to_string();
    }
    unsafe {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        if VirtualQuery(
            Some(address as *const _),
            &mut mbi,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        ) == 0
        {
            return format!("0x{address:X} <VirtualQuery-failed>");
        }
        let base = mbi.AllocationBase as usize;
        let path = module_path_from_address(address);
        format!("0x{address:X} {}+0x{:X}", path, address.saturating_sub(base))
    }
}

fn loader_safe_exception_stack(exception: *const EXCEPTION_POINTERS) -> String {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        if exception.is_null() || (*exception).ContextRecord.is_null() {
            return "<exception-context-unavailable>".to_string();
        }
        let context = &*(*exception).ContextRecord;
        format!(
            "#0 {}\r\n<symbol walking deferred because the exception occurred inside an active native Mod/preload scope; use the generated minidump for full unwind>",
            raw_address_description(context.Rip as usize),
        )
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = exception;
        "<loader-safe-stack unsupported on this architecture>".to_string()
    }
}

fn current_stack_trace() -> String {
    unsafe {
        let mut frames: [*mut core::ffi::c_void; 62] = [std::ptr::null_mut(); 62];
        let captured = RtlCaptureStackBackTrace(0, &mut frames, None);
        if captured == 0 {
            return "<empty>".to_string();
        }

        let mut lines = Vec::with_capacity(captured as usize);
        for (index, frame) in frames.iter().take(captured as usize).enumerate() {
            let address = *frame as usize;
            lines.push(format!("#{} {}", index, symbolize_address(address)));
        }
        lines.join("\r\n")
    }
}

fn exception_stack_trace(exception: *const EXCEPTION_POINTERS) -> String {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        if exception.is_null() || (*exception).ContextRecord.is_null() {
            return current_stack_trace();
        }

        if !ensure_symbols_initialized() {
            return current_stack_trace();
        }

        let process = GetCurrentProcess();
        let thread = windows::Win32::System::Threading::GetCurrentThread();
        let mut context = *(*exception).ContextRecord;
        let mut frame = STACKFRAME64 {
            AddrPC: ADDRESS64 {
                Offset: context.Rip,
                Segment: 0,
                Mode: AddrModeFlat,
            },
            AddrReturn: ADDRESS64::default(),
            AddrFrame: ADDRESS64 {
                Offset: context.Rbp,
                Segment: 0,
                Mode: AddrModeFlat,
            },
            AddrStack: ADDRESS64 {
                Offset: context.Rsp,
                Segment: 0,
                Mode: AddrModeFlat,
            },
            AddrBStore: ADDRESS64::default(),
            FuncTableEntry: std::ptr::null_mut(),
            Params: [0; 4],
            Far: false.into(),
            Virtual: false.into(),
            Reserved: [0; 3],
            KdHelp: KDHELP64::default(),
        };

        let mut lines = Vec::new();
        for index in 0..62 {
            let result = StackWalk64(
                IMAGE_FILE_MACHINE_AMD64.0 as u32,
                process,
                thread,
                &mut frame,
                &mut context as *mut _ as *mut _,
                None,
                Some(stackwalk_function_table_access),
                Some(stackwalk_get_module_base),
                None,
            );

            if !result.as_bool() || frame.AddrPC.Offset == 0 {
                break;
            }

            lines.push(format!(
                "#{} {}",
                index,
                symbolize_address(frame.AddrPC.Offset as usize)
            ));
        }

        if lines.is_empty() {
            current_stack_trace()
        } else {
            lines.join("\r\n")
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = exception;
        current_stack_trace()
    }
}

fn register_dump(exception: *const EXCEPTION_POINTERS) -> String {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        if exception.is_null() || (*exception).ContextRecord.is_null() {
            return "<unavailable>".to_string();
        }

        let ctx = &*(*exception).ContextRecord;
        return format!(
            "rip=0x{:X}\r\nrsp=0x{:X}\r\nrbp=0x{:X}\r\nrax=0x{:X}\r\nrbx=0x{:X}\r\nrcx=0x{:X}\r\nrdx=0x{:X}\r\nrsi=0x{:X}\r\nrdi=0x{:X}\r\nr8=0x{:X}\r\nr9=0x{:X}\r\nr10=0x{:X}\r\nr11=0x{:X}\r\nr12=0x{:X}\r\nr13=0x{:X}\r\nr14=0x{:X}\r\nr15=0x{:X}",
            ctx.Rip,
            ctx.Rsp,
            ctx.Rbp,
            ctx.Rax,
            ctx.Rbx,
            ctx.Rcx,
            ctx.Rdx,
            ctx.Rsi,
            ctx.Rdi,
            ctx.R8,
            ctx.R9,
            ctx.R10,
            ctx.R11,
            ctx.R12,
            ctx.R13,
            ctx.R14,
            ctx.R15,
        );
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = exception;
        "<unsupported-arch>".to_string()
    }
}

fn wide_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return "<null>".to_string();
    }

    unsafe {
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(slice::from_raw_parts(ptr, len))
    }
}

fn external_logger_path(module_handle: usize) -> Option<PathBuf> {
    if module_handle == 0 {
        return None;
    }

    unsafe {
        let mut buffer = vec![0u16; 1024];
        let len = GetModuleFileNameW(Some(HMODULE(module_handle as *mut _)), &mut buffer) as usize;
        if len == 0 {
            return None;
        }
        let module_path = PathBuf::from(String::from_utf16_lossy(&buffer[..len]));
        let module_dir = module_path.parent()?;
        let logger_path = module_dir.join("BLoaderCrashLogger.exe");
        logger_path.exists().then_some(logger_path)
    }
}

fn symbolize_address(address: usize) -> String {
    let module = module_path_from_address(address);
    let mut line = format!("0x{:X} {}", address, module);

    if address == 0 || !ensure_symbols_initialized() {
        return line;
    }

    if let Some(symbol) = symbol_name_from_address(address) {
        line.push_str(&format!(" | {symbol}"));
    }

    if let Some(source) = source_line_from_address(address) {
        line.push_str(&format!(" | {source}"));
    }

    line
}

fn ensure_symbols_initialized() -> bool {
    *SYMBOLS_READY.get_or_init(|| unsafe {
        let process = GetCurrentProcess();
        let _ = SymSetOptions(SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES | SYMOPT_UNDNAME);
        SymInitializeW(process, PCWSTR::null(), true).is_ok()
    })
}

fn symbol_name_from_address(address: usize) -> Option<String> {
    unsafe {
        let process = GetCurrentProcess();
        let mut displacement = 0u64;
        let mut package = SYMBOL_INFO_PACKAGEW::default();
        package.si.SizeOfStruct = std::mem::size_of_val(&package.si) as u32;
        package.si.MaxNameLen = package.name.len() as u32;

        SymFromAddrW(
            process,
            address as u64,
            Some(&mut displacement),
            &mut package.si,
        )
        .ok()?;

        let name_len = package.si.NameLen as usize;
        let name = String::from_utf16_lossy(&package.name[..name_len.min(package.name.len())]);
        Some(format!("{name}+0x{displacement:X}"))
    }
}

fn source_line_from_address(address: usize) -> Option<String> {
    unsafe {
        let process = GetCurrentProcess();
        let mut displacement = 0u32;
        let mut line = IMAGEHLP_LINEW64::default();
        line.SizeOfStruct = std::mem::size_of::<IMAGEHLP_LINEW64>() as u32;

        SymGetLineFromAddrW64(process, address as u64, &mut displacement, &mut line).ok()?;

        let file = pwstr_to_string(line.FileName);
        Some(format!(
            "{}:{} +0x{:X}",
            file, line.LineNumber, displacement
        ))
    }
}

fn pwstr_to_string(ptr: windows::core::PWSTR) -> String {
    let raw = ptr.0;
    if raw.is_null() {
        return "<unknown>".to_string();
    }

    unsafe {
        let mut len = 0usize;
        while *raw.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(slice::from_raw_parts(raw, len))
    }
}

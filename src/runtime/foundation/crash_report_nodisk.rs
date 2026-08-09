use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_EXECUTE_HANDLER,
    EXCEPTION_POINTERS, SetUnhandledExceptionFilter,
};

use crate::runtime::foundation::{build_info, error_dialog, logging, mod_diagnostics};

static INSTALLED: AtomicBool = AtomicBool::new(false);
static DIALOG_SHOWN: AtomicBool = AtomicBool::new(false);

type InvalidParameterHandler = unsafe extern "C" fn(*const u16, *const u16, *const u16, u32, usize);
type PurecallHandler = unsafe extern "C" fn();

unsafe extern "C" {
    fn _set_invalid_parameter_handler(
        handler: Option<InvalidParameterHandler>,
    ) -> Option<InvalidParameterHandler>;
    fn _set_purecall_handler(handler: Option<PurecallHandler>) -> Option<PurecallHandler>;
}

pub fn install_early() {
    install_handlers("dllmain-before-native-preload");
}

pub fn install() {
    install_handlers("bootstrap");
}

fn install_handlers(source: &str) {
    if INSTALLED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        unsafe {
            let _ = AddVectoredExceptionHandler(1, Some(vectored_exception_handler));
            let _ = _set_invalid_parameter_handler(Some(invalid_parameter_handler));
            let _ = _set_purecall_handler(Some(purecall_handler));
        }
        logging::write_bootstrap_marker(&format!(
            "crash_report.handlers.installed source={source} version={} sink=memory-only",
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
        "crash_report.unhandled_filter.armed reason={reason} no_disk=true"
    ));
}

/// Deliberately disabled in the no-disk diagnostic build. Spawning the external
/// logger would recreate crash-report files and add another process-side effect.
pub fn spawn_external_logger(_module_handle: usize) {
    logging::write_bootstrap_marker("crash_report.external_logger.disabled no_disk=true");
}

unsafe extern "system" fn vectored_exception_handler(exception: *mut EXCEPTION_POINTERS) -> i32 {
    if should_report_first_chance(exception) {
        emit_exception("veh", exception as *const EXCEPTION_POINTERS, false);
    }
    EXCEPTION_CONTINUE_SEARCH
}

unsafe extern "system" fn top_level_exception_filter(exception: *const EXCEPTION_POINTERS) -> i32 {
    emit_exception("seh", exception, true);
    EXCEPTION_EXECUTE_HANDLER
}

unsafe extern "C" fn invalid_parameter_handler(
    _expression: *const u16,
    _function: *const u16,
    _file: *const u16,
    line: u32,
    _reserved: usize,
) {
    emit_manual_failure(
        "invalid-parameter",
        &format!("CRT invalid parameter | line={line}"),
        true,
    );
}

unsafe extern "C" fn purecall_handler() {
    emit_manual_failure("purecall", "CRT pure virtual function call", true);
}

pub fn capture_rust_panic(details: &str, show_dialog: bool) {
    emit_manual_failure("rust-panic", details, show_dialog);
}

unsafe fn should_report_first_chance(exception: *const EXCEPTION_POINTERS) -> bool {
    if exception.is_null() || (*exception).ExceptionRecord.is_null() {
        return false;
    }
    matches!(
        (*(*exception).ExceptionRecord).ExceptionCode.0 as u32,
        0xC000_0005 | 0xC000_001D | 0xC000_0094 | 0xC000_0409
    )
}

unsafe fn emit_exception(phase: &str, exception: *const EXCEPTION_POINTERS, show_dialog: bool) {
    let (code, address) = if exception.is_null() || (*exception).ExceptionRecord.is_null() {
        (0u32, 0usize)
    } else {
        let record = &*(*exception).ExceptionRecord;
        (
            record.ExceptionCode.0 as u32,
            record.ExceptionAddress as usize,
        )
    };
    let active = mod_diagnostics::active_context_text();
    let message = format!(
        "CRASH_CAPTURED_MEMORY_ONLY | phase={phase} | code=0x{code:08X} | address=0x{address:X} | active={}",
        active.replace(['\r', '\n'], " | ")
    );
    logging::emergency_error_message("crash-report", &message);
    if show_dialog
        && DIALOG_SHOWN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        error_dialog::show_fatal_error(
            "BLoader Mod Crash",
            &format!(
                "An exception was observed by BLoader.\n\nCode: 0x{code:08X}\nAddress: 0x{address:X}\n\nNo crash file or minidump was written by this diagnostic build."
            ),
        );
    }
}

fn emit_manual_failure(phase: &str, details: &str, show_dialog: bool) {
    logging::emergency_error_message(
        "crash-report",
        &format!("MANUAL_CRASH_MEMORY_ONLY | phase={phase} | {details}"),
    );
    if show_dialog
        && DIALOG_SHOWN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        error_dialog::show_fatal_error(
            "BLoader Runtime Failure",
            &format!(
                "{details}\n\nNo crash file or minidump was written by this diagnostic build."
            ),
        );
    }
}

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
    EXCEPTION_POINTERS,
};

use crate::runtime::foundation::logging;

const IMAGE_DOS_SIGNATURE: u16 = 0x5A4D;
const IMAGE_NT_SIGNATURE: u32 = 0x0000_4550;
const IMAGE_NT_OPTIONAL_HDR32_MAGIC: u16 = 0x10B;
const IMAGE_NT_OPTIONAL_HDR64_MAGIC: u16 = 0x20B;
const EXCEPTION_BREAKPOINT_CODE: u32 = 0x8000_0003;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;
const PREMAIN_FAILURE_EXIT_CODE: u32 = 0xE017_0001;

static ENTRY_POINT: AtomicUsize = AtomicUsize::new(0);
static STARTUP_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static ORIGINAL_BYTE: AtomicU8 = AtomicU8::new(0);
static GATE_ARMED: AtomicBool = AtomicBool::new(false);
static BYTE_RESTORED: AtomicBool = AtomicBool::new(false);
static GATE_HIT: AtomicBool = AtomicBool::new(false);
static BOOTSTRAP_STARTED: AtomicBool = AtomicBool::new(false);
static BOOTSTRAP_COMPLETED: AtomicBool = AtomicBool::new(false);

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GetModuleHandleW"]
    fn get_module_handle_w(module_name: *const u16) -> *mut c_void;
    #[link_name = "VirtualProtect"]
    fn virtual_protect(
        address: *mut c_void,
        size: usize,
        new_protect: u32,
        old_protect: *mut u32,
    ) -> i32;
    #[link_name = "FlushInstructionCache"]
    fn flush_instruction_cache(
        process: *mut c_void,
        base_address: *const c_void,
        size: usize,
    ) -> i32;
    #[link_name = "GetCurrentProcess"]
    fn get_current_process() -> *mut c_void;
    #[link_name = "GetCurrentThreadId"]
    fn get_current_thread_id() -> u32;
    #[link_name = "TerminateProcess"]
    fn terminate_process(process: *mut c_void, exit_code: u32) -> i32;
}

/// Arms a one-byte INT3 gate at the host executable's original entry point.
///
/// BLoader is statically imported by the BMCBL PE patch. `DllMain` only installs
/// this gate while the Windows loader lock is held. No third-party DLL is loaded
/// from `DllMain`.
///
/// When the startup thread later reaches Minecraft's OEP, the loader lock has
/// already been released. The VEH restores the original byte and runs BLoader's
/// complete immediate startup/loading sequence synchronously on that same startup
/// thread. Only after the sequence returns successfully does execution resume at
/// the original OEP.
///
/// Dynamic `LoadLibrary` injection is intentionally excluded because the host OEP
/// may already have executed by the time BLoader is attached.
pub unsafe fn install_for_process_start(static_process_attach: bool) -> bool {
    if !static_process_attach {
        logging::write_bootstrap_marker(
            "premain-gate.skipped reason=dynamic-load-or-late-attach",
        );
        return false;
    }

    if GATE_ARMED.load(Ordering::Acquire) {
        return true;
    }

    let image_base = get_module_handle_w(ptr::null());
    if image_base.is_null() {
        logging::write_bootstrap_marker("premain-gate.install.failed reason=no-main-module");
        return false;
    }

    let Some(entry_point) = pe_entry_point(image_base as usize) else {
        logging::write_bootstrap_marker("premain-gate.install.failed reason=invalid-host-pe");
        return false;
    };

    let original_byte = ptr::read_volatile(entry_point as *const u8);
    if original_byte == 0xCC {
        logging::write_bootstrap_marker(&format!(
            "premain-gate.install.skipped reason=oep-already-breakpoint oep=0x{entry_point:X}"
        ));
        return false;
    }

    ENTRY_POINT.store(entry_point, Ordering::Release);
    ORIGINAL_BYTE.store(original_byte, Ordering::Release);
    STARTUP_THREAD_ID.store(get_current_thread_id(), Ordering::Release);
    BYTE_RESTORED.store(false, Ordering::Release);
    GATE_HIT.store(false, Ordering::Release);
    BOOTSTRAP_STARTED.store(false, Ordering::Release);
    BOOTSTRAP_COMPLETED.store(false, Ordering::Release);

    let handler = AddVectoredExceptionHandler(1, Some(pre_main_vectored_handler));
    if handler.is_null() {
        logging::write_bootstrap_marker("premain-gate.install.failed reason=veh-install");
        ENTRY_POINT.store(0, Ordering::Release);
        return false;
    }

    if !write_code_byte(entry_point, 0xCC) {
        logging::write_bootstrap_marker(&format!(
            "premain-gate.install.failed reason=oep-patch oep=0x{entry_point:X}"
        ));
        ENTRY_POINT.store(0, Ordering::Release);
        return false;
    }

    GATE_ARMED.store(true, Ordering::Release);
    logging::write_bootstrap_marker(&format!(
        "premain-gate.armed mode=oep-int3-inline-single-thread oep=0x{entry_point:X} startup_thread={}",
        STARTUP_THREAD_ID.load(Ordering::Acquire),
    ));
    true
}

pub fn is_armed() -> bool {
    GATE_ARMED.load(Ordering::Acquire)
}

unsafe extern "system" fn pre_main_vectored_handler(
    exception: *mut EXCEPTION_POINTERS,
) -> i32 {
    if !GATE_ARMED.load(Ordering::Acquire)
        || exception.is_null()
        || (*exception).ExceptionRecord.is_null()
        || (*exception).ContextRecord.is_null()
    {
        return EXCEPTION_CONTINUE_SEARCH;
    }

    let record = &*(*exception).ExceptionRecord;
    if record.ExceptionCode.0 as u32 != EXCEPTION_BREAKPOINT_CODE {
        return EXCEPTION_CONTINUE_SEARCH;
    }

    let entry_point = ENTRY_POINT.load(Ordering::Acquire);
    if entry_point == 0 || record.ExceptionAddress as usize != entry_point {
        return EXCEPTION_CONTINUE_SEARCH;
    }

    let current_thread_id = get_current_thread_id();
    if current_thread_id != STARTUP_THREAD_ID.load(Ordering::Acquire) {
        return EXCEPTION_CONTINUE_SEARCH;
    }

    GATE_HIT.store(true, Ordering::Release);

    if BYTE_RESTORED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let original = ORIGINAL_BYTE.load(Ordering::Acquire);
        if !write_code_byte(entry_point, original) {
            logging::write_bootstrap_marker(&format!(
                "premain-gate.restore.failed oep=0x{entry_point:X} thread={current_thread_id}"
            ));
            let _ = terminate_process(get_current_process(), PREMAIN_FAILURE_EXIT_CODE);
            return EXCEPTION_CONTINUE_EXECUTION;
        }
    }

    if BOOTSTRAP_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        logging::write_bootstrap_marker(&format!(
            "premain-gate.reentry.blocked oep=0x{entry_point:X} thread={current_thread_id}"
        ));
        let _ = terminate_process(get_current_process(), PREMAIN_FAILURE_EXIT_CODE);
        return EXCEPTION_CONTINUE_EXECUTION;
    }

    // The INT3 byte has already been restored. Disable the gate before invoking
    // third-party code so nested exceptions raised by a preload are never
    // mistaken for a second OEP gate hit.
    GATE_ARMED.store(false, Ordering::Release);
    logging::write_bootstrap_marker(&format!(
        "premain-gate.hit execution=inline-single-thread oep=0x{entry_point:X} thread={current_thread_id}"
    ));

    let bootstrap_ok = crate::run_bootstrap_on_startup_thread();
    if !bootstrap_ok {
        logging::write_bootstrap_marker(&format!(
            "premain-gate.bootstrap.failed thread={current_thread_id}"
        ));
        let _ = terminate_process(get_current_process(), PREMAIN_FAILURE_EXIT_CODE);
        return EXCEPTION_CONTINUE_EXECUTION;
    }

    BOOTSTRAP_COMPLETED.store(true, Ordering::Release);

    #[cfg(target_arch = "x86_64")]
    {
        (*(*exception).ContextRecord).Rip = entry_point as u64;
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = terminate_process(get_current_process(), PREMAIN_FAILURE_EXIT_CODE);
        return EXCEPTION_CONTINUE_EXECUTION;
    }

    logging::write_bootstrap_marker(&format!(
        "premain-gate.complete execution=inline-single-thread oep=0x{entry_point:X} thread={current_thread_id} byte_restored={} bootstrap_completed={}",
        BYTE_RESTORED.load(Ordering::Acquire),
        BOOTSTRAP_COMPLETED.load(Ordering::Acquire),
    ));
    EXCEPTION_CONTINUE_EXECUTION
}

unsafe fn write_code_byte(address: usize, value: u8) -> bool {
    let mut old_protect = 0u32;
    if virtual_protect(
        address as *mut c_void,
        1,
        PAGE_EXECUTE_READWRITE,
        &mut old_protect,
    ) == 0
    {
        return false;
    }

    ptr::write_volatile(address as *mut u8, value);
    let _ = flush_instruction_cache(get_current_process(), address as *const c_void, 1);

    let mut ignored = 0u32;
    virtual_protect(address as *mut c_void, 1, old_protect, &mut ignored) != 0
}

unsafe fn pe_entry_point(image_base: usize) -> Option<usize> {
    if read_u16(image_base)? != IMAGE_DOS_SIGNATURE {
        return None;
    }

    let e_lfanew = read_u32(image_base.checked_add(0x3C)?)? as usize;
    if e_lfanew < 0x40 || e_lfanew > 16 * 1024 * 1024 {
        return None;
    }

    let nt = image_base.checked_add(e_lfanew)?;
    if read_u32(nt)? != IMAGE_NT_SIGNATURE {
        return None;
    }

    let optional = nt.checked_add(4 + 20)?;
    let magic = read_u16(optional)?;
    if !matches!(magic, IMAGE_NT_OPTIONAL_HDR32_MAGIC | IMAGE_NT_OPTIONAL_HDR64_MAGIC) {
        return None;
    }

    let entry_rva = read_u32(optional.checked_add(16)?)? as usize;
    let size_of_image = read_u32(optional.checked_add(56)?)? as usize;
    if entry_rva == 0 || size_of_image == 0 || entry_rva >= size_of_image {
        return None;
    }

    image_base.checked_add(entry_rva)
}

unsafe fn read_u16(address: usize) -> Option<u16> {
    if address == 0 {
        None
    } else {
        Some(ptr::read_unaligned(address as *const u16))
    }
}

unsafe fn read_u32(address: usize) -> Option<u32> {
    if address == 0 {
        None
    } else {
        Some(ptr::read_unaligned(address as *const u32))
    }
}

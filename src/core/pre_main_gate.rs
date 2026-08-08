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
const WAIT_OBJECT_0: u32 = 0;
const PREMAIN_TIMEOUT_MS: u32 = 90_000;
const PREMAIN_TIMEOUT_EXIT_CODE: u32 = 0xE016_0001;

static ENTRY_POINT: AtomicUsize = AtomicUsize::new(0);
static READY_EVENT: AtomicUsize = AtomicUsize::new(0);
static STARTUP_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static ORIGINAL_BYTE: AtomicU8 = AtomicU8::new(0);
static GATE_ARMED: AtomicBool = AtomicBool::new(false);
static BYTE_RESTORED: AtomicBool = AtomicBool::new(false);
static RELEASE_REQUESTED: AtomicBool = AtomicBool::new(false);
static GATE_HIT: AtomicBool = AtomicBool::new(false);
static GATE_TIMED_OUT: AtomicBool = AtomicBool::new(false);

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
    #[link_name = "CreateEventW"]
    fn create_event_w(
        event_attributes: *const c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> *mut c_void;
    #[link_name = "SetEvent"]
    fn set_event(event: *mut c_void) -> i32;
    #[link_name = "WaitForSingleObject"]
    fn wait_for_single_object(handle: *mut c_void, milliseconds: u32) -> u32;
    #[link_name = "TerminateProcess"]
    fn terminate_process(process: *mut c_void, exit_code: u32) -> i32;
}

/// Arms a one-byte INT3 gate at the host executable's original entry point.
///
/// BLoader is statically imported by the BMCBL PE patch. During that startup path
/// `DllMain` runs under the Windows loader lock before Minecraft's OEP. We only
/// install the gate there; third-party DLL loading remains deferred to the
/// `bloader-bootstrap` thread after `DllMain` returns and the loader lock is free.
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

    let ready_event = create_event_w(ptr::null(), 1, 0, ptr::null());
    if ready_event.is_null() {
        logging::write_bootstrap_marker("premain-gate.install.failed reason=create-event");
        return false;
    }

    ENTRY_POINT.store(entry_point, Ordering::Release);
    ORIGINAL_BYTE.store(original_byte, Ordering::Release);
    READY_EVENT.store(ready_event as usize, Ordering::Release);
    STARTUP_THREAD_ID.store(get_current_thread_id(), Ordering::Release);
    RELEASE_REQUESTED.store(false, Ordering::Release);
    BYTE_RESTORED.store(false, Ordering::Release);
    GATE_HIT.store(false, Ordering::Release);
    GATE_TIMED_OUT.store(false, Ordering::Release);

    let handler = AddVectoredExceptionHandler(1, Some(pre_main_vectored_handler));
    if handler.is_null() {
        logging::write_bootstrap_marker("premain-gate.install.failed reason=veh-install");
        ENTRY_POINT.store(0, Ordering::Release);
        READY_EVENT.store(0, Ordering::Release);
        return false;
    }

    if !write_code_byte(entry_point, 0xCC) {
        logging::write_bootstrap_marker(&format!(
            "premain-gate.install.failed reason=oep-patch oep=0x{entry_point:X}"
        ));
        ENTRY_POINT.store(0, Ordering::Release);
        READY_EVENT.store(0, Ordering::Release);
        return false;
    }

    GATE_ARMED.store(true, Ordering::Release);
    logging::write_bootstrap_marker(&format!(
        "premain-gate.armed mode=oep-int3 oep=0x{entry_point:X} startup_thread={} timeout_ms={PREMAIN_TIMEOUT_MS}",
        STARTUP_THREAD_ID.load(Ordering::Acquire),
    ));
    true
}

/// Releases Minecraft's OEP after all critical pre-main preload work is complete.
///
/// The OEP byte is restored by the VEH on the startup thread immediately when the
/// breakpoint is hit. The thread then waits on this event and resumes from the
/// original first instruction only after this function signals readiness.
pub fn release(reason: &str) {
    if !GATE_ARMED.load(Ordering::Acquire) {
        logging::write_bootstrap_marker(&format!(
            "premain-gate.release.skipped reason={reason} armed=false"
        ));
        return;
    }

    RELEASE_REQUESTED.store(true, Ordering::Release);
    let event = READY_EVENT.load(Ordering::Acquire) as *mut c_void;
    if !event.is_null() {
        unsafe {
            let _ = set_event(event);
        }
    }

    logging::write_bootstrap_marker(&format!(
        "premain-gate.release reason={reason} hit={} byte_restored={} timed_out={}",
        GATE_HIT.load(Ordering::Acquire),
        BYTE_RESTORED.load(Ordering::Acquire),
        GATE_TIMED_OUT.load(Ordering::Acquire),
    ));
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

    if get_current_thread_id() != STARTUP_THREAD_ID.load(Ordering::Acquire) {
        return EXCEPTION_CONTINUE_SEARCH;
    }

    GATE_HIT.store(true, Ordering::Release);

    if BYTE_RESTORED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let original = ORIGINAL_BYTE.load(Ordering::Acquire);
        if !write_code_byte(entry_point, original) {
            // Continuing from OEP while the INT3 is still present would recurse
            // into this handler forever, so terminate instead of corrupting the
            // startup sequence.
            GATE_TIMED_OUT.store(true, Ordering::Release);
            let _ = terminate_process(get_current_process(), PREMAIN_TIMEOUT_EXIT_CODE);
            return EXCEPTION_CONTINUE_EXECUTION;
        }
    }

    if !RELEASE_REQUESTED.load(Ordering::Acquire) {
        let event = READY_EVENT.load(Ordering::Acquire) as *mut c_void;
        let wait_result = if event.is_null() {
            u32::MAX
        } else {
            wait_for_single_object(event, PREMAIN_TIMEOUT_MS)
        };
        if wait_result != WAIT_OBJECT_0 {
            GATE_TIMED_OUT.store(true, Ordering::Release);
            // Fail closed. Allowing Minecraft to continue while a bootstrap
            // loader is still replacing allocator/hooks recreates the exact
            // cross-allocator race this gate exists to prevent.
            let _ = terminate_process(get_current_process(), PREMAIN_TIMEOUT_EXIT_CODE);
            return EXCEPTION_CONTINUE_EXECUTION;
        }
    }

    #[cfg(target_arch = "x86_64")]
    {
        (*(*exception).ContextRecord).Rip = entry_point as u64;
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = terminate_process(get_current_process(), PREMAIN_TIMEOUT_EXIT_CODE);
        return EXCEPTION_CONTINUE_EXECUTION;
    }

    GATE_ARMED.store(false, Ordering::Release);
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
    let restored = virtual_protect(address as *mut c_void, 1, old_protect, &mut ignored) != 0;
    restored
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

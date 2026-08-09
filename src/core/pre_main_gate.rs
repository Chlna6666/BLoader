use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
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
static CONTEXT_SAVED: AtomicBool = AtomicBool::new(false);
static VEH_HANDLE: AtomicUsize = AtomicUsize::new(0);

struct SavedContextSlot(UnsafeCell<MaybeUninit<CONTEXT>>);

// The slot is written exactly once by the process startup thread while the gate
// owns the OEP and is read later by the same thread in `pre_main_dispatch`.
unsafe impl Sync for SavedContextSlot {}

static SAVED_CONTEXT: SavedContextSlot =
    SavedContextSlot(UnsafeCell::new(MaybeUninit::uninit()));

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
    #[link_name = "RemoveVectoredExceptionHandler"]
    fn remove_vectored_exception_handler(handle: *mut c_void) -> u32;
    #[link_name = "RtlRestoreContext"]
    fn rtl_restore_context(context_record: *const CONTEXT, exception_record: *const c_void);
}

/// Arms a one-byte INT3 gate at the host executable's original entry point.
///
/// BLoader is statically imported by the BMCBL PE patch. `DllMain` only installs
/// this gate while the Windows loader lock is held. No third-party DLL is loaded
/// from `DllMain`.
///
/// When the startup thread reaches Minecraft's OEP the VEH performs only the
/// minimum exception work required to hand execution to `pre_main_dispatch`:
/// restore the original OEP byte, copy the machine CONTEXT, redirect RIP, and
/// return. The complete BLoader bootstrap then runs after the VEH has returned,
/// in ordinary startup-thread execution context. `RtlRestoreContext` finally
/// restores the original register/stack state and resumes Minecraft at its OEP.
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
    CONTEXT_SAVED.store(false, Ordering::Release);
    VEH_HANDLE.store(0, Ordering::Release);

    let handler = AddVectoredExceptionHandler(1, Some(pre_main_vectored_handler));
    if handler.is_null() {
        logging::write_bootstrap_marker("premain-gate.install.failed reason=veh-install");
        ENTRY_POINT.store(0, Ordering::Release);
        return false;
    }
    VEH_HANDLE.store(handler as usize, Ordering::Release);

    if !write_code_byte(entry_point, 0xCC) {
        let handle = VEH_HANDLE.swap(0, Ordering::AcqRel);
        if handle != 0 {
            let _ = remove_vectored_exception_handler(handle as *mut c_void);
        }
        logging::write_bootstrap_marker(&format!(
            "premain-gate.install.failed reason=oep-patch oep=0x{entry_point:X}"
        ));
        ENTRY_POINT.store(0, Ordering::Release);
        return false;
    }

    GATE_ARMED.store(true, Ordering::Release);
    logging::write_bootstrap_marker(&format!(
        "premain-gate.armed mode=oep-int3-context-trampoline oep=0x{entry_point:X} startup_thread={}",
        STARTUP_THREAD_ID.load(Ordering::Acquire),
    ));
    true
}

pub fn is_armed() -> bool {
    GATE_ARMED.load(Ordering::Acquire)
}

/// First-chance VEH for the OEP breakpoint.
///
/// Keep this handler allocation-free and synchronization-free. Windows explicitly
/// recommends that vectored exception handlers only inspect/update exception state
/// and return. The real bootstrap is intentionally not called from here.
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
            let _ = terminate_process(get_current_process(), PREMAIN_FAILURE_EXIT_CODE);
            return EXCEPTION_CONTINUE_EXECUTION;
        }
    }

    if BOOTSTRAP_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        let _ = terminate_process(get_current_process(), PREMAIN_FAILURE_EXIT_CODE);
        return EXCEPTION_CONTINUE_EXECUTION;
    }

    let context = (*exception).ContextRecord;
    let saved_context = saved_context_ptr();
    ptr::copy_nonoverlapping(context, saved_context, 1);

    #[cfg(target_arch = "x86_64")]
    {
        // The breakpoint exception may report RIP after the one-byte INT3. The
        // context restored after bootstrap must restart at the original OEP.
        (*saved_context).Rip = entry_point as u64;
        (*context).Rip = pre_main_dispatch as usize as u64;
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = terminate_process(get_current_process(), PREMAIN_FAILURE_EXIT_CODE);
        return EXCEPTION_CONTINUE_EXECUTION;
    }

    CONTEXT_SAVED.store(true, Ordering::Release);

    // The OEP byte is restored and the live context now points at the ordinary
    // trampoline. Disable the gate before returning so no nested exception can
    // ever be treated as a second gate hit.
    GATE_ARMED.store(false, Ordering::Release);
    EXCEPTION_CONTINUE_EXECUTION
}

/// Ordinary execution target entered only after the VEH returned to the Windows
/// exception dispatcher. This function never returns through the Rust ABI.
unsafe extern "system" fn pre_main_dispatch() -> ! {
    let current_thread_id = get_current_thread_id();
    let expected_thread_id = STARTUP_THREAD_ID.load(Ordering::Acquire);
    let entry_point = ENTRY_POINT.load(Ordering::Acquire);

    if current_thread_id != expected_thread_id
        || entry_point == 0
        || !GATE_HIT.load(Ordering::Acquire)
        || !CONTEXT_SAVED.load(Ordering::Acquire)
    {
        terminate_for_gate_failure();
    }

    let handler = VEH_HANDLE.swap(0, Ordering::AcqRel);
    let handler_removed = if handler != 0 {
        remove_vectored_exception_handler(handler as *mut c_void) != 0
    } else {
        true
    };

    logging::write_bootstrap_marker(&format!(
        "premain-gate.dispatch.begin execution=post-veh-trampoline oep=0x{entry_point:X} thread={current_thread_id} veh_removed={handler_removed}"
    ));

    let bootstrap_ok = crate::run_bootstrap_on_startup_thread();
    if !bootstrap_ok {
        logging::write_bootstrap_marker(&format!(
            "premain-gate.bootstrap.failed execution=post-veh-trampoline thread={current_thread_id}"
        ));
        terminate_for_gate_failure();
    }

    BOOTSTRAP_COMPLETED.store(true, Ordering::Release);
    crate::core::runtime_ready::mark_oep_released("pre-main-gate-trampoline");

    logging::write_bootstrap_marker(&format!(
        "premain-gate.complete execution=post-veh-trampoline oep=0x{entry_point:X} thread={current_thread_id} byte_restored={} bootstrap_completed={} runtime_ready_signal=oep-released",
        BYTE_RESTORED.load(Ordering::Acquire),
        BOOTSTRAP_COMPLETED.load(Ordering::Acquire),
    ));
    logging::write_bootstrap_marker(&format!(
        "premain-gate.context.restore oep=0x{entry_point:X} thread={current_thread_id} method=RtlRestoreContext"
    ));

    rtl_restore_context(saved_context_ptr() as *const CONTEXT, ptr::null());

    // RtlRestoreContext is specified not to return. If it ever does, fail closed
    // rather than executing with the trampoline's temporary register state.
    terminate_for_gate_failure();
}

fn saved_context_ptr() -> *mut CONTEXT {
    SAVED_CONTEXT.0.get().cast::<CONTEXT>()
}

unsafe fn terminate_for_gate_failure() -> ! {
    let _ = terminate_process(get_current_process(), PREMAIN_FAILURE_EXIT_CODE);
    loop {
        std::hint::spin_loop();
    }
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

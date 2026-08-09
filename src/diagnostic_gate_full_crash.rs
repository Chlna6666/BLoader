#![allow(non_snake_case)]
#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
    EXCEPTION_POINTERS,
};
use windows::Win32::System::Environment::SetCurrentDirectoryW;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

mod diagnostic_foundation;

pub mod runtime {
    pub mod foundation {
        pub use crate::diagnostic_foundation::{
            build_info, crash_report, error_dialog, logging, mod_diagnostics, native_stdio,
        };
    }
}

type D3D12RenderCallback = unsafe extern "system" fn(
    device: *mut c_void,
    command_list: *mut c_void,
    back_buffer: *mut c_void,
    width: u32,
    height: u32,
);

const IMAGE_DOS_SIGNATURE: u16 = 0x5A4D;
const IMAGE_NT_SIGNATURE: u32 = 0x0000_4550;
const IMAGE_NT_OPTIONAL_HDR32_MAGIC: u16 = 0x10B;
const IMAGE_NT_OPTIONAL_HDR64_MAGIC: u16 = 0x20B;
const EXCEPTION_BREAKPOINT_CODE: u32 = 0x8000_0003;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;
const PREMAIN_FAILURE_EXIT_CODE: u32 = 0xE027_0001;

const CONTEXT_AMD64: u32 = 0x0010_0000;
const CONTEXT_ALL_AMD64: u32 = CONTEXT_AMD64 | 0x0000_001F;
const CONTEXT_XSTATE_AMD64: u32 = CONTEXT_AMD64 | 0x0000_0040;
const CONTEXT_MAX_CAPTURE_FLAGS: u32 = CONTEXT_ALL_AMD64 | CONTEXT_XSTATE_AMD64;
const SAVED_CONTEXT_BUFFER_SIZE: usize = 64 * 1024;

static ENTRY_POINT: AtomicUsize = AtomicUsize::new(0);
static STARTUP_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static ORIGINAL_BYTE: AtomicU8 = AtomicU8::new(0);
static GATE_ARMED: AtomicBool = AtomicBool::new(false);
static BYTE_RESTORED: AtomicBool = AtomicBool::new(false);
static CONTEXT_SAVED: AtomicBool = AtomicBool::new(false);
static GATE_VEH_HANDLE: AtomicUsize = AtomicUsize::new(0);
static SAVED_CONTEXT_PTR: AtomicUsize = AtomicUsize::new(0);
static SAVED_CONTEXT_CAPACITY_FLAGS: AtomicU32 = AtomicU32::new(0);

#[repr(align(64))]
struct SavedContextBuffer(UnsafeCell<[u8; SAVED_CONTEXT_BUFFER_SIZE]>);
unsafe impl Sync for SavedContextBuffer {}
static SAVED_CONTEXT_BUFFER: SavedContextBuffer =
    SavedContextBuffer(UnsafeCell::new([0; SAVED_CONTEXT_BUFFER_SIZE]));

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
    fn rtl_restore_context(context_record: *mut CONTEXT, exception_record: *const c_void);
    #[link_name = "InitializeContext"]
    fn initialize_context(
        buffer: *mut c_void,
        context_flags: u32,
        context: *mut *mut CONTEXT,
        context_length: *mut u32,
    ) -> i32;
    #[link_name = "CopyContext"]
    fn copy_context(
        destination: *mut CONTEXT,
        context_flags: u32,
        source: *const CONTEXT,
    ) -> i32;
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(
    _hinstance: HINSTANCE,
    call_reason: u32,
    reserved: *const c_void,
) -> i32 {
    if call_reason != DLL_PROCESS_ATTACH {
        return 1;
    }

    set_executable_current_directory();

    // 0.2.27 restores the real in-process crash reporter from the normal BLoader
    // runtime. This installs its full VEH/SEH/CRT handlers and keeps its real
    // report/minidump/symbolization code reachable. No other runtime subsystem is
    // initialized by this diagnostic build.
    runtime::foundation::crash_report::install_early();

    // lpvReserved != NULL identifies the static process-start load used by the
    // BMCBL PE import patch. Dynamic/late loads remain inert.
    if reserved.is_null() {
        return 1;
    }

    if install_oep_gate() { 1 } else { 0 }
}

unsafe fn set_executable_current_directory() {
    let mut path = [0u16; 32_768];
    let len = GetModuleFileNameW(None, &mut path) as usize;
    if len == 0 || len >= path.len() {
        return;
    }

    let Some(separator) = path[..len]
        .iter()
        .rposition(|ch| *ch == b'\\' as u16 || *ch == b'/' as u16)
    else {
        return;
    };
    if separator == 0 {
        return;
    }

    path[separator] = 0;
    let _ = SetCurrentDirectoryW(PCWSTR(path.as_ptr()));
}

unsafe fn install_oep_gate() -> bool {
    let image_base = get_module_handle_w(ptr::null());
    if image_base.is_null() {
        return false;
    }
    let Some(entry_point) = pe_entry_point(image_base as usize) else {
        return false;
    };

    let original_byte = ptr::read_volatile(entry_point as *const u8);
    if original_byte == 0xCC {
        return false;
    }

    let Some((saved_context, capacity_flags)) = initialize_saved_context() else {
        return false;
    };

    ENTRY_POINT.store(entry_point, Ordering::Release);
    STARTUP_THREAD_ID.store(get_current_thread_id(), Ordering::Release);
    ORIGINAL_BYTE.store(original_byte, Ordering::Release);
    BYTE_RESTORED.store(false, Ordering::Release);
    CONTEXT_SAVED.store(false, Ordering::Release);
    SAVED_CONTEXT_PTR.store(saved_context as usize, Ordering::Release);
    SAVED_CONTEXT_CAPACITY_FLAGS.store(capacity_flags, Ordering::Release);

    let handler = AddVectoredExceptionHandler(1, Some(oep_gate_veh));
    if handler.is_null() {
        return false;
    }
    GATE_VEH_HANDLE.store(handler as usize, Ordering::Release);

    if !write_code_byte(entry_point, 0xCC) {
        let handle = GATE_VEH_HANDLE.swap(0, Ordering::AcqRel);
        if handle != 0 {
            let _ = remove_vectored_exception_handler(handle as *mut c_void);
        }
        return false;
    }

    GATE_ARMED.store(true, Ordering::Release);
    true
}

unsafe fn initialize_saved_context() -> Option<(*mut CONTEXT, u32)> {
    let buffer = SAVED_CONTEXT_BUFFER.0.get().cast::<c_void>();
    let mut context = ptr::null_mut::<CONTEXT>();
    let mut context_length = SAVED_CONTEXT_BUFFER_SIZE as u32;

    if initialize_context(
        buffer,
        CONTEXT_MAX_CAPTURE_FLAGS,
        &mut context,
        &mut context_length,
    ) != 0
        && !context.is_null()
    {
        return Some((context, CONTEXT_MAX_CAPTURE_FLAGS));
    }

    context = ptr::null_mut();
    context_length = SAVED_CONTEXT_BUFFER_SIZE as u32;
    if initialize_context(
        buffer,
        CONTEXT_ALL_AMD64,
        &mut context,
        &mut context_length,
    ) != 0
        && !context.is_null()
    {
        return Some((context, CONTEXT_ALL_AMD64));
    }

    None
}

unsafe extern "system" fn oep_gate_veh(exception: *mut EXCEPTION_POINTERS) -> i32 {
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

    if BYTE_RESTORED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        if !write_code_byte(entry_point, ORIGINAL_BYTE.load(Ordering::Acquire)) {
            terminate_gate_failure();
        }
    }

    let source = (*exception).ContextRecord;
    let saved = SAVED_CONTEXT_PTR.load(Ordering::Acquire) as *mut CONTEXT;
    if saved.is_null() {
        terminate_gate_failure();
    }

    let source_flags = (*source).ContextFlags.0;
    let capacity_flags = SAVED_CONTEXT_CAPACITY_FLAGS.load(Ordering::Acquire);
    let copy_flags = source_flags & capacity_flags & CONTEXT_MAX_CAPTURE_FLAGS;
    if copy_flags == 0 || copy_context(saved, copy_flags, source) == 0 {
        terminate_gate_failure();
    }

    #[cfg(target_arch = "x86_64")]
    {
        (*saved).Rip = entry_point as u64;
        (*source).Rip = gate_dispatch as *const () as usize as u64;
    }
    #[cfg(not(target_arch = "x86_64"))]
    terminate_gate_failure();

    CONTEXT_SAVED.store(true, Ordering::Release);
    GATE_ARMED.store(false, Ordering::Release);
    EXCEPTION_CONTINUE_EXECUTION
}

unsafe extern "system" fn gate_dispatch() -> ! {
    if get_current_thread_id() != STARTUP_THREAD_ID.load(Ordering::Acquire)
        || !BYTE_RESTORED.load(Ordering::Acquire)
        || !CONTEXT_SAVED.load(Ordering::Acquire)
    {
        terminate_gate_failure();
    }

    let handler = GATE_VEH_HANDLE.swap(0, Ordering::AcqRel);
    if handler != 0 {
        let _ = remove_vectored_exception_handler(handler as *mut c_void);
    }

    let saved = SAVED_CONTEXT_PTR.load(Ordering::Acquire) as *mut CONTEXT;
    if saved.is_null() {
        terminate_gate_failure();
    }

    // No bootstrap, no config, no hooks and no worker threads. The trampoline
    // immediately performs the same XState-aware context round-trip as 0.2.26.
    rtl_restore_context(saved, ptr::null());
    terminate_gate_failure();
}

unsafe fn terminate_gate_failure() -> ! {
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
    let nt = image_base.checked_add(e_lfanew)?;
    if read_u32(nt)? != IMAGE_NT_SIGNATURE {
        return None;
    }

    let optional = nt.checked_add(24)?;
    let magic = read_u16(optional)?;
    if magic != IMAGE_NT_OPTIONAL_HDR32_MAGIC && magic != IMAGE_NT_OPTIONAL_HDR64_MAGIC {
        return None;
    }

    let entry_rva = read_u32(optional.checked_add(16)?)? as usize;
    if entry_rva == 0 {
        return None;
    }
    image_base.checked_add(entry_rva)
}

unsafe fn read_u16(address: usize) -> Option<u16> {
    if address == 0 {
        return None;
    }
    Some(ptr::read_unaligned(address as *const u16))
}

unsafe fn read_u32(address: usize) -> Option<u32> {
    if address == 0 {
        return None;
    }
    Some(ptr::read_unaligned(address as *const u32))
}

#[unsafe(export_name = "bl_i18n_current_locale")]
pub unsafe extern "system" fn diagnostic_i18n_current_locale(
    out_buf: *mut u8,
    out_len: usize,
) -> usize {
    if !out_buf.is_null() && out_len != 0 {
        *out_buf = 0;
    }
    0
}

#[unsafe(export_name = "bl_register_d3d12_render_callback")]
pub unsafe extern "system" fn diagnostic_register_d3d12_render_callback(
    _callback: D3D12RenderCallback,
) {
}

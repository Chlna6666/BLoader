use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use minhook::MinHook;
use parking_lot::{Mutex, RwLock};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{BOOL, PCSTR, PCWSTR, s, w};

use crate::runtime::foundation::{build_info, logging};

const COPYRIGHT_KEY: &[u8] = b"menu.copyright=";
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
const FILE_BEGIN: u32 = 0;
const FILE_CURRENT: u32 = 1;
const FILE_END: u32 = 2;
const PAGE_READWRITE: u32 = 0x04;
const FILE_MAP_WRITE: u32 = 0x0002;
const FILE_STANDARD_INFO_CLASS: u32 = 1;

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static STATE: OnceLock<LaunchInfoState> = OnceLock::new();
static OVERLAPPED_READ_LOGGED: AtomicBool = AtomicBool::new(false);

static ORIGINAL_READ_FILE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_SIZE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_SIZE_EX: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SET_FILE_POINTER: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SET_FILE_POINTER_EX: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_INFORMATION_BY_HANDLE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_INFORMATION_BY_HANDLE_EX: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_FILE_MAPPING_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CLOSE_HANDLE: AtomicUsize = AtomicUsize::new(0);

static GET_FINAL_PATH_NAME_BY_HANDLE_W: AtomicUsize = AtomicUsize::new(0);
static MAP_VIEW_OF_FILE: AtomicUsize = AtomicUsize::new(0);
static UNMAP_VIEW_OF_FILE: AtomicUsize = AtomicUsize::new(0);
static SET_EVENT: AtomicUsize = AtomicUsize::new(0);

type ReadFileFn =
    unsafe extern "system" fn(HANDLE, *mut c_void, u32, *mut u32, *mut RawOverlapped) -> BOOL;
type GetFileSizeFn = unsafe extern "system" fn(HANDLE, *mut u32) -> u32;
type GetFileSizeExFn = unsafe extern "system" fn(HANDLE, *mut i64) -> BOOL;
type SetFilePointerFn = unsafe extern "system" fn(HANDLE, i32, *mut i32, u32) -> u32;
type SetFilePointerExFn = unsafe extern "system" fn(HANDLE, i64, *mut i64, u32) -> BOOL;
type GetFileInformationByHandleFn = unsafe extern "system" fn(HANDLE, *mut c_void) -> BOOL;
type GetFileInformationByHandleExFn =
    unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32) -> BOOL;
type CreateFileMappingWFn = unsafe extern "system" fn(
    HANDLE,
    *const SECURITY_ATTRIBUTES,
    u32,
    u32,
    u32,
    PCWSTR,
) -> HANDLE;
type CloseHandleFn = unsafe extern "system" fn(HANDLE) -> BOOL;
type GetFinalPathNameByHandleWFn = unsafe extern "system" fn(HANDLE, *mut u16, u32, u32) -> u32;
type MapViewOfFileFn = unsafe extern "system" fn(HANDLE, u32, u32, u32, usize) -> *mut c_void;
type UnmapViewOfFileFn = unsafe extern "system" fn(*const c_void) -> BOOL;
type SetEventFn = unsafe extern "system" fn(HANDLE) -> BOOL;

#[repr(C)]
struct RawOverlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: HANDLE,
}

thread_local! {
    static IN_LAUNCH_INFO_HOOK: Cell<bool> = const { Cell::new(false) };
}

struct HookGuard;

impl HookGuard {
    fn enter() -> Option<Self> {
        IN_LAUNCH_INFO_HOOK.with(|flag| {
            if flag.get() {
                None
            } else {
                flag.set(true);
                Some(Self)
            }
        })
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        IN_LAUNCH_INFO_HOOK.with(|flag| flag.set(false));
    }
}

struct LaunchInfoState {
    texts_prefix: String,
    handles: RwLock<HashMap<usize, HandleEntry>>,
    file_cache: RwLock<HashMap<String, Arc<[u8]>>>,
}

enum HandleEntry {
    Passthrough,
    Virtual(Arc<VirtualFile>),
}

struct VirtualFile {
    data: Arc<[u8]>,
    cursor: Mutex<u64>,
}

pub fn install(game_dir: &Path) -> bool {
    let texts_dir = game_dir
        .join("data")
        .join("resource_packs")
        .join("vanilla")
        .join("texts");
    if !texts_dir.is_dir() {
        logging::scoped_debug_message(
            "launch-info",
            &format!(
                "vanilla text directory is unavailable; in-memory launch info hook skipped | path={}",
                texts_dir.display()
            ),
        );
        return false;
    }

    let mut texts_prefix = normalize_windows_path(&texts_dir.to_string_lossy());
    if !texts_prefix.ends_with('\\') {
        texts_prefix.push('\\');
    }

    if STATE
        .set(LaunchInfoState {
            texts_prefix,
            handles: RwLock::new(HashMap::new()),
            file_cache: RwLock::new(HashMap::new()),
        })
        .is_err()
    {
        return HOOK_INSTALLED.load(Ordering::Acquire);
    }

    if HOOK_INSTALLED.swap(true, Ordering::AcqRel) {
        return true;
    }

    let installed = unsafe { install_hooks() };
    if installed == 0 {
        HOOK_INSTALLED.store(false, Ordering::Release);
        logging::scoped_warn_message("launch-info", "failed to install in-memory file hooks");
        return false;
    }

    if let Err(error) = unsafe { MinHook::enable_all_hooks() } {
        HOOK_INSTALLED.store(false, Ordering::Release);
        logging::scoped_warn_message(
            "launch-info",
            &format!("failed to enable in-memory file hooks: {error:?}"),
        );
        return false;
    }

    logging::scoped_info_message(
        "launch-info",
        &format!(
            "installed {installed} in-memory hooks | target=vanilla/texts/*.lang | text=©Mojang AB / BMCBL , BLoader {} | disk_mutation=none | shadow_files=none",
            build_info::VERSION
        ),
    );
    true
}

unsafe fn install_hooks() -> usize {
    let mut installed = 0usize;

    macro_rules! hook {
        ($name:literal, $detour:expr, $slot:expr) => {
            installed += hook_kernel_export(s!($name), $name, $detour as *mut c_void, $slot) as usize;
        };
    }

    hook!("ReadFile", detour_read_file, &ORIGINAL_READ_FILE);
    hook!("GetFileSize", detour_get_file_size, &ORIGINAL_GET_FILE_SIZE);
    hook!("GetFileSizeEx", detour_get_file_size_ex, &ORIGINAL_GET_FILE_SIZE_EX);
    hook!(
        "SetFilePointer",
        detour_set_file_pointer,
        &ORIGINAL_SET_FILE_POINTER
    );
    hook!(
        "SetFilePointerEx",
        detour_set_file_pointer_ex,
        &ORIGINAL_SET_FILE_POINTER_EX
    );
    hook!(
        "GetFileInformationByHandle",
        detour_get_file_information_by_handle,
        &ORIGINAL_GET_FILE_INFORMATION_BY_HANDLE
    );
    hook!(
        "GetFileInformationByHandleEx",
        detour_get_file_information_by_handle_ex,
        &ORIGINAL_GET_FILE_INFORMATION_BY_HANDLE_EX
    );
    hook!(
        "CreateFileMappingW",
        detour_create_file_mapping_w,
        &ORIGINAL_CREATE_FILE_MAPPING_W
    );
    hook!("CloseHandle", detour_close_handle, &ORIGINAL_CLOSE_HANDLE);

    GET_FINAL_PATH_NAME_BY_HANDLE_W.store(
        resolve_kernel_export(s!("GetFinalPathNameByHandleW")),
        Ordering::Release,
    );
    MAP_VIEW_OF_FILE.store(resolve_kernel_export(s!("MapViewOfFile")), Ordering::Release);
    UNMAP_VIEW_OF_FILE.store(
        resolve_kernel_export(s!("UnmapViewOfFile")),
        Ordering::Release,
    );
    SET_EVENT.store(resolve_kernel_export(s!("SetEvent")), Ordering::Release);

    installed
}

unsafe fn hook_kernel_export(
    proc_name: PCSTR,
    label: &str,
    detour: *mut c_void,
    original: &AtomicUsize,
) -> bool {
    let target = resolve_kernel_export(proc_name);
    if target == 0 {
        logging::scoped_debug_message(
            "launch-info",
            &format!("hook export unavailable api={label}"),
        );
        return false;
    }

    match MinHook::create_hook(target as *mut c_void, detour) {
        Ok(trampoline) => {
            original.store(trampoline as usize, Ordering::Release);
            true
        }
        Err(error) => {
            logging::scoped_warn_message(
                "launch-info",
                &format!("failed to hook {label}: {error:?}"),
            );
            false
        }
    }
}

unsafe fn resolve_kernel_export(proc_name: PCSTR) -> usize {
    for module_name in [w!("KernelBase.dll"), w!("kernel32.dll")] {
        let Ok(module) = GetModuleHandleW(module_name) else {
            continue;
        };
        if module.is_invalid() {
            continue;
        }
        if let Some(proc) = GetProcAddress(module, proc_name) {
            return proc as usize;
        }
    }
    0
}

macro_rules! guard_or_call_original {
    ($original:expr $(, $arg:expr)* $(,)?) => {
        let Some(_hook_guard) = HookGuard::enter() else {
            return $original($($arg),*);
        };
    };
}

unsafe extern "system" fn detour_read_file(
    handle: HANDLE,
    buffer: *mut c_void,
    bytes_to_read: u32,
    bytes_read: *mut u32,
    overlapped: *mut RawOverlapped,
) -> BOOL {
    let original: ReadFileFn = mem::transmute(ORIGINAL_READ_FILE.load(Ordering::Acquire));
    guard_or_call_original!(original, handle, buffer, bytes_to_read, bytes_read, overlapped);

    let Some(file) = virtual_file_for_handle(handle) else {
        return original(handle, buffer, bytes_to_read, bytes_read, overlapped);
    };
    if buffer.is_null() && bytes_to_read != 0 {
        return original(handle, buffer, bytes_to_read, bytes_read, overlapped);
    }

    let (offset, update_cursor) = if let Some(overlapped) = overlapped.as_mut() {
        if !OVERLAPPED_READ_LOGGED.swap(true, Ordering::Relaxed) {
            logging::scoped_debug_message(
                "launch-info",
                "serving an OVERLAPPED language read synchronously from the in-memory view",
            );
        }
        (
            u64::from(overlapped.offset) | (u64::from(overlapped.offset_high) << 32),
            false,
        )
    } else {
        (*file.cursor.lock(), true)
    };

    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(file.data.len());
    let available = file.data.len().saturating_sub(start);
    let count = available.min(bytes_to_read as usize);
    if count != 0 {
        ptr::copy_nonoverlapping(file.data.as_ptr().add(start), buffer.cast::<u8>(), count);
    }

    if !bytes_read.is_null() {
        *bytes_read = count as u32;
    }

    if update_cursor {
        let new_position = offset.saturating_add(count as u64);
        *file.cursor.lock() = new_position;
        sync_kernel_cursor(handle, new_position);
    } else if let Some(overlapped) = overlapped.as_mut() {
        overlapped.internal = 0;
        overlapped.internal_high = count;
        if !overlapped.event.is_invalid() {
            signal_event(overlapped.event);
        }
    }

    BOOL(1)
}

unsafe extern "system" fn detour_get_file_size(handle: HANDLE, high: *mut u32) -> u32 {
    let original: GetFileSizeFn = mem::transmute(ORIGINAL_GET_FILE_SIZE.load(Ordering::Acquire));
    guard_or_call_original!(original, handle, high);

    let Some(file) = virtual_file_for_handle(handle) else {
        return original(handle, high);
    };
    let size = file.data.len() as u64;
    if !high.is_null() {
        *high = (size >> 32) as u32;
    }
    size as u32
}

unsafe extern "system" fn detour_get_file_size_ex(handle: HANDLE, size: *mut i64) -> BOOL {
    let original: GetFileSizeExFn =
        mem::transmute(ORIGINAL_GET_FILE_SIZE_EX.load(Ordering::Acquire));
    guard_or_call_original!(original, handle, size);

    let Some(file) = virtual_file_for_handle(handle) else {
        return original(handle, size);
    };
    if size.is_null() {
        return original(handle, size);
    }
    *size = file.data.len() as i64;
    BOOL(1)
}

unsafe extern "system" fn detour_set_file_pointer(
    handle: HANDLE,
    distance_low: i32,
    distance_high: *mut i32,
    method: u32,
) -> u32 {
    let original: SetFilePointerFn =
        mem::transmute(ORIGINAL_SET_FILE_POINTER.load(Ordering::Acquire));
    guard_or_call_original!(original, handle, distance_low, distance_high, method);

    let Some(file) = virtual_file_for_handle(handle) else {
        return original(handle, distance_low, distance_high, method);
    };

    let distance = if distance_high.is_null() {
        i64::from(distance_low)
    } else {
        ((*distance_high as i64) << 32) | i64::from(distance_low as u32)
    };
    let Some(position) = calculate_seek(&file, distance, method) else {
        return original(handle, distance_low, distance_high, method);
    };

    *file.cursor.lock() = position;
    sync_kernel_cursor(handle, position);
    if !distance_high.is_null() {
        *distance_high = (position >> 32) as i32;
    }
    position as u32
}

unsafe extern "system" fn detour_set_file_pointer_ex(
    handle: HANDLE,
    distance: i64,
    new_position: *mut i64,
    method: u32,
) -> BOOL {
    let original: SetFilePointerExFn =
        mem::transmute(ORIGINAL_SET_FILE_POINTER_EX.load(Ordering::Acquire));
    guard_or_call_original!(original, handle, distance, new_position, method);

    let Some(file) = virtual_file_for_handle(handle) else {
        return original(handle, distance, new_position, method);
    };
    let Some(position) = calculate_seek(&file, distance, method) else {
        return original(handle, distance, new_position, method);
    };

    *file.cursor.lock() = position;
    sync_kernel_cursor(handle, position);
    if !new_position.is_null() {
        *new_position = position as i64;
    }
    BOOL(1)
}

unsafe extern "system" fn detour_get_file_information_by_handle(
    handle: HANDLE,
    information: *mut c_void,
) -> BOOL {
    let original: GetFileInformationByHandleFn = mem::transmute(
        ORIGINAL_GET_FILE_INFORMATION_BY_HANDLE.load(Ordering::Acquire),
    );
    guard_or_call_original!(original, handle, information);

    let result = original(handle, information);
    if result.as_bool() && !information.is_null() {
        if let Some(file) = virtual_file_for_handle(handle) {
            let size = file.data.len() as u64;
            let bytes = information.cast::<u8>();
            ptr::write_unaligned(bytes.add(32).cast::<u32>(), (size >> 32) as u32);
            ptr::write_unaligned(bytes.add(36).cast::<u32>(), size as u32);
        }
    }
    result
}

unsafe extern "system" fn detour_get_file_information_by_handle_ex(
    handle: HANDLE,
    info_class: u32,
    information: *mut c_void,
    buffer_size: u32,
) -> BOOL {
    let original: GetFileInformationByHandleExFn = mem::transmute(
        ORIGINAL_GET_FILE_INFORMATION_BY_HANDLE_EX.load(Ordering::Acquire),
    );
    guard_or_call_original!(original, handle, info_class, information, buffer_size);

    let result = original(handle, info_class, information, buffer_size);
    if result.as_bool()
        && info_class == FILE_STANDARD_INFO_CLASS
        && !information.is_null()
        && buffer_size >= 16
    {
        if let Some(file) = virtual_file_for_handle(handle) {
            let size = file.data.len() as i64;
            let bytes = information.cast::<u8>();
            ptr::write_unaligned(bytes.cast::<i64>(), size);
            ptr::write_unaligned(bytes.add(8).cast::<i64>(), size);
        }
    }
    result
}

unsafe extern "system" fn detour_create_file_mapping_w(
    handle: HANDLE,
    security_attributes: *const SECURITY_ATTRIBUTES,
    protect: u32,
    maximum_size_high: u32,
    maximum_size_low: u32,
    name: PCWSTR,
) -> HANDLE {
    let original: CreateFileMappingWFn = mem::transmute(
        ORIGINAL_CREATE_FILE_MAPPING_W.load(Ordering::Acquire),
    );
    guard_or_call_original!(
        original,
        handle,
        security_attributes,
        protect,
        maximum_size_high,
        maximum_size_low,
        name
    );

    let Some(file) = virtual_file_for_handle(handle) else {
        return original(
            handle,
            security_attributes,
            protect,
            maximum_size_high,
            maximum_size_low,
            name,
        );
    };

    let requested_size =
        (u64::from(maximum_size_high) << 32) | u64::from(maximum_size_low);
    let mapping_size = if requested_size == 0 {
        file.data.len() as u64
    } else {
        requested_size.max(file.data.len() as u64)
    };
    if mapping_size == 0 {
        return original(
            handle,
            security_attributes,
            protect,
            maximum_size_high,
            maximum_size_low,
            name,
        );
    }

    let invalid_file = HANDLE((-1isize) as *mut c_void);
    let mapping = original(
        invalid_file,
        security_attributes,
        PAGE_READWRITE,
        (mapping_size >> 32) as u32,
        mapping_size as u32,
        name,
    );
    if mapping.is_invalid() {
        return original(
            handle,
            security_attributes,
            protect,
            maximum_size_high,
            maximum_size_low,
            name,
        );
    }

    if !populate_mapping(mapping, &file.data) {
        close_original_handle(mapping);
        return original(
            handle,
            security_attributes,
            protect,
            maximum_size_high,
            maximum_size_low,
            name,
        );
    }

    logging::scoped_debug_message(
        "launch-info",
        &format!(
            "served memory-mapped language view | bytes={} requested_protect=0x{protect:X}",
            file.data.len()
        ),
    );
    mapping
}

unsafe extern "system" fn detour_close_handle(handle: HANDLE) -> BOOL {
    let original: CloseHandleFn = mem::transmute(ORIGINAL_CLOSE_HANDLE.load(Ordering::Acquire));
    guard_or_call_original!(original, handle);

    if let Some(state) = STATE.get() {
        state.handles.write().remove(&handle_key(handle));
    }
    original(handle)
}

fn virtual_file_for_handle(handle: HANDLE) -> Option<Arc<VirtualFile>> {
    if handle.is_invalid() {
        return None;
    }
    let state = STATE.get()?;
    let key = handle_key(handle);

    if let Some(entry) = state.handles.read().get(&key) {
        return match entry {
            HandleEntry::Passthrough => None,
            HandleEntry::Virtual(file) => Some(Arc::clone(file)),
        };
    }

    classify_handle(state, handle, key)
}

fn classify_handle(
    state: &LaunchInfoState,
    handle: HANDLE,
    key: usize,
) -> Option<Arc<VirtualFile>> {
    let Some(final_path) = final_path_for_handle(handle) else {
        state.handles.write().insert(key, HandleEntry::Passthrough);
        return None;
    };
    let path_key = normalize_windows_path(&final_path);
    if !is_target_language_path(state, &path_key) {
        state.handles.write().insert(key, HandleEntry::Passthrough);
        return None;
    }

    let data = if let Some(cached) = state.file_cache.read().get(&path_key) {
        Arc::clone(cached)
    } else {
        let disk_path = windows_path_for_std(&final_path);
        let Ok(original) = std::fs::read(&disk_path) else {
            logging::scoped_warn_message(
                "launch-info",
                &format!(
                    "failed to read language file for in-memory virtualization; using original handle | path={}",
                    disk_path.display()
                ),
            );
            state.handles.write().insert(key, HandleEntry::Passthrough);
            return None;
        };
        let launch_text = format!(
            "©Mojang AB / BMCBL , BLoader {}",
            build_info::VERSION
        );
        let rewritten: Arc<[u8]> = rewrite_language_bytes(&original, &launch_text).into();
        state
            .file_cache
            .write()
            .insert(path_key.clone(), Arc::clone(&rewritten));
        rewritten
    };

    let cursor = current_kernel_cursor(handle).unwrap_or(0);
    let file = Arc::new(VirtualFile {
        data,
        cursor: Mutex::new(cursor),
    });

    let mut handles = state.handles.write();
    if let Some(existing) = handles.get(&key) {
        return match existing {
            HandleEntry::Passthrough => None,
            HandleEntry::Virtual(existing) => Some(Arc::clone(existing)),
        };
    }
    handles.insert(key, HandleEntry::Virtual(Arc::clone(&file)));
    drop(handles);

    logging::scoped_debug_message(
        "launch-info",
        &format!(
            "virtualized language handle in memory | path={} bytes={}",
            final_path,
            file.data.len()
        ),
    );
    Some(file)
}

fn is_target_language_path(state: &LaunchInfoState, path_key: &str) -> bool {
    path_key.starts_with(&state.texts_prefix)
        && path_key
            .rsplit('\\')
            .next()
            .is_some_and(|name| name.ends_with(".lang"))
}

fn calculate_seek(file: &VirtualFile, distance: i64, method: u32) -> Option<u64> {
    let base = match method {
        FILE_BEGIN => 0i128,
        FILE_CURRENT => i128::from(*file.cursor.lock()),
        FILE_END => file.data.len() as i128,
        _ => return None,
    };
    let position = base.checked_add(i128::from(distance))?;
    if position < 0 || position > i128::from(u64::MAX) {
        return None;
    }
    Some(position as u64)
}

fn current_kernel_cursor(handle: HANDLE) -> Option<u64> {
    let address = ORIGINAL_SET_FILE_POINTER_EX.load(Ordering::Acquire);
    if address == 0 {
        return Some(0);
    }
    let original: SetFilePointerExFn = unsafe { mem::transmute(address) };
    let mut position = 0i64;
    let ok = unsafe { original(handle, 0, &mut position, FILE_CURRENT) };
    if ok.as_bool() && position >= 0 {
        Some(position as u64)
    } else {
        Some(0)
    }
}

fn sync_kernel_cursor(handle: HANDLE, position: u64) {
    let address = ORIGINAL_SET_FILE_POINTER_EX.load(Ordering::Acquire);
    if address == 0 || position > i64::MAX as u64 {
        return;
    }
    let original: SetFilePointerExFn = unsafe { mem::transmute(address) };
    unsafe {
        let _ = original(handle, position as i64, ptr::null_mut(), FILE_BEGIN);
    }
}

fn final_path_for_handle(handle: HANDLE) -> Option<String> {
    let address = GET_FINAL_PATH_NAME_BY_HANDLE_W.load(Ordering::Acquire);
    if address == 0 {
        return None;
    }
    let get_path: GetFinalPathNameByHandleWFn = unsafe { mem::transmute(address) };

    let mut stack = [0u16; 512];
    let length = unsafe { get_path(handle, stack.as_mut_ptr(), stack.len() as u32, 0) } as usize;
    if length == 0 {
        return None;
    }
    if length < stack.len() {
        return String::from_utf16(&stack[..length]).ok();
    }

    let mut buffer = vec![0u16; length.saturating_add(1)];
    let length = unsafe { get_path(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0) } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    String::from_utf16(&buffer[..length]).ok()
}

fn populate_mapping(mapping: HANDLE, data: &[u8]) -> bool {
    let map_address = MAP_VIEW_OF_FILE.load(Ordering::Acquire);
    let unmap_address = UNMAP_VIEW_OF_FILE.load(Ordering::Acquire);
    if map_address == 0 || unmap_address == 0 {
        return false;
    }
    let map_view: MapViewOfFileFn = unsafe { mem::transmute(map_address) };
    let unmap_view: UnmapViewOfFileFn = unsafe { mem::transmute(unmap_address) };

    let view = unsafe { map_view(mapping, FILE_MAP_WRITE, 0, 0, data.len()) };
    if view.is_null() {
        return false;
    }
    if !data.is_empty() {
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), view.cast::<u8>(), data.len());
        }
    }
    unsafe { unmap_view(view.cast_const()).as_bool() }
}

fn close_original_handle(handle: HANDLE) {
    let address = ORIGINAL_CLOSE_HANDLE.load(Ordering::Acquire);
    if address == 0 {
        return;
    }
    let close: CloseHandleFn = unsafe { mem::transmute(address) };
    unsafe {
        let _ = close(handle);
    }
}

fn signal_event(event: HANDLE) {
    let address = SET_EVENT.load(Ordering::Acquire);
    if address == 0 {
        return;
    }
    let set_event: SetEventFn = unsafe { mem::transmute(address) };
    unsafe {
        let _ = set_event(event);
    }
}

fn handle_key(handle: HANDLE) -> usize {
    handle.0 as usize
}

fn normalize_windows_path(path: &str) -> String {
    let mut path = path.trim().replace('/', "\\");
    let lower = path.to_ascii_lowercase();
    if lower.starts_with(r"\\?\unc\") {
        path = format!(r"\\{}", &path[8..]);
    } else if lower.starts_with(r"\??\unc\") {
        path = format!(r"\\{}", &path[8..]);
    } else if lower.starts_with(r"\\?\") || lower.starts_with(r"\??\") {
        path = path[4..].to_string();
    }
    path.trim_end_matches('\\').to_ascii_lowercase()
}

fn windows_path_for_std(path: &str) -> PathBuf {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with(r"\??\unc\") {
        return PathBuf::from(format!(r"\\{}", &path[8..]));
    }
    if lower.starts_with(r"\??\") {
        return PathBuf::from(&path[4..]);
    }
    PathBuf::from(path)
}

fn rewrite_language_bytes(input: &[u8], launch_text: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len().saturating_add(launch_text.len() + 32));
    let mut found = false;
    let mut first_line = true;

    for segment in input.split_inclusive(|byte| *byte == b'\n') {
        let newline_len = if segment.ends_with(b"\r\n") {
            2
        } else if segment.ends_with(b"\n") {
            1
        } else {
            0
        };
        let body_len = segment.len() - newline_len;
        let body = &segment[..body_len];
        let bom_len = if first_line && body.starts_with(UTF8_BOM) {
            UTF8_BOM.len()
        } else {
            0
        };

        if body[bom_len..].starts_with(COPYRIGHT_KEY) {
            output.extend_from_slice(&body[..bom_len]);
            output.extend_from_slice(COPYRIGHT_KEY);
            output.extend_from_slice(launch_text.as_bytes());
            output.extend_from_slice(&segment[body_len..]);
            found = true;
        } else {
            output.extend_from_slice(segment);
        }
        first_line = false;
    }

    if !found {
        let newline: &[u8] = if input.windows(2).any(|window| window == b"\r\n") {
            b"\r\n"
        } else {
            b"\n"
        };
        if !output.is_empty() && !output.ends_with(b"\n") {
            output.extend_from_slice(newline);
        }
        output.extend_from_slice(COPYRIGHT_KEY);
        output.extend_from_slice(launch_text.as_bytes());
        output.extend_from_slice(newline);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TEXT: &str = "©Mojang AB / BMCBL , BLoader 0.2.77";

    #[test]
    fn rewrites_copyright_without_touching_other_keys() {
        let input = b"foo=bar\r\nmenu.copyright=\xC2\xA9Mojang AB\r\nbaz=qux\r\n";
        let output = rewrite_language_bytes(input, TEST_TEXT);
        let expected = format!("foo=bar\r\nmenu.copyright={TEST_TEXT}\r\nbaz=qux\r\n");
        assert_eq!(output, expected.as_bytes());
    }

    #[test]
    fn preserves_utf8_bom() {
        let mut input = UTF8_BOM.to_vec();
        input.extend_from_slice(b"menu.copyright=old\nother=value\n");
        let output = rewrite_language_bytes(&input, TEST_TEXT);
        assert!(output.starts_with(UTF8_BOM));
        assert!(String::from_utf8_lossy(&output).contains(TEST_TEXT));
    }

    #[test]
    fn appends_missing_copyright_key() {
        let input = b"foo=bar\nbaz=qux";
        let output = rewrite_language_bytes(input, TEST_TEXT);
        let expected = format!("foo=bar\nbaz=qux\nmenu.copyright={TEST_TEXT}\n");
        assert_eq!(output, expected.as_bytes());
    }

    #[test]
    fn matches_every_language_under_vanilla_texts() {
        let state = LaunchInfoState {
            texts_prefix: r"c:\games\minecraft\data\resource_packs\vanilla\texts\".to_string(),
            handles: RwLock::new(HashMap::new()),
            file_cache: RwLock::new(HashMap::new()),
        };
        assert!(is_target_language_path(
            &state,
            r"c:\games\minecraft\data\resource_packs\vanilla\texts\zh_cn.lang"
        ));
        assert!(is_target_language_path(
            &state,
            r"c:\games\minecraft\data\resource_packs\vanilla\texts\en_us.lang"
        ));
        assert!(!is_target_language_path(
            &state,
            r"c:\games\minecraft\data\resource_packs\vanilla\texts\languages.json"
        ));
    }

    #[test]
    fn strips_windows_device_prefix_for_matching() {
        assert_eq!(
            normalize_windows_path(r"\\?\C:\Games\Minecraft\data\resource_packs\vanilla\texts\ja_JP.lang"),
            r"c:\games\minecraft\data\resource_packs\vanilla\texts\ja_jp.lang"
        );
    }
}

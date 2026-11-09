// src/utils.rs
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Environment::SetCurrentDirectoryW;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

static LOADER_MODULE: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
struct VsFixedFileInfo {
    signature: u32,
    struct_version: u32,
    file_version_ms: u32,
    file_version_ls: u32,
    product_version_ms: u32,
    product_version_ls: u32,
    file_flags_mask: u32,
    file_flags: u32,
    file_os: u32,
    file_type: u32,
    file_subtype: u32,
    file_date_ms: u32,
    file_date_ls: u32,
}

#[link(name = "version")]
unsafe extern "system" {
    fn GetFileVersionInfoSizeW(filename: *const u16, handle: *mut u32) -> u32;
    fn GetFileVersionInfoW(
        filename: *const u16,
        handle: u32,
        len: u32,
        data: *mut c_void,
    ) -> i32;
    fn VerQueryValueW(
        block: *const c_void,
        sub_block: *const u16,
        value: *mut *mut c_void,
        len: *mut u32,
    ) -> i32;
}

pub fn set_loader_module_handle(module: usize) {
    LOADER_MODULE.store(module, Ordering::Release);
}

pub fn loader_module_handle() -> usize {
    LOADER_MODULE.load(Ordering::Acquire)
}

pub fn get_exe_path() -> PathBuf {
    get_module_path(0)
}

pub fn get_exe_directory() -> PathBuf {
    get_module_directory(0)
}

pub fn get_loader_directory() -> PathBuf {
    let handle = loader_module_handle();
    if handle != 0 {
        let dir = get_module_directory(handle);
        if dir.exists() {
            return dir;
        }
    }
    get_exe_directory()
}

pub fn get_module_path(module: usize) -> PathBuf {
    unsafe {
        let mut buffer = vec![0u16; 32_768];
        let handle = (module != 0).then_some(HMODULE(module as *mut _));
        let len = GetModuleFileNameW(handle, &mut buffer);
        if len > 0 {
            PathBuf::from(String::from_utf16_lossy(&buffer[..len as usize]))
        } else {
            PathBuf::new()
        }
    }
}

pub fn get_module_directory(module: usize) -> PathBuf {
    get_module_path(module)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Reads the Win32 fixed file version embedded in an executable or DLL.
pub fn read_file_version(path: &Path) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut ignored = 0u32;
        let size = GetFileVersionInfoSizeW(wide.as_ptr(), &mut ignored);
        if size == 0 {
            return None;
        }

        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(wide.as_ptr(), 0, size, data.as_mut_ptr() as *mut c_void) == 0 {
            return None;
        }

        let root_query = [b'\\' as u16, 0];
        let mut value: *mut c_void = std::ptr::null_mut();
        let mut value_len = 0u32;
        if VerQueryValueW(
            data.as_ptr() as *const c_void,
            root_query.as_ptr(),
            &mut value,
            &mut value_len,
        ) == 0
            || value.is_null()
            || value_len < std::mem::size_of::<VsFixedFileInfo>() as u32
        {
            return None;
        }

        let info = &*(value as *const VsFixedFileInfo);
        if info.signature != 0xFEEF04BD {
            return None;
        }

        Some(format!(
            "{}.{}.{}.{}",
            info.file_version_ms >> 16,
            info.file_version_ms & 0xFFFF,
            info.file_version_ls >> 16,
            info.file_version_ls & 0xFFFF
        ))
    }
}

pub fn current_application_version() -> Option<String> {
    read_file_version(&get_exe_path())
}

/// 将当前工作目录设置为 EXE 所在目录。
pub fn set_exe_cwd() -> bool {
    let dir = get_exe_directory();
    let wide_path: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe { SetCurrentDirectoryW(PCWSTR(wide_path.as_ptr())).as_bool() }
}

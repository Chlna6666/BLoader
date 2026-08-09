#![allow(non_snake_case)]

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::Environment::SetCurrentDirectoryW;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

/// Diagnostic 0.2.25 entrypoint.
///
/// The DLL is still loaded through Minecraft.Windows.exe's static PE import,
/// but PROCESS_ATTACH intentionally performs only one host-compatibility action:
/// set the process current directory to the directory containing the game EXE.
/// No BLoader runtime subsystem is initialized by this build.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(
    _hinstance: HINSTANCE,
    call_reason: u32,
    _reserved: *const c_void,
) -> i32 {
    if call_reason == DLL_PROCESS_ATTACH {
        set_executable_current_directory();
    }

    1
}

unsafe fn set_executable_current_directory() {
    // Keep DllMain allocation-free and silent. The only API calls are the same
    // basic Kernel32-style path operations needed to normalize the host CWD.
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

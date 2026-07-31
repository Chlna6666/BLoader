use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_SYSTEMMODAL, MB_TOPMOST,
    MessageBoxW,
};
use windows::core::{HSTRING, PCWSTR};

static FATAL_DIALOG_SHOWN: AtomicBool = AtomicBool::new(false);
static NATIVE_LOAD_ERROR_SHOWN: AtomicBool = AtomicBool::new(false);
static NATIVE_LOAD_SUCCESS_SHOWN: AtomicBool = AtomicBool::new(false);

pub fn show_fatal_error(title: &str, message: &str) {
    if FATAL_DIALOG_SHOWN
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let title = HSTRING::from(title);
    let message = HSTRING::from(message);
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST | MB_SYSTEMMODAL,
        );
    }
}

pub fn show_native_load_error(title: &str, message: &str) {
    if NATIVE_LOAD_ERROR_SHOWN
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let title = HSTRING::from(title);
    let message = HSTRING::from(message);
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST | MB_SYSTEMMODAL,
        );
    }
}

pub fn show_native_load_success(title: &str, message: &str) {
    if NATIVE_LOAD_SUCCESS_SHOWN
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let title = HSTRING::from(title);
    let message = HSTRING::from(message);
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST | MB_SYSTEMMODAL,
        );
    }
}

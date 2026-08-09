// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::{c_char, c_void};
use std::{
    ffi::CStr,
    ptr,
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};

use super::logging;

const DETECTION_UNKNOWN: u8 = 0;
const DETECTION_NATIVE_WINDOWS: u8 = 1;
const DETECTION_WINE: u8 = 2;
const MAX_DIAGNOSTIC_FIELD_CHARS: usize = 256;

const NTDLL_NAME: [u16; 10] = [
    b'n' as u16,
    b't' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];

const WINE_GET_VERSION: &[u8] = b"wine_get_version\0";
const WINE_GET_BUILD_ID: &[u8] = b"wine_get_build_id\0";
const WINE_GET_HOST_VERSION: &[u8] = b"wine_get_host_version\0";

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

type WineGetStringFn = unsafe extern "C" fn() -> *const c_char;
type WineGetHostVersionFn = unsafe extern "C" fn(*mut *const c_char, *mut *const c_char);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeKind {
    NativeWindows,
    Wine,
}

impl RuntimeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeWindows => "native-windows",
            Self::Wine => "wine",
        }
    }

    pub const fn is_wine(self) -> bool {
        matches!(self, Self::Wine)
    }

    const fn from_detection_code(code: u8) -> Self {
        if code == DETECTION_WINE {
            Self::Wine
        } else {
            Self::NativeWindows
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WineRuntimeInfo {
    pub version: Option<String>,
    pub build_id: Option<String>,
    pub host_system: Option<String>,
    pub host_release: Option<String>,
    pub proton: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEnvironment {
    NativeWindows,
    Wine(WineRuntimeInfo),
}

static EARLY_DETECTION: AtomicU8 = AtomicU8::new(DETECTION_UNKNOWN);
static RUNTIME_ENVIRONMENT: OnceLock<RuntimeEnvironment> = OnceLock::new();

/// Performs a loader-lock-safe Wine check without loading any new module.
///
/// Wine exposes `wine_get_version` from its already loaded `ntdll.dll`. Native
/// Windows does not export that symbol. Detailed strings and Proton heuristics
/// are intentionally deferred until the normal bootstrap thread is running.
pub fn prime_detection() -> RuntimeKind {
    let cached = EARLY_DETECTION.load(Ordering::Acquire);
    if cached != DETECTION_UNKNOWN {
        return RuntimeKind::from_detection_code(cached);
    }

    let detected = if wine_export_present() {
        DETECTION_WINE
    } else {
        DETECTION_NATIVE_WINDOWS
    };

    match EARLY_DETECTION.compare_exchange(
        DETECTION_UNKNOWN,
        detected,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => RuntimeKind::from_detection_code(detected),
        Err(existing) => RuntimeKind::from_detection_code(existing),
    }
}

pub fn current() -> &'static RuntimeEnvironment {
    RUNTIME_ENVIRONMENT.get_or_init(collect_runtime_environment)
}

pub fn log_summary() {
    match current() {
        RuntimeEnvironment::NativeWindows => {
            logging::info_message("Runtime environment: native-windows | wine=false");
        }
        RuntimeEnvironment::Wine(info) => {
            let compatibility = if info.proton { "proton" } else { "wine" };
            logging::info_message(&format!(
                "Runtime environment: wine | compatibility={} | version={} | build_id={} | host_system={} | host_release={}",
                compatibility,
                optional_field(&info.version),
                optional_field(&info.build_id),
                optional_field(&info.host_system),
                optional_field(&info.host_release),
            ));
        }
    }
}

fn collect_runtime_environment() -> RuntimeEnvironment {
    if !prime_detection().is_wine() {
        return RuntimeEnvironment::NativeWindows;
    }

    let module = ntdll_module();
    if module.is_null() {
        return RuntimeEnvironment::Wine(WineRuntimeInfo {
            version: None,
            build_id: None,
            host_system: None,
            host_release: None,
            proton: detect_proton(None, None),
        });
    }

    let version = call_string_export(module, WINE_GET_VERSION);
    let build_id = call_string_export(module, WINE_GET_BUILD_ID);
    let (host_system, host_release) = call_host_version_export(module);
    let proton = detect_proton(version.as_deref(), build_id.as_deref());

    RuntimeEnvironment::Wine(WineRuntimeInfo {
        version,
        build_id,
        host_system,
        host_release,
        proton,
    })
}

fn wine_export_present() -> bool {
    let module = ntdll_module();
    !module.is_null() && !find_export(module, WINE_GET_VERSION).is_null()
}

fn ntdll_module() -> *mut c_void {
    unsafe { GetModuleHandleW(NTDLL_NAME.as_ptr()) }
}

fn find_export(module: *mut c_void, name: &[u8]) -> *mut c_void {
    if module.is_null() || name.last().copied() != Some(0) {
        return ptr::null_mut();
    }
    unsafe { GetProcAddress(module, name.as_ptr()) }
}

fn call_string_export(module: *mut c_void, name: &[u8]) -> Option<String> {
    let address = find_export(module, name);
    if address.is_null() {
        return None;
    }

    let function: WineGetStringFn = unsafe { core::mem::transmute(address) };
    copy_diagnostic_string(unsafe { function() })
}

fn call_host_version_export(module: *mut c_void) -> (Option<String>, Option<String>) {
    let address = find_export(module, WINE_GET_HOST_VERSION);
    if address.is_null() {
        return (None, None);
    }

    let function: WineGetHostVersionFn = unsafe { core::mem::transmute(address) };
    let mut system = ptr::null();
    let mut release = ptr::null();
    unsafe {
        function(&mut system, &mut release);
    }
    (
        copy_diagnostic_string(system),
        copy_diagnostic_string(release),
    )
}

fn copy_diagnostic_string(value: *const c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }

    let text = unsafe { CStr::from_ptr(value) }.to_string_lossy();
    let sanitized = sanitize_field(&text);
    (!sanitized.is_empty()).then_some(sanitized)
}

fn sanitize_field(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(MAX_DIAGNOSTIC_FIELD_CHARS));
    let mut previous_was_space = false;

    for character in value.chars().take(MAX_DIAGNOSTIC_FIELD_CHARS) {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if character.is_whitespace() {
            if !previous_was_space && !output.is_empty() {
                output.push(' ');
            }
            previous_was_space = true;
        } else {
            output.push(character);
            previous_was_space = false;
        }
    }

    output.trim().to_string()
}

fn detect_proton(version: Option<&str>, build_id: Option<&str>) -> bool {
    const PROTON_ENVIRONMENT_VARIABLES: [&str; 3] = [
        "STEAM_COMPAT_DATA_PATH",
        "STEAM_COMPAT_CLIENT_INSTALL_PATH",
        "PROTON_VERSION",
    ];

    PROTON_ENVIRONMENT_VARIABLES
        .iter()
        .any(|name| std::env::var_os(name).is_some())
        || version.is_some_and(has_proton_marker)
        || build_id.is_some_and(has_proton_marker)
}

fn has_proton_marker(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("proton") || value.contains("steamplay")
}

fn optional_field(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_fields_remove_controls_and_collapse_whitespace() {
        assert_eq!(sanitize_field(" Wine\n\t10.0   build "), "Wine 10.0 build");
    }

    #[test]
    fn proton_markers_are_case_insensitive() {
        assert!(has_proton_marker("GE-Proton9-27"));
        assert!(has_proton_marker("SteamPlay compatibility tool"));
        assert!(!has_proton_marker("wine-10.0"));
    }
}

use std::ffi::c_void;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use minhook::MinHook;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{s, w};

use crate::config::Config;
use crate::runtime::foundation::logging;
use crate::utils;

/// Process-scoped override. A path may point to the DLL itself or to a directory.
/// Empty, `0`, `off`, `none`, `disable`, or `disabled` disables redirection for
/// the current process and overrides config.json.
pub const XGAMERUNTIME_PATH_ENV: &str = "BLOADER_XGAMERUNTIME_PATH";

const TARGET_DLL_NAME: &[u16] = &[
    b'x' as u16,
    b'g' as u16,
    b'a' as u16,
    b'm' as u16,
    b'e' as u16,
    b'r' as u16,
    b'u' as u16,
    b'n' as u16,
    b't' as u16,
    b'i' as u16,
    b'm' as u16,
    b'e' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
];

const STATUS_DLL_NOT_FOUND: i32 = 0xC000_0135u32 as i32;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

// ntdll!LdrLoadDll(PWSTR, PULONG, PUNICODE_STRING, PHANDLE)
type LdrLoadDllFn = unsafe extern "system" fn(
    search_path: *const u16,
    dll_characteristics: *mut u32,
    dll_name: *mut UnicodeString,
    dll_handle: *mut *mut c_void,
) -> i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteSource {
    Environment,
    Config,
}

impl RouteSource {
    fn label(self) -> &'static str {
        match self {
            Self::Environment => "process environment",
            Self::Config => "config.json",
        }
    }
}

struct RuntimeRoute {
    target: PathBuf,
    target_wide: Vec<u16>,
    source: RouteSource,
}

// RuntimeRoute is immutable after publication. The UTF-16 backing buffer stays
// alive for the lifetime of the process, so it is safe to pass to LdrLoadDll.

static ROUTE: OnceLock<RuntimeRoute> = OnceLock::new();
static ORIGINAL_LDR_LOAD_DLL: AtomicUsize = AtomicUsize::new(0);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static REDIRECT_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn install(config: &Config, exe_dir: &Path) -> bool {
    logging::scoped_info_message(
        "xgameruntime",
        &format!(
            "install entry | installed={} | route_published={} | exe_dir={}",
            INSTALLED.load(Ordering::Acquire),
            ROUTE.get().is_some(),
            exe_dir.display(),
        ),
    );

    if INSTALLED.load(Ordering::Acquire) {
        logging::scoped_info_message(
            "xgameruntime",
            "install skipped: hook is already active in this process.",
        );
        return true;
    }

    let selection = select_route(config, exe_dir);
    let Some((target, source)) = selection else {
        logging::scoped_info_message(
            "xgameruntime",
            "no redirect route selected; Windows loader behavior remains unchanged.",
        );
        return false;
    };

    logging::scoped_info_message(
        "xgameruntime",
        &format!(
            "route selected | source={} | target={} | exists={}",
            source.label(),
            target.display(),
            target.is_file(),
        ),
    );

    if !target.is_file() {
        let message = format!(
            "[xgameruntime] redirect target does not exist; system loading remains unchanged | source={} | target={}",
            source.label(),
            target.display()
        );
        if source == RouteSource::Environment {
            logging::warn_message(&message);
        } else {
            logging::info_message(&message);
        }
        return false;
    }

    if let Ok(module) = unsafe { GetModuleHandleW(w!("xgameruntime.dll")) } {
        let loaded_path = utils::get_module_path(module.0 as usize);
        logging::scoped_warn_message(
            "xgameruntime",
            &format!(
                "xgameruntime.dll is already loaded before BLoader routing | handle=0x{:X} | loaded_path={} | requested_target={}. LdrLoadDll cannot replace an existing/static import.",
                module.0 as usize,
                loaded_path.display(),
                target.display(),
            ),
        );
        return false;
    }

    logging::scoped_info_message(
        "xgameruntime",
        "preload check passed: xgameruntime.dll is not loaded yet.",
    );

    let mut target_wide: Vec<u16> = target.as_os_str().encode_wide().collect();
    if target_wide.len() >= (u16::MAX as usize / 2) {
        logging::warn_message(&format!(
            "[xgameruntime] redirect target path is too long: {}",
            target.display()
        ));
        return false;
    }
    target_wide.push(0);

    let route = RuntimeRoute {
        target,
        target_wide,
        source,
    };

    let target_proc = unsafe {
        let Ok(ntdll) = GetModuleHandleW(w!("ntdll.dll")) else {
            logging::warn_message("[xgameruntime] ntdll.dll is unavailable; redirect disabled.");
            return false;
        };
        let Some(proc) = GetProcAddress(ntdll, s!("LdrLoadDll")) else {
            logging::warn_message("[xgameruntime] ntdll!LdrLoadDll is unavailable; redirect disabled.");
            return false;
        };
        proc as *mut c_void
    };

    let original = match unsafe {
        MinHook::create_hook(target_proc, detour_ldr_load_dll as *mut c_void)
    } {
        Ok(original) => original,
        Err(error) => {
            logging::warn_message(&format!(
                "[xgameruntime] failed to create LdrLoadDll hook: {error:?}"
            ));
            return false;
        }
    };
    ORIGINAL_LDR_LOAD_DLL.store(original as usize, Ordering::Release);
    if ROUTE.set(route).is_err() {
        ORIGINAL_LDR_LOAD_DLL.store(0, Ordering::Release);
        let installed = INSTALLED.load(Ordering::Acquire);
        logging::scoped_warn_message(
            "xgameruntime",
            &format!(
                "route publication raced with another installer | installed={installed}"
            ),
        );
        return installed;
    }

    if let Err(error) = unsafe { MinHook::enable_all_hooks() } {
        ORIGINAL_LDR_LOAD_DLL.store(0, Ordering::Release);
        logging::warn_message(&format!(
            "[xgameruntime] failed to enable LdrLoadDll hook: {error:?}"
        ));
        return false;
    }

    INSTALLED.store(true, Ordering::Release);
    let route = ROUTE.get().expect("route was initialized before hook activation");
    logging::scoped_info_message(
        "xgameruntime",
        &format!(
            "process-local LdrLoadDll redirect armed | source={} | target={} | env={} has priority over config",
            route.source.label(),
            route.target.display(),
            XGAMERUNTIME_PATH_ENV
        ),
    );
    true
}

pub fn is_installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

pub fn redirect_count() -> u64 {
    REDIRECT_COUNT.load(Ordering::Acquire)
}

fn select_route(config: &Config, exe_dir: &Path) -> Option<(PathBuf, RouteSource)> {
    if let Some(raw) = std::env::var_os(XGAMERUNTIME_PATH_ENV) {
        let raw_text = raw.to_string_lossy();
        logging::scoped_info_message(
            "xgameruntime",
            &format!(
                "process environment override detected | {}={}",
                XGAMERUNTIME_PATH_ENV,
                raw_text,
            ),
        );
        if is_disabled_value(raw_text.trim()) {
            logging::info_message(&format!(
                "[xgameruntime] redirect disabled by process environment variable {}.",
                XGAMERUNTIME_PATH_ENV
            ));
            return None;
        }
        return Some((resolve_target_path(Path::new(&raw), exe_dir), RouteSource::Environment));
    }

    if !config.xgameruntime_redirection.enabled {
        logging::scoped_info_message(
            "xgameruntime",
            "redirect disabled by config.json.",
        );
        return None;
    }

    let configured = config.xgameruntime_redirection.path.trim();
    let configured = if configured.is_empty() { "." } else { configured };
    Some((
        resolve_target_path(Path::new(configured), exe_dir),
        RouteSource::Config,
    ))
}

fn resolve_target_path(value: &Path, exe_dir: &Path) -> PathBuf {
    let candidate = if value == Path::new(".") {
        exe_dir.to_path_buf()
    } else if value.is_absolute() {
        value.to_path_buf()
    } else {
        exe_dir.join(value)
    };

    if candidate
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("xgameruntime.dll"))
        || candidate
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
    {
        candidate
    } else {
        candidate.join("xgameruntime.dll")
    }
}

fn is_disabled_value(value: &str) -> bool {
    value.is_empty()
        || value.eq_ignore_ascii_case("0")
        || value.eq_ignore_ascii_case("off")
        || value.eq_ignore_ascii_case("none")
        || value.eq_ignore_ascii_case("disable")
        || value.eq_ignore_ascii_case("disabled")
}

unsafe extern "system" fn detour_ldr_load_dll(
    search_path: *const u16,
    dll_characteristics: *mut u32,
    dll_name: *mut UnicodeString,
    dll_handle: *mut *mut c_void,
) -> i32 {
    let original_address = ORIGINAL_LDR_LOAD_DLL.load(Ordering::Acquire);
    if original_address == 0 {
        return STATUS_DLL_NOT_FOUND;
    }
    let original: LdrLoadDllFn = unsafe { mem::transmute(original_address) };

    let Some(route) = ROUTE.get() else {
        return unsafe { original(search_path, dll_characteristics, dll_name, dll_handle) };
    };

    if unsafe { is_bare_xgameruntime_request(dll_name) } {
        let byte_length = (route.target_wide.len().saturating_sub(1) * 2) as u16;
        let mut redirected_name = UnicodeString {
            length: byte_length,
            maximum_length: byte_length.saturating_add(2),
            buffer: route.target_wide.as_ptr() as *mut u16,
        };
        REDIRECT_COUNT.fetch_add(1, Ordering::Relaxed);
        return unsafe {
            original(
                search_path,
                dll_characteristics,
                &mut redirected_name,
                dll_handle,
            )
        };
    }

    unsafe { original(search_path, dll_characteristics, dll_name, dll_handle) }
}

unsafe fn is_bare_xgameruntime_request(name: *const UnicodeString) -> bool {
    if name.is_null() {
        return false;
    }
    let name = unsafe { &*name };
    if name.buffer.is_null() || name.length == 0 || name.length % 2 != 0 {
        return false;
    }

    let len = (name.length / 2) as usize;
    let value = unsafe { std::slice::from_raw_parts(name.buffer, len) };

    // Only redirect a bare import name. Absolute or relative paths are passed
    // through untouched so a proxy DLL can load a native runtime by absolute
    // path without recursively loading itself.
    if value.iter().any(|unit| *unit == b'\\' as u16 || *unit == b'/' as u16 || *unit == b':' as u16) {
        return false;
    }

    utf16_ascii_eq_ignore_case(value, TARGET_DLL_NAME)
}

fn utf16_ascii_eq_ignore_case(left: &[u16], right: &[u16]) -> bool {
    fn lower_ascii(value: u16) -> u16 {
        if (b'A' as u16..=b'Z' as u16).contains(&value) {
            value + 32
        } else {
            value
        }
    }

    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| lower_ascii(*left) == lower_ascii(*right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_directory_resolves_beside_executable() {
        let exe_dir = Path::new(r"C:\Games\Minecraft");
        assert_eq!(
            resolve_target_path(Path::new("."), exe_dir),
            PathBuf::from(r"C:\Games\Minecraft\xgameruntime.dll")
        );
    }

    #[test]
    fn explicit_dll_path_is_not_modified() {
        let exe_dir = Path::new(r"C:\Games\Minecraft");
        assert_eq!(
            resolve_target_path(Path::new(r"D:\Accounts\Alice\xgameruntime.dll"), exe_dir),
            PathBuf::from(r"D:\Accounts\Alice\xgameruntime.dll")
        );
    }

    #[test]
    fn disabled_values_are_case_insensitive() {
        assert!(is_disabled_value("OFF"));
        assert!(is_disabled_value("disabled"));
        assert!(!is_disabled_value(r"D:\Runtime"));
    }

    #[test]
    fn request_match_rejects_paths() {
        assert!(utf16_ascii_eq_ignore_case(
            &"XGameRuntime.dll".encode_utf16().collect::<Vec<_>>(),
            TARGET_DLL_NAME
        ));
        assert!(!utf16_ascii_eq_ignore_case(
            &r"native\xgameruntime.dll".encode_utf16().collect::<Vec<_>>(),
            TARGET_DLL_NAME
        ));
    }
}

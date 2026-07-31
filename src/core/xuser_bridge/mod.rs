// SPDX-License-Identifier: GPL-3.0-or-later
//
// Only Microsoft xgameruntime.dll!QueryApiImpl is intercepted. Every other
// XGameRuntime export and every non-XUser runtime class stays in the official
// Microsoft implementation.

#[cfg(not(target_arch = "x86_64"))]
compile_error!("BLoader's embedded XUser bridge currently supports Windows x64 only");

mod abi;
mod crypto;
mod ipc;
mod token;
mod xasync;
mod xuser;

use core::ffi::c_void;
use minhook::MinHook;
use std::{mem, ptr, sync::OnceLock};

use abi::{CLSID_XUSER_IMPL, E_POINTER, Guid, HResult, QueryApiImplFn};
use ipc::Session;

use crate::runtime::foundation::logging;

static SESSION: OnceLock<Session> = OnceLock::new();
static ORIGINAL_QUERY_API: OnceLock<usize> = OnceLock::new();

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

pub fn initialize_before_mods() {
    if SESSION.get().is_some() {
        return;
    }

    let candidate = match ipc::receive_session() {
        Ok(Some(session)) => session,
        Ok(None) => {
            bridge_info(
                "未检测到 BMCBL 安全一次性管道；不安装 QueryApiImpl Hook，继续使用微软官方 XUser 登录",
            );
            return;
        }
        Err(error) => {
            bridge_warn(&format!(
                "BMCBL 安全会话验证失败；不安装 QueryApiImpl Hook，继续使用微软官方 XUser 登录 | reason={error}"
            ));
            return;
        }
    };

    // Gamertag is public profile data and is useful for confirming that BMCBL
    // delivered the intended account. Remove control characters and bound the
    // length before it reaches any text log to prevent log injection.
    let gamertag = sanitize_gamertag(&candidate.gamertag);

    match install_hook(candidate) {
        Ok(()) => bridge_info(&format!(
            "已接收 BMCBL 安全一次性管道会话；仅接管官方 QueryApiImpl | xbox_gamertag={gamertag}"
        )),
        Err(error) => bridge_error(&format!(
            "QueryApiImpl Hook 安装失败；自定义 XUser 已停用，继续使用微软官方 XUser 登录 | reason={error}"
        )),
    }
}

fn install_hook(session: Session) -> Result<(), String> {
    // BLoader and xgameruntime are both static imports of the Win32 game. Do
    // not LoadLibrary from DllMain: if the official runtime has not already
    // been mapped, fail closed and leave the original login path untouched.
    let module_name = wide("xgameruntime.dll");
    let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    if module.is_null() {
        return Err("official xgameruntime.dll is not mapped yet".to_string());
    }
    let target = unsafe { GetProcAddress(module, b"QueryApiImpl\0".as_ptr()) };
    if target.is_null() {
        return Err("official xgameruntime.dll does not export QueryApiImpl".to_string());
    }

    let trampoline = unsafe {
        MinHook::create_hook(target, query_api_hook as *const () as *mut c_void)
    }
    .map_err(|status| format!("MinHook create failed: {status:?}"))?;
    ORIGINAL_QUERY_API
        .set(trampoline as usize)
        .map_err(|_| "official QueryApiImpl trampoline was already installed".to_string())?;
    SESSION
        .set(session)
        .map_err(|_| "XUser session was already installed".to_string())?;
    unsafe { MinHook::enable_all_hooks() }
        .map_err(|status| format!("MinHook enable failed: {status:?}"))?;
    Ok(())
}

pub(crate) fn session() -> Option<&'static Session> {
    SESSION.get()
}

pub(crate) unsafe fn call_original_query(
    runtime_class_id: *const Guid,
    interface_id: *const Guid,
    out: *mut *mut c_void,
) -> HResult {
    let Some(address) = ORIGINAL_QUERY_API.get().copied() else {
        return abi::E_FAIL;
    };
    let function: QueryApiImplFn = unsafe { mem::transmute(address) };
    unsafe { function(runtime_class_id, interface_id, out) }
}

unsafe extern "system" fn query_api_hook(
    runtime_class_id: *const Guid,
    interface_id: *const Guid,
    out: *mut *mut c_void,
) -> HResult {
    if runtime_class_id.is_null() || interface_id.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe {
        out.write(ptr::null_mut());
    }

    if unsafe { *runtime_class_id } == CLSID_XUSER_IMPL {
        return unsafe { xuser::query_interface(interface_id, out) };
    }
    unsafe { call_original_query(runtime_class_id, interface_id, out) }
}

fn sanitize_gamertag(value: &str) -> String {
    let sanitized = value
        .chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect::<String>();
    let sanitized = sanitized.trim();
    if sanitized.is_empty() {
        "<unknown>".to_string()
    } else {
        sanitized.to_string()
    }
}

fn bridge_info(message: &str) {
    if logging::is_ready() {
        logging::scoped_info_message("xuser-bridge", message);
    } else {
        logging::write_bootstrap_marker(&format!("xuser-bridge.info {message}"));
    }
}

fn bridge_warn(message: &str) {
    if logging::is_ready() {
        logging::scoped_warn_message("xuser-bridge", message);
    } else {
        logging::write_bootstrap_marker(&format!("xuser-bridge.warn {message}"));
    }
}

fn bridge_error(message: &str) {
    if logging::is_ready() {
        logging::scoped_error_message("xuser-bridge", message);
    } else {
        logging::write_bootstrap_marker(&format!("xuser-bridge.error {message}"));
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::sanitize_gamertag;

    #[test]
    fn gamertag_log_value_removes_control_characters() {
        assert_eq!(sanitize_gamertag(" Civil\r\nRelic\t4341 "), "CivilRelic4341");
    }

    #[test]
    fn gamertag_log_value_is_bounded() {
        assert_eq!(sanitize_gamertag(&"a".repeat(80)).len(), 64);
    }
}

// SPDX-License-Identifier: GPL-3.0-only
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
use std::{
    mem,
    path::{Path, PathBuf},
    ptr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use abi::{CLSID_XUSER_IMPL, E_POINTER, Guid, HResult, QueryApiImplFn};
use ipc::Session;

use crate::runtime::foundation::logging;

const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
const XUSER_BRIDGE_PROTOCOL: u32 = 1;

static SESSION: OnceLock<Session> = OnceLock::new();
static ORIGINAL_QUERY_API: OnceLock<usize> = OnceLock::new();
static QUERY_HOOK_FIRST_CALL: AtomicBool = AtomicBool::new(false);
static XUSER_QUERY_FIRST_CALL: AtomicBool = AtomicBool::new(false);
static PENDING_LOGS: OnceLock<Mutex<Vec<PendingLog>>> = OnceLock::new();

#[derive(Clone, Copy)]
enum BridgeLogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

struct PendingLog {
    level: BridgeLogLevel,
    message: String,
}

#[derive(Debug)]
struct HookInstallReport {
    runtime_path: String,
    runtime_source: &'static str,
    query_api_address: usize,
    trampoline_address: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn LoadLibraryExW(file_name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetModuleFileNameW(module: *mut c_void, file_name: *mut u16, size: u32) -> u32;
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
}

pub fn initialize_before_mods() {
    bridge_info(&format!(
        "XUser Bridge 入口已执行 | protocol={} | mode=pipe-gated | hook=QueryApiImpl-only",
        XUSER_BRIDGE_PROTOCOL
    ));

    if SESSION.get().is_some() {
        bridge_warn("XUser Bridge 已存在活动会话；跳过重复初始化");
        return;
    }

    bridge_debug("开始探测进程专属 BMCBL XUser named pipe；不存在时保持微软官方 XUser 原样");
    let candidate = match ipc::receive_session() {
        Ok(Some(session)) => session,
        Ok(None) => {
            bridge_info(&official_runtime_passthrough_message());
            return;
        }
        Err(error) => {
            bridge_warn(&format!(
                "BMCBL 安全会话验证失败；不安装 QueryApiImpl Hook；系统原生 xgameruntime.dll 将按游戏正常流程启动，继续使用微软官方 XUser 登录 | reason={error}"
            ));
            return;
        }
    };

    // Gamertag and XUID are public Xbox identity metadata. Secret bearer tokens,
    // UHS values and the signing private key are never written to diagnostics.
    let gamertag = sanitize_gamertag(&candidate.gamertag);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let token_routes = candidate
        .tokens
        .iter()
        .map(|token| {
            format!(
                "{}:{}s",
                token.relying_party,
                token.expires_at.saturating_sub(now)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    bridge_info(&format!(
        "已从 BMCBL 安全一次性管道接收并验证 Xbox 会话 | xbox_gamertag={gamertag} | xbox_xuid={} | token_routes={} | privilege_count={} | secrets_logged=false | next=load-official-runtime-and-hook",
        candidate.xuid,
        candidate.tokens.len(),
        candidate.privileges.len(),
    ));
    bridge_debug(&format!(
        "XUser token 路由已装载 | routes=[{token_routes}] | token_body=redacted | uhs=redacted | signing_key=redacted"
    ));

    match install_hook(candidate) {
        Ok(report) => bridge_info(&format!(
            "XUser Bridge 已启用；仅接管官方 QueryApiImpl | xbox_gamertag={gamertag} | native_runtime_source={} | native_runtime_path={} | QueryApiImpl=0x{:X} | trampoline=0x{:X}",
            report.runtime_source,
            report.runtime_path,
            report.query_api_address,
            report.trampoline_address,
        )),
        Err(error) => bridge_error(&format!(
            "QueryApiImpl Hook 安装失败；自定义 XUser 已停用；系统原生 xgameruntime.dll 保持原样，继续使用微软官方 XUser 登录 | reason={error}"
        )),
    }
}

/// Replays XUser bridge diagnostics generated before the normal tracing and
/// console pipeline became available. The bridge now initializes in the OEP-gated
/// pre-main phase after the Windows loader lock has been released.
pub fn publish_pending_logs() {
    let Some(queue) = PENDING_LOGS.get() else {
        return;
    };
    let pending = {
        let mut guard = match queue.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        mem::take(&mut *guard)
    };

    if pending.is_empty() {
        return;
    }

    logging::scoped_info_message(
        "xuser-bridge",
        &format!(
            "正在回放 {} 条早期 XUser Bridge 诊断；以下状态生成于 BLoader pre-main 阶段（loader lock 已释放）",
            pending.len()
        ),
    );
    for entry in pending {
        match entry.level {
            BridgeLogLevel::Debug => logging::scoped_debug_message("xuser-bridge", &entry.message),
            BridgeLogLevel::Info => logging::scoped_info_message("xuser-bridge", &entry.message),
            BridgeLogLevel::Warn => logging::scoped_warn_message("xuser-bridge", &entry.message),
            BridgeLogLevel::Error => logging::scoped_error_message("xuser-bridge", &entry.message),
        }
    }
}

fn install_hook(session: Session) -> Result<HookInstallReport, String> {
    let module_name = wide("xgameruntime.dll");
    let mut module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    let runtime_source = if module.is_null() {
        bridge_info(
            "已认证 BMCBL 会话，但系统原生 xgameruntime.dll 尚未映射；正在从 System32 同步加载官方 Runtime",
        );
        module = unsafe {
            LoadLibraryExW(
                module_name.as_ptr(),
                ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err("failed to load official xgameruntime.dll from System32".to_string());
        }
        "bloader-system32-preload"
    } else {
        "host-preloaded"
    };

    let runtime_path = module_path(module)?;
    verify_system_runtime_path(&runtime_path)?;
    bridge_info(&format!(
        "系统原生 xgameruntime.dll 已就绪 | source={runtime_source} | path={runtime_path}"
    ));

    let target = unsafe { GetProcAddress(module, b"QueryApiImpl\0".as_ptr()) };
    if target.is_null() {
        return Err(format!(
            "official xgameruntime.dll does not export QueryApiImpl | path={runtime_path}"
        ));
    }
    bridge_info(&format!(
        "已定位系统原生 QueryApiImpl | address=0x{:X}",
        target as usize
    ));

    let trampoline =
        unsafe { MinHook::create_hook(target, query_api_hook as *const () as *mut c_void) }
            .map_err(|status| format!("MinHook create failed: {status:?}"))?;
    ORIGINAL_QUERY_API
        .set(trampoline as usize)
        .map_err(|_| "official QueryApiImpl trampoline was already installed".to_string())?;
    SESSION
        .set(session)
        .map_err(|_| "XUser session was already installed".to_string())?;
    unsafe { MinHook::enable_all_hooks() }
        .map_err(|status| format!("MinHook enable failed: {status:?}"))?;

    Ok(HookInstallReport {
        runtime_path,
        runtime_source,
        query_api_address: target as usize,
        trampoline_address: trampoline as usize,
    })
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
        bridge_error("QueryApiImpl trampoline 不可用；无法回退到微软官方 Runtime");
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
    if !QUERY_HOOK_FIRST_CALL.swap(true, Ordering::AcqRel) {
        bridge_info("QueryApiImpl Hook 已首次命中；BLoader 拦截链路正在工作");
    }

    if runtime_class_id.is_null() || interface_id.is_null() || out.is_null() {
        bridge_warn("QueryApiImpl 收到空指针参数；返回 E_POINTER");
        return E_POINTER;
    }
    unsafe {
        out.write(ptr::null_mut());
    }

    if unsafe { *runtime_class_id } == CLSID_XUSER_IMPL {
        if !XUSER_QUERY_FIRST_CALL.swap(true, Ordering::AcqRel) {
            let gamertag = session()
                .map(|session| sanitize_gamertag(&session.gamertag))
                .unwrap_or_else(|| "<unknown>".to_string());
            bridge_info(&format!(
                "QueryApiImpl 已请求 CLSID_XUserImpl；返回 BLoader 内置 Rust XUser | xbox_gamertag={gamertag}"
            ));
        } else {
            bridge_debug("QueryApiImpl route=embedded-XUser");
        }
        return unsafe { xuser::query_interface(interface_id, out) };
    }

    bridge_debug("QueryApiImpl route=official-runtime | class=non-XUser");
    unsafe { call_original_query(runtime_class_id, interface_id, out) }
}

fn official_runtime_passthrough_message() -> String {
    let module_name = wide("xgameruntime.dll");
    let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    if module.is_null() {
        "XUser Bridge 入口已执行；未检测到 BMCBL 安全一次性管道；不主动加载系统 Runtime、不安装 QueryApiImpl Hook；系统原生 xgameruntime.dll 将由游戏按微软正常流程启动，继续使用官方 XUser 登录".to_string()
    } else {
        let path = module_path(module).unwrap_or_else(|_| "<path-unavailable>".to_string());
        format!(
            "XUser Bridge 入口已执行；未检测到 BMCBL 安全一次性管道；不安装 QueryApiImpl Hook；系统原生 xgameruntime.dll 已由宿主加载并保持原样，继续使用官方 XUser 登录 | path={path}"
        )
    }
}

fn module_path(module: *mut c_void) -> Result<String, String> {
    let mut buffer = vec![0u16; 32_768];
    let written = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) };
    if written == 0 || written as usize >= buffer.len() {
        return Err("unable to resolve official xgameruntime.dll path".to_string());
    }
    buffer.truncate(written as usize);
    Ok(String::from_utf16_lossy(&buffer))
}

fn verify_system_runtime_path(actual: &str) -> Result<(), String> {
    let expected = system_runtime_path()?;
    if !same_windows_path(Path::new(actual), &expected) {
        return Err(format!(
            "refusing to hook a non-System32 xgameruntime.dll | actual={actual} | expected={}",
            expected.display()
        ));
    }
    Ok(())
}

fn system_runtime_path() -> Result<PathBuf, String> {
    let mut buffer = vec![0u16; 32_768];
    let written = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if written == 0 || written as usize >= buffer.len() {
        return Err("unable to resolve Windows System32 directory".to_string());
    }
    buffer.truncate(written as usize);
    Ok(PathBuf::from(String::from_utf16_lossy(&buffer)).join("xgameruntime.dll"))
}

fn same_windows_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .eq_ignore_ascii_case(right.to_string_lossy().trim_start_matches(r"\\?\"))
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

fn queue_pending(level: BridgeLogLevel, message: &str) {
    let queue = PENDING_LOGS.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = match queue.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.push(PendingLog {
        level,
        message: message.to_string(),
    });
}

pub(crate) fn bridge_debug(message: &str) {
    logging::write_bootstrap_marker(&format!("xuser-bridge.debug {message}"));
    if logging::is_ready() {
        logging::scoped_debug_message("xuser-bridge", message);
    } else {
        queue_pending(BridgeLogLevel::Debug, message);
    }
}

pub(crate) fn bridge_info(message: &str) {
    logging::write_bootstrap_marker(&format!("xuser-bridge.info {message}"));
    if logging::is_ready() {
        logging::scoped_info_message("xuser-bridge", message);
    } else {
        queue_pending(BridgeLogLevel::Info, message);
    }
}

pub(crate) fn bridge_warn(message: &str) {
    logging::write_bootstrap_marker(&format!("xuser-bridge.warn {message}"));
    if logging::is_ready() {
        logging::scoped_warn_message("xuser-bridge", message);
    } else {
        queue_pending(BridgeLogLevel::Warn, message);
    }
}

pub(crate) fn bridge_error(message: &str) {
    logging::write_bootstrap_marker(&format!("xuser-bridge.error {message}"));
    if logging::is_ready() {
        logging::scoped_error_message("xuser-bridge", message);
    } else {
        queue_pending(BridgeLogLevel::Error, message);
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{same_windows_path, sanitize_gamertag};

    #[test]
    fn gamertag_log_value_removes_control_characters() {
        assert_eq!(
            sanitize_gamertag(" Civil\r\nRelic\t4341 "),
            "CivilRelic4341"
        );
    }

    #[test]
    fn gamertag_log_value_is_bounded() {
        assert_eq!(sanitize_gamertag(&"a".repeat(80)).len(), 64);
    }

    #[test]
    fn system_runtime_path_comparison_is_case_insensitive() {
        assert!(same_windows_path(
            Path::new(r"C:\Windows\System32\xgameruntime.dll"),
            Path::new(r"c:\windows\system32\XGameRuntime.DLL"),
        ));
    }

    #[test]
    fn system_runtime_path_comparison_accepts_extended_prefix() {
        assert!(same_windows_path(
            Path::new(r"\\?\C:\Windows\System32\xgameruntime.dll"),
            Path::new(r"C:\Windows\System32\xgameruntime.dll"),
        ));
    }
}

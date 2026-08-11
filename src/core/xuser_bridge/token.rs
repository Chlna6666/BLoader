// SPDX-License-Identifier: GPL-3.0-only

#[path = "pre_xsts.rs"]
mod pre_xsts;

use core::ffi::{c_char, c_void};
use std::{
    collections::HashSet,
    ffi::{CStr, CString},
    mem, ptr,
    sync::{Mutex, OnceLock},
};

use super::{
    abi::{
        E_FAIL, E_INVALIDARG, HResult, TokenData, TokenHeader, TokenUtf16Data, TokenUtf16Header,
        XAsyncBlock, XUserHandle,
    },
    bridge_info, bridge_warn, session, xuser,
};

const MAX_URL_BYTES: usize = 32 * 1024;

const XBOX_LIVE_RP: &str = "http://xboxlive.com";
const PLAYFAB_RP: &str = "https://b980a380.minecraft.playfabapi.com/";
const MULTIPLAYER_RP: &str = "https://multiplayer.minecraft.net/";
const REALMS_RP: &str = "https://pocket.realms.minecraft.net/";
const LICENSING_RP: &str = "http://licensing.xboxlive.com";

static ROUTE_LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static DIAGNOSTIC_PROBES_STARTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

const XUSER_ADD_DEFAULT_USER_SILENTLY: u32 = 0x01;

macro_rules! hresult_try {
    ($expression:expr) => {
        match $expression {
            Ok(value) => value,
            Err(error) => return error,
        }
    };
}

fn log_once(key: String, message: impl FnOnce()) {
    let inserted = ROUTE_LOGGED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key);
    if inserted {
        message();
    }
}

fn native_token_interface() -> Result<*mut c_void, HResult> {
    xuser::native_base_interface().ok_or(E_FAIL)
}

fn native_token_slot(index: usize) -> Result<usize, HResult> {
    xuser::native_base_slot(index).ok_or(E_FAIL)
}

fn diagnostic_probe_once(key: String) -> bool {
    DIAGNOSTIC_PROBES_STARTED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key)
}

struct NativeAddProbeContext {
    relying_party: String,
    url: String,
    options: u32,
}

struct NativeTokenProbeContext {
    relying_party: String,
    native_user: XUserHandle,
    native_xuid: u64,
    method: CString,
    url: CString,
}

fn native_add_result(async_block: *mut XAsyncBlock, user: *mut XUserHandle) -> HResult {
    let Ok(interface) = native_token_interface() else {
        return E_FAIL;
    };
    let Ok(slot) = native_token_slot(8) else {
        return E_FAIL;
    };
    type Function =
        unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut XUserHandle) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, async_block, user) }
}

fn native_user_id(user: XUserHandle) -> Result<u64, HResult> {
    if user.is_null() {
        return Err(E_INVALIDARG);
    }
    let interface = native_token_interface()?;
    let slot = native_token_slot(11)?;
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle, *mut u64) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    let mut xuid = 0u64;
    let status = unsafe { function(interface, user, &mut xuid) };
    if status < 0 || xuid == 0 {
        return Err(if status < 0 { status } else { E_FAIL });
    }
    Ok(xuid)
}

fn close_native_user(user: XUserHandle) {
    if user.is_null() {
        return;
    }
    let (Ok(interface), Ok(slot)) = (native_token_interface(), native_token_slot(4)) else {
        return;
    };
    type Function = unsafe extern "system" fn(*mut c_void, XUserHandle);
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, user) };
}

fn start_pre_xsts_diagnostic_probe(relying_party: &str, url: &str, options: u32) {
    let key = format!("{relying_party}|{url}");
    if !diagnostic_probe_once(key) {
        return;
    }

    let (Ok(interface), Ok(slot)) = (native_token_interface(), native_token_slot(7)) else {
        bridge_warn(&format!(
            "pre-XSTS builder 诊断无法启动 native Add；Microsoft Runtime XUserAddAsync 不可用 | rp={relying_party} | result=no-native-provider | action=diagnostic-skip"
        ));
        return;
    };

    let context = Box::new(NativeAddProbeContext {
        relying_party: relying_party.to_string(),
        url: url.to_string(),
        options,
    });
    let context_ptr = Box::into_raw(context);

    let block = Box::new(XAsyncBlock {
        queue: ptr::null_mut(),
        context: context_ptr.cast(),
        callback: Some(native_add_probe_complete),
        internal: [0; 4],
    });
    let block_ptr = Box::into_raw(block);

    type Function = unsafe extern "system" fn(*mut c_void, u32, *mut XAsyncBlock) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    let result = unsafe { function(interface, XUSER_ADD_DEFAULT_USER_SILENTLY, block_ptr) };
    if result < 0 {
        unsafe {
            drop(Box::from_raw(context_ptr));
            drop(Box::from_raw(block_ptr));
        }
        bridge_warn(&format!(
            "pre-XSTS builder 诊断 native Add 启动失败；不会触发系统登录 UI | rp={relying_party} | result=0x{:08X} | action=diagnostic-skip",
            result as u32
        ));
        return;
    }

    bridge_info(&format!(
        "pre-XSTS builder 诊断 native Add 已启动 | rp={relying_party} | mode=silent-diagnostic | system_login_ui=not-invoked | result_discarded=true | secrets_logged=false"
    ));
}

unsafe extern "system" fn native_add_probe_complete(async_block: *mut XAsyncBlock) {
    if async_block.is_null() {
        return;
    }

    let context_ptr = unsafe { (*async_block).context }.cast::<NativeAddProbeContext>();
    if context_ptr.is_null() {
        unsafe { drop(Box::from_raw(async_block)) };
        return;
    }

    let context = unsafe { Box::from_raw(context_ptr) };
    let NativeAddProbeContext {
        relying_party,
        url,
        options,
    } = *context;

    let mut native_user = ptr::null_mut();
    let result = native_add_result(async_block, &mut native_user);
    unsafe { drop(Box::from_raw(async_block)) };

    if result < 0 || native_user.is_null() {
        bridge_warn(&format!(
            "pre-XSTS builder 诊断 native Add 未返回用户；无系统账号或 Runtime 拒绝 silent Add | rp={relying_party} | result=0x{:08X} | action=diagnostic-skip",
            result as u32
        ));
        return;
    }

    let native_xuid = match native_user_id(native_user) {
        Ok(value) => value,
        Err(status) => {
            close_native_user(native_user);
            bridge_warn(&format!(
                "pre-XSTS builder 诊断 native 用户身份无法读取；已关闭诊断句柄 | rp={relying_party} | result=0x{:08X} | action=diagnostic-skip",
                status as u32
            ));
            return;
        }
    };

    bridge_info(&format!(
        "pre-XSTS builder 诊断 native 用户已建立 | rp={relying_party} | native_xuid={native_xuid} | purpose=trigger-builder-probe-only | result_discarded=true | secrets_logged=false"
    ));

    start_native_token_diagnostic_probe(relying_party, url, options, native_user, native_xuid);
}

fn start_native_token_diagnostic_probe(
    relying_party: String,
    url: String,
    options: u32,
    native_user: XUserHandle,
    native_xuid: u64,
) {
    let (Ok(interface), Ok(slot)) = (native_token_interface(), native_token_slot(23)) else {
        close_native_user(native_user);
        bridge_warn(&format!(
            "pre-XSTS builder 诊断 native token 请求无法启动；Token slot 不可用 | rp={relying_party} | native_xuid={native_xuid} | action=diagnostic-skip"
        ));
        return;
    };

    let Ok(url) = CString::new(url) else {
        close_native_user(native_user);
        bridge_warn(&format!(
            "pre-XSTS builder 诊断 URL 包含非法 NUL；跳过 native token 请求 | rp={relying_party} | native_xuid={native_xuid} | action=diagnostic-skip"
        ));
        return;
    };
    let method = CString::new("GET").expect("static diagnostic method has no interior NUL");

    let context = Box::new(NativeTokenProbeContext {
        relying_party,
        native_user,
        native_xuid,
        method,
        url,
    });
    let method_ptr = context.method.as_ptr();
    let url_ptr = context.url.as_ptr();
    let relying_party_for_log = context.relying_party.clone();
    let context_ptr = Box::into_raw(context);

    let block = Box::new(XAsyncBlock {
        queue: ptr::null_mut(),
        context: context_ptr.cast(),
        callback: Some(native_token_probe_complete),
        internal: [0; 4],
    });
    let block_ptr = Box::into_raw(block);

    type Function = unsafe extern "system" fn(
        *mut c_void,
        XUserHandle,
        u32,
        *const c_char,
        *const c_char,
        usize,
        *const TokenHeader,
        usize,
        *const c_void,
        *mut XAsyncBlock,
    ) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    let result = unsafe {
        function(
            interface,
            native_user,
            options,
            method_ptr,
            url_ptr,
            0,
            ptr::null(),
            0,
            ptr::null(),
            block_ptr,
        )
    };
    if result < 0 {
        unsafe {
            drop(Box::from_raw(context_ptr));
            drop(Box::from_raw(block_ptr));
        }
        close_native_user(native_user);
        bridge_warn(&format!(
            "pre-XSTS builder 诊断 native token 请求启动失败；已关闭诊断 native 句柄 | rp={relying_party_for_log} | native_xuid={native_xuid} | result=0x{:08X} | result_returned_to_minecraft=false | secrets_logged=false",
            result as u32
        ));
        return;
    }

    bridge_info(&format!(
        "pre-XSTS builder 诊断 native token 请求已启动 | rp={relying_party_for_log} | native_xuid={native_xuid} | mode=trigger-builder-probe-only | result_returned_to_minecraft=false | result_discarded=true | secrets_logged=false"
    ));
}

unsafe extern "system" fn native_token_probe_complete(async_block: *mut XAsyncBlock) {
    if async_block.is_null() {
        return;
    }

    let context_ptr = unsafe { (*async_block).context }.cast::<NativeTokenProbeContext>();
    if context_ptr.is_null() {
        unsafe { drop(Box::from_raw(async_block)) };
        return;
    }

    let context = unsafe { Box::from_raw(context_ptr) };
    let mut result_size = 0usize;
    let size_status = native_token_result_size(async_block, &mut result_size);
    unsafe { drop(Box::from_raw(async_block)) };

    bridge_info(&format!(
        "pre-XSTS builder 诊断 native token 请求完成并丢弃结果 | rp={} | native_xuid={} | result_size_status=0x{:08X} | result_size={} | result_returned_to_minecraft=false | token_body_read=false | secrets_logged=false",
        context.relying_party,
        context.native_xuid,
        size_status as u32,
        result_size,
    ));

    close_native_user(context.native_user);
}

fn native_token_result_size(async_block: *mut XAsyncBlock, size: *mut usize) -> HResult {
    let (Ok(interface), Ok(slot)) = (native_token_interface(), native_token_slot(24)) else {
        return E_FAIL;
    };
    type Function = unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut usize) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, async_block, size) }
}

/// Selects an official Microsoft user only when Windows already exposes the
/// exact same XUID as the BMCBL session. The public XUser handle remains the
/// synthetic BMCBL object and is never passed into the official Runtime.
///
/// Different-account and no-system-account cases intentionally share the same
/// path: neither is allowed to borrow an unrelated native user's final XSTS.
fn official_user_for_request(relying_party: &str, url: &str, options: u32) -> Result<XUserHandle, HResult> {
    let runtime = session().ok_or(E_FAIL)?;
    if runtime.custom_user_token().is_none() {
        bridge_warn(&format!(
            "BMCBL UToken 不可用；拒绝 Xbox Token 请求 | rp={relying_party} | custom_xuid={} | action=fail-closed",
            runtime.xuid
        ));
        return Err(E_FAIL);
    }

    if let Some(native_user) = xuser::native_user_for_custom_identity() {
        let key = format!("native:{relying_party}");
        log_once(key, || {
            bridge_info(&format!(
                "BMCBL 账号存在同 XUID 的系统 native XUser；使用 Microsoft Runtime 官方 Token 快速路径 | rp={relying_party} | xuid={} | route=same-account-native-capability | DToken=official | UToken=official-same-user | TToken=official | XSTS=official | signature=official",
                runtime.xuid,
            ));
        });
        return Ok(native_user);
    }

    let discovery = pre_xsts::ensure_discovered();
    if discovery
        .as_ref()
        .is_ok_and(|summary| summary.high_confidence_builder_candidates != 0)
    {
        start_pre_xsts_diagnostic_probe(relying_party, url, options);
    }
    let key = format!("pre-xsts:{relying_party}");
    log_once(key, || match discovery {
        Ok(summary) => bridge_warn(&format!(
            "BMCBL custom XUser 已独立于系统账号建立，但官方 pre-XSTS UToken 注入 ABI 尚未解析 | rp={relying_party} | custom_xuid={} | route=custom-user-pre-xsts-pending | custom_utoken_available=true | native_matching_user=false | UserTokens_markers={} | UserTokens_text_xrefs={} | DeviceToken_markers={} | TitleToken_markers={} | XSTS_markers={} | reason=pre-xsts-user-token-provider-unresolved | action=fail-closed",
            runtime.xuid,
            summary.user_tokens_markers,
            summary.user_tokens_xrefs,
            summary.device_token_markers,
            summary.title_token_markers,
            summary.xsts_markers,
        )),
        Err(error) => bridge_warn(&format!(
            "BMCBL custom XUser 已独立于系统账号建立，但无法定位官方 pre-XSTS UToken 聚合点 | rp={relying_party} | custom_xuid={} | route=custom-user-pre-xsts-pending | custom_utoken_available=true | native_matching_user=false | reason={error} | action=fail-closed",
            runtime.xuid,
        )),
    });
    Err(E_FAIL)
}

fn relying_party_for_url(url: &str) -> String {
    let host = url_host(url).unwrap_or_default();
    if matches!(
        host.as_str(),
        "collections.mp.microsoft.com"
            | "purchase.mp.microsoft.com"
            | "displaycatalog.mp.microsoft.com"
            | "inventory.xboxlive.com"
            | "licensing.xboxlive.com"
    ) {
        LICENSING_RP.to_string()
    } else if host == "playfabapi.com" || host.ends_with(".playfabapi.com") {
        PLAYFAB_RP.to_string()
    } else if host == "multiplayer.minecraft.net" || host.ends_with(".multiplayer.minecraft.net") {
        MULTIPLAYER_RP.to_string()
    } else if matches!(
        host.as_str(),
        "pocket.realms.minecraft.net"
            | "bedrock.frontend.realms.minecraft-services.net"
            | "bedrock.frontendlegacy.realms.minecraft-services.net"
    ) {
        REALMS_RP.to_string()
    } else {
        XBOX_LIVE_RP.to_string()
    }
}

fn url_host(url: &str) -> Option<String> {
    let authority = url.split_once("://")?.1;
    let end = authority
        .char_indices()
        .find_map(|(index, character)| matches!(character, '/' | '?' | '#').then_some(index))
        .unwrap_or(authority.len());
    let authority = &authority[..end];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if let Some(value) = host_port.strip_prefix('[') {
        value.split_once(']')?.0
    } else {
        host_port.split_once(':').map_or(host_port, |(host, _)| host)
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

unsafe fn ansi_url(url: *const c_char) -> Result<String, HResult> {
    if url.is_null() {
        return Err(E_INVALIDARG);
    }
    let value = unsafe { CStr::from_ptr(url) }
        .to_str()
        .map_err(|_| E_INVALIDARG)?;
    if value.is_empty() || value.len() > MAX_URL_BYTES || !value.is_ascii() {
        return Err(E_INVALIDARG);
    }
    Ok(value.to_string())
}

unsafe fn utf16_url(url: *const u16) -> Result<String, HResult> {
    if url.is_null() {
        return Err(E_INVALIDARG);
    }
    let mut length = 0usize;
    while length <= MAX_URL_BYTES / 2 {
        if unsafe { url.add(length).read() } == 0 {
            let value = String::from_utf16(unsafe { std::slice::from_raw_parts(url, length) })
                .map_err(|_| E_INVALIDARG)?;
            if value.is_empty() || !value.is_ascii() {
                return Err(E_INVALIDARG);
            }
            return Ok(value);
        }
        length += 1;
    }
    Err(E_INVALIDARG)
}

pub unsafe extern "system" fn get_token_and_signature_async(
    _interface: *mut c_void,
    user: XUserHandle,
    options: u32,
    method: *const c_char,
    url: *const c_char,
    header_count: usize,
    headers: *const TokenHeader,
    body_size: usize,
    body: *const c_void,
    async_block: *mut XAsyncBlock,
) -> HResult {
    if !xuser::valid_user(user) {
        return E_INVALIDARG;
    }
    let url_text = hresult_try!(unsafe { ansi_url(url) });
    let relying_party = relying_party_for_url(&url_text);
    let native_user = hresult_try!(official_user_for_request(&relying_party, &url_text, options));

    type Function = unsafe extern "system" fn(
        *mut c_void,
        XUserHandle,
        u32,
        *const c_char,
        *const c_char,
        usize,
        *const TokenHeader,
        usize,
        *const c_void,
        *mut XAsyncBlock,
    ) -> HResult;
    let slot = hresult_try!(native_token_slot(23));
    let interface = hresult_try!(native_token_interface());
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe {
        function(
            interface,
            native_user,
            options,
            method,
            url,
            header_count,
            headers,
            body_size,
            body,
            async_block,
        )
    }
}

pub unsafe extern "system" fn get_token_and_signature_result_size(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: *mut usize,
) -> HResult {
    type Function = unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut usize) -> HResult;
    let slot = hresult_try!(native_token_slot(24));
    let interface = hresult_try!(native_token_interface());
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, async_block, size) }
}

pub unsafe extern "system" fn get_token_and_signature_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenData,
    used: *mut usize,
) -> HResult {
    type Function = unsafe extern "system" fn(
        *mut c_void,
        *mut XAsyncBlock,
        usize,
        *mut c_void,
        *mut *mut TokenData,
        *mut usize,
    ) -> HResult;
    let slot = hresult_try!(native_token_slot(25));
    let interface = hresult_try!(native_token_interface());
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, async_block, size, buffer, data, used) }
}

pub unsafe extern "system" fn get_token_and_signature_utf16_async(
    _interface: *mut c_void,
    user: XUserHandle,
    options: u32,
    method: *const u16,
    url: *const u16,
    header_count: usize,
    headers: *const TokenUtf16Header,
    body_size: usize,
    body: *const c_void,
    async_block: *mut XAsyncBlock,
) -> HResult {
    if !xuser::valid_user(user) {
        return E_INVALIDARG;
    }
    let url_text = hresult_try!(unsafe { utf16_url(url) });
    let relying_party = relying_party_for_url(&url_text);
    let native_user = hresult_try!(official_user_for_request(&relying_party, &url_text, options));

    type Function = unsafe extern "system" fn(
        *mut c_void,
        XUserHandle,
        u32,
        *const u16,
        *const u16,
        usize,
        *const TokenUtf16Header,
        usize,
        *const c_void,
        *mut XAsyncBlock,
    ) -> HResult;
    let slot = hresult_try!(native_token_slot(26));
    let interface = hresult_try!(native_token_interface());
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe {
        function(
            interface,
            native_user,
            options,
            method,
            url,
            header_count,
            headers,
            body_size,
            body,
            async_block,
        )
    }
}

pub unsafe extern "system" fn get_token_and_signature_utf16_result_size(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: *mut usize,
) -> HResult {
    type Function = unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut usize) -> HResult;
    let slot = hresult_try!(native_token_slot(27));
    let interface = hresult_try!(native_token_interface());
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, async_block, size) }
}

pub unsafe extern "system" fn get_token_and_signature_utf16_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenUtf16Data,
    used: *mut usize,
) -> HResult {
    type Function = unsafe extern "system" fn(
        *mut c_void,
        *mut XAsyncBlock,
        usize,
        *mut c_void,
        *mut *mut TokenUtf16Data,
        *mut usize,
    ) -> HResult;
    let slot = hresult_try!(native_token_slot(28));
    let interface = hresult_try!(native_token_interface());
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, async_block, size, buffer, data, used) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_minecraft_services_to_native_relying_parties() {
        assert_eq!(
            relying_party_for_url("https://userpresence.xboxlive.com/users/xuid(1)"),
            XBOX_LIVE_RP,
        );
        assert_eq!(
            relying_party_for_url("https://b980a380.minecraft.playfabapi.com/Client/Login"),
            PLAYFAB_RP,
        );
        assert_eq!(
            relying_party_for_url("https://multiplayer.minecraft.net/authentication"),
            MULTIPLAYER_RP,
        );
        assert_eq!(
            relying_party_for_url("https://pocket.realms.minecraft.net/worlds"),
            REALMS_RP,
        );
    }
}

// SPDX-License-Identifier: GPL-3.0-only

#[path = "pre_xsts.rs"]
mod pre_xsts;

use core::ffi::{c_char, c_void};
use std::{
    collections::HashSet,
    ffi::CStr,
    mem,
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

/// Selects an official Microsoft user only when Windows already exposes the
/// exact same XUID as the BMCBL session. The public XUser handle remains the
/// synthetic BMCBL object and is never passed into the official Runtime.
///
/// Different-account and no-system-account cases intentionally share the same
/// path: neither is allowed to borrow an unrelated native user's final XSTS.
fn official_user_for_request(relying_party: &str) -> Result<XUserHandle, HResult> {
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
    let native_user = hresult_try!(official_user_for_request(&relying_party));

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
    let native_user = hresult_try!(official_user_for_request(&relying_party));

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

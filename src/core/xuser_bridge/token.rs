// SPDX-License-Identifier: GPL-3.0-only

#[path = "token/native_msa.rs"]
mod native_msa;

use base64::{Engine as _, engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD}};
use core::ffi::{c_char, c_void};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    ffi::CStr,
    mem, ptr,
    sync::{Mutex, OnceLock},
};

use super::{
    abi::{
        E_FAIL, E_INVALIDARG, E_NOT_SUFFICIENT_BUFFER, E_POINTER, HResult, TokenData, TokenHeader,
        TokenUtf16Data, TokenUtf16Header, XAsyncBlock, XUserHandle,
    },
    bridge_info, bridge_warn, session, xuser,
};

const MAX_URL_BYTES: usize = 32 * 1024;
const XUSER_TOKEN_FORCE_REFRESH: u32 = 0x01;

const XBOX_LIVE_RP: &str = "http://xboxlive.com";
const PLAYFAB_RP: &str = "https://b980a380.minecraft.playfabapi.com/";
const MULTIPLAYER_RP: &str = "https://multiplayer.minecraft.net/";
const REALMS_RP: &str = "https://pocket.realms.minecraft.net/";
const LICENSING_RP: &str = "http://licensing.xboxlive.com";

static IDENTITY_ROUTE_LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static PENDING_CROSS_ACCOUNT: OnceLock<Mutex<HashMap<usize, PendingCrossAccount>>> = OnceLock::new();

#[derive(Clone)]
struct PendingCrossAccount {
    relying_party: String,
    native_xuid: u64,
    custom_xuid: u64,
    msa_override_generation: u64,
}

macro_rules! hresult_try {
    ($expression:expr) => {
        match $expression {
            Ok(value) => value,
            Err(error) => return error,
        }
    };
}

fn log_once(key: String, message: impl FnOnce()) {
    let inserted = IDENTITY_ROUTE_LOGGED
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

fn prepare_native_request(
    user: XUserHandle,
    relying_party: &str,
    options: u32,
    async_block: *mut XAsyncBlock,
) -> Result<u32, HResult> {
    if async_block.is_null() {
        return Err(E_POINTER);
    }
    let runtime = session().ok_or(E_FAIL)?;
    let native_xuid = native_user_id(user)?;
    if native_xuid == runtime.xuid {
        let key = format!("same:{relying_party}");
        log_once(key, || {
            bridge_info(&format!(
                "官方 Token 身份与 BMCBL 账号一致；直接复用 Microsoft Runtime | rp={relying_party} | xuid={} | route=same-account-native | DToken=official | TToken=official | XSTS=official | signature=official | forced_refresh=false",
                runtime.xuid,
            ));
        });
        return Ok(options);
    }

    if runtime.custom_user_token().is_none() {
        bridge_warn(&format!(
            "跨账号官方 Token 请求被拒绝 | rp={relying_party} | native_xuid={native_xuid} | custom_xuid={} | reason=custom-utoken-expired-or-unavailable | action=fail-closed",
            runtime.xuid,
        ));
        return Err(E_FAIL);
    }
    if runtime.custom_msa_access_token().is_none() {
        bridge_warn(&format!(
            "跨账号官方 Token 请求被拒绝 | rp={relying_party} | native_xuid={native_xuid} | custom_xuid={} | reason=custom-msa-access-token-expired-or-unavailable | action=fail-closed",
            runtime.xuid,
        ));
        return Err(E_FAIL);
    }
    if let Err(error) = native_msa::initialize() {
        bridge_warn(&format!(
            "跨账号 Microsoft Runtime 用户凭据覆盖桥初始化失败 | rp={relying_party} | reason={error} | action=fail-closed"
        ));
        return Err(E_FAIL);
    }

    let start_generation = native_msa::override_generation();
    PENDING_CROSS_ACCOUNT
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            async_block as usize,
            PendingCrossAccount {
                relying_party: relying_party.to_string(),
                native_xuid,
                custom_xuid: runtime.xuid,
                msa_override_generation: start_generation,
            },
        );

    let key = format!("cross-native:{relying_party}");
    log_once(key, || {
        bridge_info(&format!(
            "跨账号 Token 请求进入 Microsoft Runtime 用户凭据覆盖链 | rp={relying_party} | native_xuid={native_xuid} | custom_xuid={} | route=cross-account-native-credential-override | MSA=custom-short-lived | UToken=runtime-generated-target | DToken=official | TToken=official | XSTS=official | signature=official | force_refresh=true",
            runtime.xuid,
        ));
    });

    Ok(options | XUSER_TOKEN_FORCE_REFRESH)
}

fn cancel_pending(async_block: *mut XAsyncBlock) {
    if async_block.is_null() {
        return;
    }
    PENDING_CROSS_ACCOUNT
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(async_block as usize));
}

fn pending(async_block: *mut XAsyncBlock) -> Option<PendingCrossAccount> {
    if async_block.is_null() {
        return None;
    }
    PENDING_CROSS_ACCOUNT
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&(async_block as usize))
        .cloned()
}

fn finish_pending(async_block: *mut XAsyncBlock, result: HResult) {
    if result != E_NOT_SUFFICIENT_BUFFER {
        cancel_pending(async_block);
    }
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
    let effective_options = hresult_try!(prepare_native_request(
        user,
        &relying_party,
        options,
        async_block,
    ));

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
    let result = unsafe {
        function(
            interface,
            user,
            effective_options,
            method,
            url,
            header_count,
            headers,
            body_size,
            body,
            async_block,
        )
    };
    if result < 0 {
        cancel_pending(async_block);
    }
    result
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
    let native_result = unsafe { function(interface, async_block, size, buffer, data, used) };
    let result = unsafe {
        verify_ansi_result(async_block, native_result, size, buffer, data, used)
    };
    finish_pending(async_block, result);
    result
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
    let effective_options = hresult_try!(prepare_native_request(
        user,
        &relying_party,
        options,
        async_block,
    ));

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
    let result = unsafe {
        function(
            interface,
            user,
            effective_options,
            method,
            url,
            header_count,
            headers,
            body_size,
            body,
            async_block,
        )
    };
    if result < 0 {
        cancel_pending(async_block);
    }
    result
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
    let native_result = unsafe { function(interface, async_block, size, buffer, data, used) };
    let result = unsafe {
        verify_utf16_result(async_block, native_result, size, buffer, data, used)
    };
    finish_pending(async_block, result);
    result
}

unsafe fn verify_ansi_result(
    async_block: *mut XAsyncBlock,
    native_result: HResult,
    size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenData,
    used: *mut usize,
) -> HResult {
    let Some(request) = pending(async_block) else {
        return native_result;
    };
    if native_result < 0 || native_result == E_NOT_SUFFICIENT_BUFFER {
        return native_result;
    }
    let extracted = unsafe { ansi_result_xuid(size, buffer, data) };
    verify_cross_identity(request, extracted, size, buffer, data.cast(), used)
}

unsafe fn verify_utf16_result(
    async_block: *mut XAsyncBlock,
    native_result: HResult,
    size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenUtf16Data,
    used: *mut usize,
) -> HResult {
    let Some(request) = pending(async_block) else {
        return native_result;
    };
    if native_result < 0 || native_result == E_NOT_SUFFICIENT_BUFFER {
        return native_result;
    }
    let extracted = unsafe { utf16_result_xuid(size, buffer, data) };
    verify_cross_identity(request, extracted, size, buffer, data.cast(), used)
}

fn verify_cross_identity(
    request: PendingCrossAccount,
    extracted_xuid: Option<u64>,
    size: usize,
    buffer: *mut c_void,
    data: *mut *mut c_void,
    used: *mut usize,
) -> HResult {
    let current_generation = native_msa::override_generation();
    let override_hits = current_generation.saturating_sub(request.msa_override_generation);
    if extracted_xuid == Some(request.custom_xuid) {
        bridge_info(&format!(
            "跨账号官方 XSTS 身份验证通过 | rp={} | native_xuid={} | custom_xuid={} | msa_override_hits={} | route=cross-account-native-credential-override | DToken=official | TToken=official | XSTS=official | signature=official",
            request.relying_party,
            request.native_xuid,
            request.custom_xuid,
            override_hits,
        ));
        return 0;
    }

    if !buffer.is_null() && size != 0 {
        unsafe { ptr::write_bytes(buffer.cast::<u8>(), 0, size) };
    }
    if !data.is_null() {
        unsafe { data.write(ptr::null_mut()) };
    }
    if !used.is_null() {
        unsafe { used.write(0) };
    }
    bridge_warn(&format!(
        "跨账号官方 XSTS 结果被拒绝 | rp={} | native_xuid={} | custom_xuid={} | extracted_xuid={} | msa_override_hits={} | reason=final-xsts-user-mismatch-or-unverifiable | action=zeroize-and-fail-closed",
        request.relying_party,
        request.native_xuid,
        request.custom_xuid,
        extracted_xuid
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<unverifiable>".to_string()),
        override_hits,
    ));
    E_FAIL
}

unsafe fn ansi_result_xuid(
    buffer_size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenData,
) -> Option<u64> {
    if buffer.is_null() || data.is_null() {
        return None;
    }
    let data_ptr = unsafe { *data };
    if data_ptr.is_null() {
        return None;
    }
    let value = unsafe { &*data_ptr };
    let token_ptr = value.token.cast::<u8>();
    let bytes = bounded_bytes(buffer, buffer_size, token_ptr, value.token_size)?;
    let text = std::str::from_utf8(bytes).ok()?.trim_matches(char::from(0));
    extract_xuid_from_xsts(text)
}

unsafe fn utf16_result_xuid(
    buffer_size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenUtf16Data,
) -> Option<u64> {
    if buffer.is_null() || data.is_null() {
        return None;
    }
    let data_ptr = unsafe { *data };
    if data_ptr.is_null() {
        return None;
    }
    let value = unsafe { &*data_ptr };
    let token_ptr = value.token;
    if token_ptr.is_null() {
        return None;
    }
    let start = buffer as usize;
    let end = start.checked_add(buffer_size)?;
    let token_start = token_ptr as usize;
    if token_start < start || token_start >= end {
        return None;
    }
    let max_units = (end - token_start) / mem::size_of::<u16>();
    let requested = value.token_count.min(max_units);
    let units = unsafe { std::slice::from_raw_parts(token_ptr, requested) };
    let nul = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
    let text = String::from_utf16(&units[..nul]).ok()?;
    extract_xuid_from_xsts(&text)
}

fn bounded_bytes<'a>(
    buffer: *mut c_void,
    buffer_size: usize,
    pointer: *const u8,
    requested: usize,
) -> Option<&'a [u8]> {
    if pointer.is_null() {
        return None;
    }
    let start = buffer as usize;
    let end = start.checked_add(buffer_size)?;
    let value_start = pointer as usize;
    if value_start < start || value_start >= end {
        return None;
    }
    let length = requested.min(end - value_start);
    Some(unsafe { std::slice::from_raw_parts(pointer, length) })
}

fn extract_xuid_from_xsts(value: &str) -> Option<u64> {
    let value = value.trim_matches(|character: char| character.is_ascii_whitespace() || character == '\0');
    let token = if value.starts_with("XBL3.0 ") {
        value.rsplit_once(';').map(|(_, token)| token).unwrap_or(value)
    } else {
        value
    };
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .ok()?;
    let json: Value = serde_json::from_slice(&decoded).ok()?;
    find_xuid_claim(&json)
}

fn find_xuid_claim(value: &Value) -> Option<u64> {
    match value {
        Value::Object(object) => {
            for key in ["xid", "xuid", "Xuid", "XUID"] {
                if let Some(value) = object.get(key) {
                    if let Some(number) = value.as_u64() {
                        if number != 0 {
                            return Some(number);
                        }
                    }
                    if let Some(text) = value.as_str() {
                        if let Ok(number) = text.parse::<u64>() {
                            if number != 0 {
                                return Some(number);
                            }
                        }
                    }
                }
            }
            object.values().find_map(find_xuid_claim)
        }
        Value::Array(values) => values.iter().find_map(find_xuid_claim),
        _ => None,
    }
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

    #[test]
    fn extracts_xuid_from_xsts_jwt_payload() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"xui":[{"xid":"2535433707460133"}]}"#);
        let payload = payload.replace("XFwi", "");
        let correct_payload = URL_SAFE_NO_PAD.encode(b"{\"xui\":[{\"xid\":\"2535433707460133\"}]}");
        let token = format!("header.{correct_payload}.signature");
        assert_eq!(extract_xuid_from_xsts(&token), Some(2535433707460133));
        let _ = payload;
    }
}

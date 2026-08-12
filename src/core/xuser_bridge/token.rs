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

const XUSER_ADD_DEFAULT_USER_SILENTLY: u32 = 0x01;
const XUSER_TOKEN_FORCE_REFRESH: u32 = 0x01;

static ROUTE_LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static DIAGNOSTIC_PROBES_STARTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static AUTH_CAPABILITY_NATIVE_USER: OnceLock<Mutex<Option<(usize, u64)>>> = OnceLock::new();

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

fn diagnostic_probe_once(relying_party: &str) -> bool {
    DIAGNOSTIC_PROBES_STARTED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(relying_party.to_string())
}

struct NativeAddProbeContext {
    relying_party: String,
    url: String,
    original_options: u32,
    diagnostic_options: u32,
}

struct NativeTokenProbeContext {
    relying_party: String,
    native_user: XUserHandle,
    native_xuid: u64,
    method: CString,
    url: CString,
    original_options: u32,
    diagnostic_options: u32,
    close_native_user_on_complete: bool,
}

/// A Microsoft Runtime user handle that has already been verified to represent
/// exactly the same Xbox identity as the BMCBL virtual XUser.
///
/// Keeping this wrapper private is intentional: the normal token forwarding
/// path cannot accept an arbitrary native user (for example Windows account A)
/// and therefore cannot accidentally return A's final XSTS for virtual user B.
#[derive(Clone, Copy)]
struct SameIdentityNativeCapability {
    user: XUserHandle,
}

impl SameIdentityNativeCapability {
    fn handle(self) -> XUserHandle {
        self.user
    }
}

#[derive(Clone, Copy)]
enum MicrosoftAuthCapability {
    SameIdentity(SameIdentityNativeCapability),
    PreXstsInjected { user: XUserHandle, native_xuid: u64 },
}

impl MicrosoftAuthCapability {
    fn handle(self) -> XUserHandle {
        match self {
            Self::SameIdentity(capability) => capability.handle(),
            Self::PreXstsInjected { user, native_xuid } => {
                debug_assert_ne!(native_xuid, 0);
                user
            }
        }
    }
}

fn cached_auth_capability_user() -> Option<(XUserHandle, u64)> {
    let guard = AUTH_CAPABILITY_NATIVE_USER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.map(|(user, xuid)| (user as XUserHandle, xuid))
}

fn remember_auth_capability_user(user: XUserHandle, xuid: u64) -> (XUserHandle, u64) {
    let mut guard = AUTH_CAPABILITY_NATIVE_USER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((existing, existing_xuid)) = *guard {
        if existing != user as usize {
            close_native_user(user);
        }
        return (existing as XUserHandle, existing_xuid);
    }
    *guard = Some((user as usize, xuid));
    (user, xuid)
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

fn native_relation(native_xuid: u64) -> &'static str {
    match session() {
        Some(runtime) if native_xuid == runtime.xuid => "same",
        Some(_) => "different",
        None => "unknown",
    }
}

/// Triggers Microsoft Runtime's own token machinery only to observe the
/// pre-XSTS DeviceToken/TitleToken/UserTokens aggregation path.
///
/// A native Windows user returned here is a disposable capability-bootstrap
/// handle. It is never installed as the backing identity for BLoader's public
/// synthetic XUser and its final token result is never returned to Minecraft.
fn start_pre_xsts_diagnostic_probe(relying_party: &str, url: &str, options: u32) {
    if !diagnostic_probe_once(relying_party) {
        bridge_info(&format!(
            "pre-XSTS capability probe 已存在；跳过重复启动 | rp={relying_party} | reason=diagnostic-already-started | native_user_is_backing_identity=false | result_discarded=true | secrets_logged=false"
        ));
        return;
    }

    let diagnostic_options = options | XUSER_TOKEN_FORCE_REFRESH;

    let (Ok(interface), Ok(slot)) = (native_token_interface(), native_token_slot(7)) else {
        bridge_warn(&format!(
            "pre-XSTS capability probe 无法启动 native Add；Microsoft Runtime XUserAddAsync 不可用 | rp={relying_party} | system_account_optional=true | result=no-native-provider | action=diagnostic-skip"
        ));
        return;
    };

    let context = Box::new(NativeAddProbeContext {
        relying_party: relying_party.to_string(),
        url: url.to_string(),
        original_options: options,
        diagnostic_options,
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
            "pre-XSTS capability probe native Add 启动失败；不会触发系统登录 UI | rp={relying_party} | result=0x{:08X} | system_account_optional=true | action=diagnostic-skip",
            result as u32
        ));
        return;
    }

    bridge_info(&format!(
        "pre-XSTS capability probe native Add 已启动 | rp={relying_party} | mode=silent-diagnostic | identity_owner=bloader-virtual-xuser | microsoft_runtime_role=auth-capability-only | native_user_is_backing_identity=false | system_login_ui=not-invoked | original_options=0x{options:08X} | diagnostic_options=0x{diagnostic_options:08X} | diagnostic_force_refresh=true | result_discarded=true | secrets_logged=false"
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
        original_options,
        diagnostic_options,
    } = *context;

    let mut native_user = ptr::null_mut();
    let result = native_add_result(async_block, &mut native_user);
    unsafe { drop(Box::from_raw(async_block)) };

    if result < 0 || native_user.is_null() {
        bridge_warn(&format!(
            "pre-XSTS capability probe native Add 未返回用户；无系统账号或 Runtime 拒绝 silent Add | rp={relying_party} | result=0x{:08X} | system_account_optional=true | action=diagnostic-skip",
            result as u32
        ));
        return;
    }

    let native_xuid = match native_user_id(native_user) {
        Ok(value) => value,
        Err(status) => {
            close_native_user(native_user);
            bridge_warn(&format!(
                "pre-XSTS capability probe native 用户身份无法读取；已关闭诊断句柄 | rp={relying_party} | result=0x{:08X} | native_user_is_backing_identity=false | action=diagnostic-skip",
                status as u32
            ));
            return;
        }
    };
    let relation = native_relation(native_xuid);

    bridge_info(&format!(
        "pre-XSTS capability probe native 用户已建立 | rp={relying_party} | native_xuid={native_xuid} | native_identity_relation={relation} | purpose=trigger-microsoft-auth-capability-only | identity_owner=bloader-virtual-xuser | native_user_is_backing_identity=false | diagnostic_force_refresh=true | result_discarded=true | secrets_logged=false"
    ));

    let (native_user, native_xuid) = remember_auth_capability_user(native_user, native_xuid);
    start_native_token_diagnostic_probe(
        relying_party,
        url,
        original_options,
        diagnostic_options,
        native_user,
        native_xuid,
        false,
    );
}

fn start_native_token_diagnostic_probe(
    relying_party: String,
    url: String,
    original_options: u32,
    diagnostic_options: u32,
    native_user: XUserHandle,
    native_xuid: u64,
    close_native_user_on_complete: bool,
) {
    let (Ok(interface), Ok(slot)) = (native_token_interface(), native_token_slot(23)) else {
        if close_native_user_on_complete {
            close_native_user(native_user);
        }
        bridge_warn(&format!(
            "pre-XSTS capability probe native token 请求无法启动；Token slot 不可用 | rp={relying_party} | native_xuid={native_xuid} | native_user_is_backing_identity=false | action=diagnostic-skip"
        ));
        return;
    };

    let Ok(url) = CString::new(url) else {
        if close_native_user_on_complete {
            close_native_user(native_user);
        }
        bridge_warn(&format!(
            "pre-XSTS capability probe URL 包含非法 NUL；跳过 native token 请求 | rp={relying_party} | native_xuid={native_xuid} | native_user_is_backing_identity=false | action=diagnostic-skip"
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
        original_options,
        diagnostic_options,
        close_native_user_on_complete,
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
            diagnostic_options,
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
        if close_native_user_on_complete {
            close_native_user(native_user);
        }
        bridge_warn(&format!(
            "pre-XSTS capability probe native token 请求启动失败；诊断请求未进入 Runtime | rp={relying_party_for_log} | native_xuid={native_xuid} | result=0x{:08X} | native_user_is_backing_identity=false | native_final_xsts_reuse=false | original_options=0x{original_options:08X} | diagnostic_options=0x{diagnostic_options:08X} | diagnostic_force_refresh=true | result_returned_to_minecraft=false | secrets_logged=false",
            result as u32
        ));
        return;
    }

    bridge_info(&format!(
        "pre-XSTS capability probe native token 请求已启动 | rp={relying_party_for_log} | native_xuid={native_xuid} | native_identity_relation={} | mode=trigger-pre-xsts-capability-only | identity_owner=bloader-virtual-xuser | native_user_is_backing_identity=false | native_final_xsts_reuse=false | original_options=0x{original_options:08X} | diagnostic_options=0x{diagnostic_options:08X} | diagnostic_force_refresh=true | result_returned_to_minecraft=false | result_discarded=true | secrets_logged=false",
        native_relation(native_xuid),
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
        "pre-XSTS capability probe native token 请求完成并丢弃结果 | rp={} | native_xuid={} | native_identity_relation={} | result_size_status=0x{:08X} | result_size={} | original_options=0x{:08X} | diagnostic_options=0x{:08X} | diagnostic_force_refresh=true | identity_owner=bloader-virtual-xuser | native_user_is_backing_identity=false | native_final_xsts_reuse=false | result_returned_to_minecraft=false | token_body_read=false | secrets_logged=false",
        context.relying_party,
        context.native_xuid,
        native_relation(context.native_xuid),
        size_status as u32,
        result_size,
        context.original_options,
        context.diagnostic_options,
    ));

    if context.close_native_user_on_complete {
        close_native_user(context.native_user);
    }
}

fn native_token_result_size(async_block: *mut XAsyncBlock, size: *mut usize) -> HResult {
    let (Ok(interface), Ok(slot)) = (native_token_interface(), native_token_slot(24)) else {
        return E_FAIL;
    };
    type Function = unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut usize) -> HResult;
    let function: Function = unsafe { mem::transmute(slot) };
    unsafe { function(interface, async_block, size) }
}

/// Resolves the only Microsoft Runtime handle that is allowed to feed the
/// normal `XUserGetTokenAndSignature*` result path.
///
/// The fast path is deliberately restricted to a verified same-XUID Windows
/// user. Different-account and no-system-account sessions must use the
/// pre-XSTS auth adapter, where Microsoft's device/title credentials and
/// XSTS/signature machinery remain official while BMCBL supplies only B's
/// raw UToken before `xsts/authorize`.
///
/// Until the pre-XSTS ABI is resolved, that virtual-user route fails closed.
/// In particular, a native account A may be used to *trigger diagnostics*,
/// but A is never a backing XUser and A's final XSTS is never returned.
fn microsoft_auth_capability_for_request(
    relying_party: &str,
    url: &str,
    options: u32,
) -> Result<MicrosoftAuthCapability, HResult> {
    let runtime = session().ok_or(E_FAIL)?;
    if runtime.custom_user_token().is_none() {
        bridge_warn(&format!(
            "BMCBL UToken 不可用；拒绝 Xbox Token 请求 | rp={relying_party} | custom_xuid={} | identity_owner=bloader-virtual-xuser | action=fail-closed",
            runtime.xuid
        ));
        return Err(E_FAIL);
    }

    if let Some(native_user) = xuser::native_user_for_custom_identity() {
        let key = format!("native:{relying_party}");
        log_once(key, || {
            bridge_info(&format!(
                "BMCBL 账号存在同 XUID 的系统 native XUser；使用 Microsoft Runtime 官方同身份快速路径 | rp={relying_party} | xuid={} | route=same-identity-native-capability | public_identity=bloader-synthetic | native_identity_relation=same | DToken=official | UToken=official-same-user | TToken=official | XSTS=official | signature=official",
                runtime.xuid,
            ));
        });
        return Ok(MicrosoftAuthCapability::SameIdentity(
            SameIdentityNativeCapability { user: native_user },
        ));
    }

    let cached_capability = cached_auth_capability_user();
    let native_identity_relation = cached_capability.map_or_else(
        || match runtime.native_system_xuid_hint {
            Some(native_xuid) if native_xuid == runtime.xuid => "same-hint-no-verified-capability",
            Some(_) => "different",
            None => "none",
        },
        |(_, native_xuid)| native_relation(native_xuid),
    );

    let discovery = pre_xsts::ensure_discovered();
    if pre_xsts::custom_user_injection_ready()
        && let Some((native_user, native_xuid)) = cached_capability
    {
        let key = format!("pre-xsts-ready:{relying_party}");
        log_once(key, || {
            bridge_info(&format!(
                "pre-XSTS UToken 注入 ABI 已解析；启用跨身份 Microsoft Runtime 认证能力路径 | rp={relying_party} | custom_xuid={} | native_xuid={native_xuid} | native_identity_relation={} | route=virtual-user-pre-xsts-injected | identity_owner=bloader-virtual-xuser | native_user_is_backing_identity=false | DToken=official | UToken=bmcbl-custom | TToken=official | XSTS=official-after-user-substitution | signature=official | secrets_logged=false",
                runtime.xuid,
                native_relation(native_xuid),
            ));
        });
        return Ok(MicrosoftAuthCapability::PreXstsInjected {
            user: native_user,
            native_xuid,
        });
    }
    if discovery
        .as_ref()
        .is_ok_and(|summary| summary.high_confidence_builder_candidates != 0)
    {
        start_pre_xsts_diagnostic_probe(relying_party, url, options);
    }
    let key = format!("pre-xsts:{relying_party}");
    log_once(key, || match discovery {
        Ok(summary) => bridge_warn(&format!(
            "Virtual XUser(B) 已独立建立；Microsoft Runtime 仅作为认证能力源，但 pre-XSTS UToken 注入 ABI 尚未解析 | rp={relying_party} | custom_xuid={} | route=virtual-user-official-auth-adapter-pending | identity_owner=bloader-virtual-xuser | microsoft_runtime_role=device-title-xsts-signature-capability | custom_utoken_available=true | native_identity_relation={native_identity_relation} | native_user_is_backing_identity=false | native_final_xsts_reuse=false | UserTokens_markers={} | UserTokens_text_xrefs={} | DeviceToken_markers={} | TitleToken_markers={} | XSTS_markers={} | reason=pre-xsts-user-token-provider-unresolved | action=fail-closed",
            runtime.xuid,
            summary.user_tokens_markers,
            summary.user_tokens_xrefs,
            summary.device_token_markers,
            summary.title_token_markers,
            summary.xsts_markers,
        )),
        Err(error) => bridge_warn(&format!(
            "Virtual XUser(B) 已独立建立，但无法定位 Microsoft Runtime pre-XSTS 认证能力聚合点 | rp={relying_party} | custom_xuid={} | route=virtual-user-official-auth-adapter-pending | identity_owner=bloader-virtual-xuser | microsoft_runtime_role=device-title-xsts-signature-capability | custom_utoken_available=true | native_identity_relation={native_identity_relation} | native_user_is_backing_identity=false | native_final_xsts_reuse=false | reason={error} | action=fail-closed",
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
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(value) = host_port.strip_prefix('[') {
        value.split_once(']')?.0
    } else {
        host_port
            .split_once(':')
            .map_or(host_port, |(host, _)| host)
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
    let capability = hresult_try!(microsoft_auth_capability_for_request(
        &relying_party,
        &url_text,
        options
    ));
    let native_user = capability.handle();

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
    let capability = hresult_try!(microsoft_auth_capability_for_request(
        &relying_party,
        &url_text,
        options
    ));
    let native_user = capability.handle();

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

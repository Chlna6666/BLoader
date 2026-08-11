// SPDX-License-Identifier: GPL-3.0-only

use core::ffi::{c_char, c_void};
use std::{
    collections::{HashMap, HashSet},
    ffi::CStr,
    mem, ptr,
    sync::{Mutex, OnceLock},
};
use zeroize::{Zeroize, Zeroizing};

use super::{
    abi::{
        E_FAIL, E_INVALIDARG, E_NOT_SUFFICIENT_BUFFER, E_POINTER, HResult, S_OK, TokenData,
        TokenHeader, TokenUtf16Data, TokenUtf16Header, XAsyncBlock, XAsyncOp,
        XAsyncProviderData, XUserHandle,
    },
    bridge_debug, bridge_info, bridge_warn, session, xasync, xuser,
};

const TOKEN_OPTIONS_MASK: u32 = 0x03;
const MAX_METHOD_LENGTH: usize = 32;
const MAX_URL_LENGTH: usize = 32 * 1024;
const MAX_HEADER_COUNT: usize = 128;
const MAX_HEADER_NAME_LENGTH: usize = 256;
const MAX_HEADER_VALUE_LENGTH: usize = 32 * 1024;
const MAX_REQUEST_BODY_SIZE: usize = 64 * 1024 * 1024;
const MAX_UTF16_INPUT_UNITS: usize = 32 * 1024;
const DEFAULT_XBOX_MAX_BODY_BYTES: usize = 8 * 1024;
const FULL_BODY_MAX_BYTES: usize = u32::MAX as usize;
const TOKEN_NAME_ANSI: &[u8] = b"XUserGetTokenAndSignatureAsync\0";
const TOKEN_NAME_UTF16: &[u8] = b"XUserGetTokenAndSignatureUtf16Async\0";
static TOKEN_IDENTITY_ANSI: u8 = 0x41;
static TOKEN_IDENTITY_UTF16: u8 = 0x57;

const XBOX_LIVE_RP: &str = "http://xboxlive.com";
const PLAYFAB_RP: &str = "https://b980a380.minecraft.playfabapi.com/";
const MULTIPLAYER_RP: &str = "https://multiplayer.minecraft.net/";
const REALMS_RP: &str = "https://pocket.realms.minecraft.net/";
const LICENSING_RP: &str = "http://licensing.xboxlive.com";

static IDENTITY_ROUTE_LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static ASYNC_ROUTES: OnceLock<Mutex<HashMap<usize, AsyncRoute>>> = OnceLock::new();

macro_rules! hresult_try {
    ($expression:expr) => {
        match $expression {
            Ok(value) => value,
            Err(error) => return error,
        }
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TokenRoute {
    Native,
    Fallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AsyncRoute {
    Native,
    Fallback,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestHeader {
    name: String,
    value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SigningPolicy {
    max_body_bytes: usize,
    extra_header_names: Vec<String>,
}

impl SigningPolicy {
    fn xbox_default() -> Self {
        Self {
            max_body_bytes: DEFAULT_XBOX_MAX_BODY_BYTES,
            extra_header_names: Vec::new(),
        }
    }

    fn xbox_full_body() -> Self {
        Self {
            max_body_bytes: FULL_BODY_MAX_BYTES,
            extra_header_names: Vec::new(),
        }
    }

    fn caller_headers(headers: &[RequestHeader]) -> Self {
        let mut names = Vec::new();
        for header in headers {
            if is_transport_or_auth_header(&header.name)
                || names
                    .iter()
                    .any(|name: &String| name.eq_ignore_ascii_case(&header.name))
            {
                continue;
            }
            names.push(header.name.clone());
        }
        Self {
            max_body_bytes: FULL_BODY_MAX_BYTES,
            extra_header_names: names,
        }
    }
}

struct TokenContext {
    utf16: bool,
    method: String,
    request_target: String,
    relying_party: &'static str,
    policy_header_values: Vec<String>,
    max_body_bytes: usize,
    body: Vec<u8>,
    authorization: Vec<u8>,
    authorization_utf16: Vec<u16>,
    signature: Vec<u8>,
    signature_utf16: Vec<u16>,
    prepared: bool,
}

impl TokenContext {
    fn new(
        method: &str,
        url: &str,
        headers: Vec<RequestHeader>,
        body: &[u8],
        utf16: bool,
    ) -> Result<Self, HResult> {
        let relying_party = relying_party_for_url(url);
        let runtime = session().ok_or(E_FAIL)?;
        let token = runtime
            .fallback_token_for_relying_party(relying_party)
            .ok_or_else(|| {
                bridge_warn(&format!(
                    "跨账号 Token 请求无法路由 | rp={relying_party} | reason=no-valid-bmcbl-fallback-token"
                ));
                E_FAIL
            })?;
        let remaining = token.expires_at.saturating_sub(now_epoch());
        let request_target = request_target_from_url(url).ok_or(E_INVALIDARG)?;
        let policy = signing_policy_for_url(url, &headers);
        let policy_header_values =
            select_policy_header_values(&headers, &policy.extra_header_names);

        let authorization_text = Zeroizing::new(format!(
            "XBL3.0 x={};{}",
            token.user_hash, token.token
        ));
        let mut authorization = authorization_text.as_bytes().to_vec();
        authorization.push(0);
        let mut authorization_utf16 = authorization_text.encode_utf16().collect::<Vec<_>>();
        authorization_utf16.push(0);

        bridge_info(&format!(
            "跨账号 Token 请求使用 BMCBL 预认证回退 | rp={relying_party} | encoding={} | method={} | path={} | token_remaining={}s | final_xsts=bmcbl-preauth | signature=bmcbl-device-key | secrets_logged=false",
            if utf16 { "utf16" } else { "ansi" },
            method.to_ascii_uppercase(),
            safe_request_path(&request_target),
            remaining,
        ));

        Ok(Self {
            utf16,
            method: method.to_ascii_uppercase(),
            request_target,
            relying_party,
            policy_header_values,
            max_body_bytes: policy.max_body_bytes,
            body: body.to_vec(),
            authorization,
            authorization_utf16,
            signature: Vec::new(),
            signature_utf16: Vec::new(),
            prepared: false,
        })
    }

    fn prepare(&mut self) -> Result<(), HResult> {
        if self.prepared {
            return Ok(());
        }
        let authorization = self
            .authorization
            .strip_suffix(&[0])
            .and_then(|value| std::str::from_utf8(value).ok())
            .ok_or(E_INVALIDARG)?;
        let policy_header_values = self
            .policy_header_values
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let body_to_sign = &self.body[..self.body.len().min(self.max_body_bytes)];
        let signing_key = session()
            .and_then(|runtime| runtime.fallback_signing_key())
            .ok_or(E_FAIL)?;
        let signature_text = Zeroizing::new(signing_key.sign_request(
            &self.method,
            &self.request_target,
            authorization,
            &policy_header_values,
            body_to_sign,
        )?);

        self.body.zeroize();
        self.body.clear();
        self.policy_header_values.zeroize();
        self.policy_header_values.clear();

        self.signature = signature_text.as_bytes().to_vec();
        self.signature.push(0);
        self.signature_utf16 = signature_text.encode_utf16().collect();
        self.signature_utf16.push(0);
        self.prepared = true;

        bridge_debug(&format!(
            "跨账号请求签名已生成 | rp={} | encoding={} | signature=bmcbl-device-key | authorization=redacted | signature_body=redacted",
            self.relying_party,
            if self.utf16 { "utf16" } else { "ansi" },
        ));
        Ok(())
    }

    fn required_size(&self) -> Option<usize> {
        if !self.prepared {
            return None;
        }
        if self.utf16 {
            self.authorization_utf16
                .len()
                .checked_add(self.signature_utf16.len())?
                .checked_mul(mem::size_of::<u16>())?
                .checked_add(mem::size_of::<TokenUtf16Data>())
        } else {
            self.authorization
                .len()
                .checked_add(self.signature.len())?
                .checked_add(mem::size_of::<TokenData>())
        }
    }
}

impl Drop for TokenContext {
    fn drop(&mut self) {
        self.body.zeroize();
        self.policy_header_values.zeroize();
        self.authorization.zeroize();
        self.authorization_utf16.zeroize();
        self.signature.zeroize();
        self.signature_utf16.zeroize();
    }
}

fn token_identity(utf16: bool) -> *const c_void {
    if utf16 {
        (&TOKEN_IDENTITY_UTF16 as *const u8).cast()
    } else {
        (&TOKEN_IDENTITY_ANSI as *const u8).cast()
    }
}

fn token_name(utf16: bool) -> *const c_char {
    if utf16 {
        TOKEN_NAME_UTF16.as_ptr().cast()
    } else {
        TOKEN_NAME_ANSI.as_ptr().cast()
    }
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

fn token_route(user: XUserHandle, relying_party: &'static str) -> Result<TokenRoute, HResult> {
    let runtime = session().ok_or(E_FAIL)?;
    let native_xuid = native_user_id(user)?;
    if native_xuid == runtime.xuid {
        let key = format!("native:{relying_party}");
        log_once(key, || {
            bridge_info(&format!(
                "官方 Token 身份与 BMCBL 账号一致；直接复用 Microsoft Runtime | rp={relying_party} | xuid={} | route=same-account-native | DToken=official | TToken=official | XSTS=official | signature=official | forced_refresh=false",
                runtime.xuid,
            ));
        });
        return Ok(TokenRoute::Native);
    }

    let fallback_ready = runtime
        .fallback_token_for_relying_party(relying_party)
        .is_some()
        && runtime.fallback_signing_key().is_some();
    if !fallback_ready {
        let key = format!("cross-missing:{relying_party}");
        log_once(key, || {
            bridge_warn(&format!(
                "跨账号 Token 请求被拒绝 | rp={relying_party} | native_xuid={native_xuid} | custom_xuid={} | custom_utoken_available={} | fallback_ready=false | action=fail-closed",
                runtime.xuid,
                runtime.custom_user_token().is_some(),
            ));
        });
        return Err(E_FAIL);
    }

    let key = format!("cross-fallback:{relying_party}");
    log_once(key, || {
        bridge_info(&format!(
            "跨账号 Token 路由切换到 BMCBL 预认证回退 | rp={relying_party} | native_xuid={native_xuid} | custom_xuid={} | custom_utoken_available={} | route=cross-account-bmcbl-preauth | MicrosoftRuntimeUserToken=bypassed | secrets_logged=false",
            runtime.xuid,
            runtime.custom_user_token().is_some(),
        ));
    });
    Ok(TokenRoute::Fallback)
}

fn remember_async_route(async_block: *mut XAsyncBlock, route: AsyncRoute) {
    if async_block.is_null() {
        return;
    }
    ASYNC_ROUTES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(async_block as usize, route);
}

fn async_route(async_block: *mut XAsyncBlock) -> Option<AsyncRoute> {
    if async_block.is_null() {
        return None;
    }
    ASYNC_ROUTES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&(async_block as usize))
        .copied()
}

fn finish_async_route(async_block: *mut XAsyncBlock, result: HResult) {
    if async_block.is_null() || result == E_NOT_SUFFICIENT_BUFFER {
        return;
    }
    ASYNC_ROUTES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(async_block as usize));
}

unsafe extern "system" fn token_provider(
    operation: XAsyncOp,
    provider_data: *const XAsyncProviderData,
) -> HResult {
    if provider_data.is_null() {
        return E_POINTER;
    }
    let provider_data = unsafe { &*provider_data };
    let context = provider_data.context.cast::<TokenContext>();
    if context.is_null() {
        return E_POINTER;
    }

    match operation {
        XAsyncOp::Begin => unsafe { xasync::schedule(provider_data.async_block, 0) },
        XAsyncOp::DoWork => {
            let context = unsafe { &mut *context };
            let completion = context
                .prepare()
                .and_then(|()| context.required_size().ok_or(E_FAIL));
            match completion {
                Ok(required_size) => unsafe {
                    xasync::complete(provider_data.async_block, S_OK, required_size)
                },
                Err(error) => unsafe { xasync::complete(provider_data.async_block, error, 0) },
            }
            S_OK
        }
        XAsyncOp::GetResult => {
            let context = unsafe { &*context };
            let Some(required_size) = context.required_size() else {
                return E_FAIL;
            };
            if provider_data.buffer.is_null() || provider_data.buffer_size < required_size {
                return E_NOT_SUFFICIENT_BUFFER;
            }

            if context.utf16 {
                let data = provider_data.buffer.cast::<TokenUtf16Data>();
                let token = unsafe { data.add(1).cast::<u16>() };
                let signature = unsafe { token.add(context.authorization_utf16.len()) };
                unsafe {
                    ptr::copy_nonoverlapping(
                        context.authorization_utf16.as_ptr(),
                        token,
                        context.authorization_utf16.len(),
                    );
                    ptr::copy_nonoverlapping(
                        context.signature_utf16.as_ptr(),
                        signature,
                        context.signature_utf16.len(),
                    );
                    data.write(TokenUtf16Data {
                        token_count: context.authorization_utf16.len() * mem::size_of::<u16>(),
                        signature_count: context.signature_utf16.len() * mem::size_of::<u16>(),
                        token,
                        signature,
                    });
                }
            } else {
                let data = provider_data.buffer.cast::<TokenData>();
                let token = unsafe { data.add(1).cast::<u8>() };
                let signature = unsafe { token.add(context.authorization.len()) };
                unsafe {
                    ptr::copy_nonoverlapping(
                        context.authorization.as_ptr(),
                        token,
                        context.authorization.len(),
                    );
                    ptr::copy_nonoverlapping(
                        context.signature.as_ptr(),
                        signature,
                        context.signature.len(),
                    );
                    data.write(TokenData {
                        token_size: context.authorization.len(),
                        signature_size: context.signature.len(),
                        token: token.cast(),
                        signature: signature.cast(),
                    });
                }
            }
            S_OK
        }
        XAsyncOp::Cancel => S_OK,
        XAsyncOp::Cleanup => {
            unsafe { drop(Box::from_raw(context)) };
            S_OK
        }
    }
}

unsafe fn begin_fallback_request(
    options: u32,
    method: &str,
    url: &str,
    headers: Vec<RequestHeader>,
    body_size: usize,
    body: *const c_void,
    async_block: *mut XAsyncBlock,
    utf16: bool,
) -> HResult {
    if async_block.is_null() || (body_size != 0 && body.is_null()) {
        return E_POINTER;
    }
    if options & !TOKEN_OPTIONS_MASK != 0
        || method.is_empty()
        || method.len() > MAX_METHOD_LENGTH
        || !method.is_ascii()
        || url.is_empty()
        || url.len() > MAX_URL_LENGTH
        || !url.is_ascii()
        || body_size > MAX_REQUEST_BODY_SIZE
    {
        return E_INVALIDARG;
    }

    let body = if body_size == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(body.cast::<u8>(), body_size) }
    };
    let context = match TokenContext::new(method, url, headers, body, utf16) {
        Ok(context) => Box::into_raw(Box::new(context)),
        Err(error) => return error,
    };
    let result = unsafe {
        xasync::begin(
            async_block,
            context.cast(),
            token_identity(utf16),
            token_name(utf16),
            token_provider,
        )
    };
    if result < 0 {
        unsafe { drop(Box::from_raw(context)) };
    } else {
        remember_async_route(async_block, AsyncRoute::Fallback);
    }
    result
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
    let url_text = hresult_try!(unsafe { ansi_string_bounded(url, MAX_URL_LENGTH) });
    let relying_party = relying_party_for_url(&url_text);
    match hresult_try!(token_route(user, relying_party)) {
        TokenRoute::Native => {
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
                    options,
                    method,
                    url,
                    header_count,
                    headers,
                    body_size,
                    body,
                    async_block,
                )
            };
            if result >= 0 {
                remember_async_route(async_block, AsyncRoute::Native);
            }
            result
        }
        TokenRoute::Fallback => {
            if method.is_null()
                || header_count > MAX_HEADER_COUNT
                || (header_count != 0 && headers.is_null())
            {
                return E_POINTER;
            }
            let method_text = hresult_try!(unsafe { ansi_string_bounded(method, MAX_METHOD_LENGTH) });
            let copied_headers = hresult_try!(unsafe { copy_ansi_headers(headers, header_count) });
            unsafe {
                begin_fallback_request(
                    options,
                    &method_text,
                    &url_text,
                    copied_headers,
                    body_size,
                    body,
                    async_block,
                    false,
                )
            }
        }
    }
}

pub unsafe extern "system" fn get_token_and_signature_result_size(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: *mut usize,
) -> HResult {
    match async_route(async_block) {
        Some(AsyncRoute::Native) => {
            type Function =
                unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut usize) -> HResult;
            let slot = hresult_try!(native_token_slot(24));
            let interface = hresult_try!(native_token_interface());
            let function: Function = unsafe { mem::transmute(slot) };
            unsafe { function(interface, async_block, size) }
        }
        Some(AsyncRoute::Fallback) => unsafe { xasync::get_result_size(async_block, size) },
        None => E_FAIL,
    }
}

pub unsafe extern "system" fn get_token_and_signature_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenData,
    used: *mut usize,
) -> HResult {
    let result = match async_route(async_block) {
        Some(AsyncRoute::Native) => {
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
        Some(AsyncRoute::Fallback) => {
            if async_block.is_null() || buffer.is_null() || data.is_null() {
                return E_POINTER;
            }
            let result = unsafe {
                xasync::get_result(async_block, token_identity(false), size, buffer, used)
            };
            if result >= 0 {
                unsafe { data.write(buffer.cast()) };
            }
            result
        }
        None => E_FAIL,
    };
    finish_async_route(async_block, result);
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
    let url_text = hresult_try!(unsafe { utf16_to_string_bounded(url, MAX_URL_LENGTH) });
    let relying_party = relying_party_for_url(&url_text);
    match hresult_try!(token_route(user, relying_party)) {
        TokenRoute::Native => {
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
                    options,
                    method,
                    url,
                    header_count,
                    headers,
                    body_size,
                    body,
                    async_block,
                )
            };
            if result >= 0 {
                remember_async_route(async_block, AsyncRoute::Native);
            }
            result
        }
        TokenRoute::Fallback => {
            if method.is_null()
                || header_count > MAX_HEADER_COUNT
                || (header_count != 0 && headers.is_null())
            {
                return E_POINTER;
            }
            let method_text = hresult_try!(unsafe {
                utf16_to_string_bounded(method, MAX_METHOD_LENGTH)
            });
            let copied_headers = hresult_try!(unsafe { copy_utf16_headers(headers, header_count) });
            unsafe {
                begin_fallback_request(
                    options,
                    &method_text,
                    &url_text,
                    copied_headers,
                    body_size,
                    body,
                    async_block,
                    true,
                )
            }
        }
    }
}

pub unsafe extern "system" fn get_token_and_signature_utf16_result_size(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: *mut usize,
) -> HResult {
    match async_route(async_block) {
        Some(AsyncRoute::Native) => {
            type Function =
                unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut usize) -> HResult;
            let slot = hresult_try!(native_token_slot(27));
            let interface = hresult_try!(native_token_interface());
            let function: Function = unsafe { mem::transmute(slot) };
            unsafe { function(interface, async_block, size) }
        }
        Some(AsyncRoute::Fallback) => unsafe { xasync::get_result_size(async_block, size) },
        None => E_FAIL,
    }
}

pub unsafe extern "system" fn get_token_and_signature_utf16_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenUtf16Data,
    used: *mut usize,
) -> HResult {
    let result = match async_route(async_block) {
        Some(AsyncRoute::Native) => {
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
        Some(AsyncRoute::Fallback) => {
            if async_block.is_null() || buffer.is_null() || data.is_null() {
                return E_POINTER;
            }
            let result = unsafe {
                xasync::get_result(async_block, token_identity(true), size, buffer, used)
            };
            if result >= 0 {
                unsafe { data.write(buffer.cast()) };
            }
            result
        }
        None => E_FAIL,
    };
    finish_async_route(async_block, result);
    result
}

unsafe fn copy_ansi_headers(
    headers: *const TokenHeader,
    count: usize,
) -> Result<Vec<RequestHeader>, HResult> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if headers.is_null() {
        return Err(E_POINTER);
    }
    let mut copied = Vec::with_capacity(count);
    for header in unsafe { std::slice::from_raw_parts(headers, count) } {
        if header.name.is_null() || header.value.is_null() {
            return Err(E_POINTER);
        }
        let name = unsafe { CStr::from_ptr(header.name) }
            .to_str()
            .map_err(|_| E_INVALIDARG)?;
        let value = unsafe { CStr::from_ptr(header.value) }
            .to_str()
            .map_err(|_| E_INVALIDARG)?;
        copied.push(validate_header(name, value)?);
    }
    Ok(copied)
}

unsafe fn copy_utf16_headers(
    headers: *const TokenUtf16Header,
    count: usize,
) -> Result<Vec<RequestHeader>, HResult> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if headers.is_null() {
        return Err(E_POINTER);
    }
    let mut copied = Vec::with_capacity(count);
    for header in unsafe { std::slice::from_raw_parts(headers, count) } {
        let name = unsafe { utf16_to_string_bounded(header.name, MAX_HEADER_NAME_LENGTH) }?;
        let value = unsafe { utf16_to_string_bounded(header.value, MAX_HEADER_VALUE_LENGTH) }?;
        copied.push(validate_header(&name, &value)?);
    }
    Ok(copied)
}

fn validate_header(name: &str, value: &str) -> Result<RequestHeader, HResult> {
    if name.is_empty()
        || name.len() > MAX_HEADER_NAME_LENGTH
        || value.len() > MAX_HEADER_VALUE_LENGTH
        || !name.is_ascii()
        || !value.is_ascii()
        || !name.bytes().all(is_http_token_byte)
        || value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
    {
        return Err(E_INVALIDARG);
    }
    Ok(RequestHeader {
        name: name.to_ascii_lowercase(),
        value: value.to_string(),
    })
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn is_transport_or_auth_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "signature" | "host" | "content-length"
    )
}

fn select_policy_header_values(
    headers: &[RequestHeader],
    policy_header_names: &[String],
) -> Vec<String> {
    policy_header_names
        .iter()
        .map(|policy_name| {
            headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case(policy_name))
                .map(|header| header.value.clone())
                .unwrap_or_default()
        })
        .collect()
}

fn signing_policy_for_url(url: &str, headers: &[RequestHeader]) -> SigningPolicy {
    let host = url_host(url).unwrap_or_default();
    if host == "device.mgt.xboxlive.com" || host == "data-vef.xboxlive.com" {
        return SigningPolicy::xbox_full_body();
    }
    if host == "xboxlive.com" || host.ends_with(".xboxlive.com") {
        return SigningPolicy::xbox_default();
    }
    SigningPolicy::caller_headers(headers)
}

fn relying_party_for_url(url: &str) -> &'static str {
    let host = url_host(url).unwrap_or_default();
    if matches!(
        host.as_str(),
        "collections.mp.microsoft.com"
            | "purchase.mp.microsoft.com"
            | "displaycatalog.mp.microsoft.com"
            | "inventory.xboxlive.com"
            | "licensing.xboxlive.com"
    ) {
        LICENSING_RP
    } else if host == "playfabapi.com" || host.ends_with(".playfabapi.com") {
        PLAYFAB_RP
    } else if host == "multiplayer.minecraft.net" || host.ends_with(".multiplayer.minecraft.net") {
        MULTIPLAYER_RP
    } else if matches!(
        host.as_str(),
        "pocket.realms.minecraft.net"
            | "bedrock.frontend.realms.minecraft-services.net"
            | "bedrock.frontendlegacy.realms.minecraft-services.net"
    ) {
        REALMS_RP
    } else {
        XBOX_LIVE_RP
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

fn request_target_from_url(url: &str) -> Option<String> {
    let authority = url.split_once("://")?.1;
    if authority.is_empty() {
        return None;
    }
    let start = authority
        .char_indices()
        .find_map(|(index, character)| matches!(character, '/' | '?' | '#').then_some(index));
    let Some(start) = start else {
        return Some("/".to_string());
    };
    let suffix = &authority[start..];
    if suffix.starts_with('#') {
        return Some("/".to_string());
    }
    let suffix = suffix.split_once('#').map_or(suffix, |(value, _)| value);
    if suffix.starts_with('?') {
        Some(format!("/{suffix}"))
    } else {
        Some(suffix.to_string())
    }
}

fn safe_request_path(target: &str) -> String {
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    let path = path.split_once('#').map_or(path, |(path, _)| path);
    let mut value = path
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect::<String>();
    if value.is_empty() {
        value.push('/');
    }
    value
}

unsafe fn ansi_string_bounded(value: *const c_char, max_bytes: usize) -> Result<String, HResult> {
    if value.is_null() {
        return Err(E_POINTER);
    }
    let value = unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|_| E_INVALIDARG)?;
    if value.is_empty() || value.len() > max_bytes || !value.is_ascii() {
        return Err(E_INVALIDARG);
    }
    Ok(value.to_string())
}

unsafe fn utf16_to_string_bounded(value: *const u16, max_units: usize) -> Result<String, HResult> {
    if value.is_null() {
        return Err(E_POINTER);
    }
    let mut length = 0usize;
    while length <= max_units && length < MAX_UTF16_INPUT_UNITS {
        if unsafe { value.add(length).read() } == 0 {
            let result = String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) })
                .map_err(|_| E_INVALIDARG)?;
            if result.is_empty() || !result.is_ascii() {
                return Err(E_INVALIDARG);
            }
            return Ok(result);
        }
        length += 1;
    }
    Err(E_INVALIDARG)
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, value: &str) -> RequestHeader {
        RequestHeader {
            name: name.to_ascii_lowercase(),
            value: value.to_string(),
        }
    }

    #[test]
    fn known_services_use_specific_tokens() {
        assert_eq!(
            relying_party_for_url("https://userpresence.xboxlive.com/users/xuid(1)"),
            XBOX_LIVE_RP
        );
        assert_eq!(
            relying_party_for_url("https://multiplayer.minecraft.net/authentication"),
            MULTIPLAYER_RP
        );
        assert_eq!(
            relying_party_for_url("https://b980a380.minecraft.playfabapi.com/Client/Login"),
            PLAYFAB_RP
        );
        assert_eq!(
            relying_party_for_url("https://pocket.realms.minecraft.net/worlds"),
            REALMS_RP
        );
    }

    #[test]
    fn xbox_default_policy_does_not_sign_transport_headers() {
        let headers = vec![
            header("x-xbl-contract-version", "3"),
            header("content-type", "application/json"),
        ];
        let policy = signing_policy_for_url(
            "https://userpresence.xboxlive.com/users/xuid(1)/devices/current/titles/current",
            &headers,
        );
        assert_eq!(policy.max_body_bytes, DEFAULT_XBOX_MAX_BODY_BYTES);
        assert!(policy.extra_header_names.is_empty());
    }

    #[test]
    fn header_validation_rejects_injection() {
        assert_eq!(validate_header("x-test", "ok\r\nbad"), Err(E_INVALIDARG));
        assert_eq!(validate_header("bad header", "ok"), Err(E_INVALIDARG));
    }
}

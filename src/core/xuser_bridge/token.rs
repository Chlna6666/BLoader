// SPDX-License-Identifier: GPL-3.0-only

use core::ffi::{c_char, c_void};
use std::{ffi::CStr, mem, ptr};
use zeroize::{Zeroize, Zeroizing};

use super::{
    abi::{
        E_FAIL, E_INVALIDARG, E_NOT_SUFFICIENT_BUFFER, E_POINTER, HResult, S_OK,
        TokenData, TokenHeader, TokenUtf16Data, TokenUtf16Header, XAsyncBlock,
        XAsyncOp, XAsyncProviderData, XUserHandle,
    },
    bridge_debug, bridge_info, bridge_warn, session,
    xasync,
    xuser,
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
        user: XUserHandle,
        method: &str,
        url: &str,
        headers: Vec<RequestHeader>,
        body: &[u8],
        utf16: bool,
    ) -> Result<Self, HResult> {
        if !xuser::valid_user(user) {
            bridge_warn("XUser token/signature 请求被拒绝 | reason=invalid-user-handle");
            return Err(E_INVALIDARG);
        }
        let relying_party = relying_party_for_url(url);
        let runtime = session().ok_or(E_FAIL)?;
        let token = match runtime.token_for_relying_party(relying_party) {
            Some(token) => token,
            None => {
                bridge_warn(&format!(
                    "XUser token/signature 请求无法路由 | rp={relying_party} | reason=no-token-for-relying-party"
                ));
                return Err(E_FAIL);
            }
        };
        let remaining = token.expires_at.saturating_sub(now_epoch());
        if token.expires_at <= now_epoch().saturating_add(30) {
            bridge_warn(&format!(
                "XUser token/signature 请求被拒绝 | rp={relying_party} | token_remaining={}s | reason=token-near-expiry",
                remaining
            ));
            return Err(E_FAIL);
        }
        let request_target = request_target_from_url(url).ok_or(E_INVALIDARG)?;
        let policy = signing_policy_for_url(url, &headers);
        let policy_header_values =
            select_policy_header_values(&headers, &policy.extra_header_names);

        let host = url_host(url).unwrap_or_else(|| "<invalid-host>".to_string());
        let header_names = headers
            .iter()
            .map(|header| header.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        bridge_info(&format!(
            "XUser token/signature request | encoding={} | method={} | host={} | path={} | rp={} | options=validated | header_count={} | header_names=[{}] | body_bytes={} | token_remaining={}s | secrets_logged=false",
            if utf16 { "utf16" } else { "ansi" },
            method.to_ascii_uppercase(),
            host,
            safe_request_path(&request_target),
            relying_party,
            headers.len(),
            header_names,
            body.len(),
            remaining,
        ));
        bridge_debug(&format!(
            "XUser signing policy selected | host={} | max_signed_body_bytes={} | extra_policy_headers={} | authorization=redacted | signature=redacted",
            host,
            policy.max_body_bytes,
            policy.extra_header_names.len(),
        ));

        let authorization_text = Zeroizing::new(format!(
            "XBL3.0 x={};{}",
            token.user_hash, token.token
        ));
        let mut authorization = authorization_text.as_bytes().to_vec();
        authorization.push(0);
        let mut authorization_utf16 = authorization_text.encode_utf16().collect::<Vec<_>>();
        authorization_utf16.push(0);

        Ok(Self {
            utf16,
            method: method.to_ascii_uppercase(),
            request_target,
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
        let signed_body_bytes = body_to_sign.len();
        let policy_header_count = policy_header_values.len();

        let signature_text = Zeroizing::new(
            session()
                .ok_or(E_FAIL)?
                .signing_key
                .sign_request(
                    &self.method,
                    &self.request_target,
                    authorization,
                    &policy_header_values,
                    body_to_sign,
                )?,
        );
        let signature_text_bytes = signature_text.len();
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
            "XUser request signature prepared | encoding={} | method={} | path={} | signed_body_bytes={} | policy_header_count={} | signature_text_bytes={} | signature_body=redacted",
            if self.utf16 { "utf16" } else { "ansi" },
            self.method,
            safe_request_path(&self.request_target),
            signed_body_bytes,
            policy_header_count,
            signature_text_bytes,
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
        XAsyncOp::Begin => {
            bridge_debug("XUser token async provider | op=Begin");
            unsafe { xasync::schedule(provider_data.async_block, 0) }
        }
        XAsyncOp::DoWork => {
            let context = unsafe { &mut *context };
            let completion = context
                .prepare()
                .and_then(|()| context.required_size().ok_or(E_FAIL));
            match completion {
                Ok(required_size) => {
                    bridge_debug(&format!(
                        "XUser token async provider | op=DoWork | result=S_OK | required_bytes={required_size}"
                    ));
                    unsafe { xasync::complete(provider_data.async_block, S_OK, required_size) }
                }
                Err(error) => {
                    bridge_warn(&format!(
                        "XUser token async provider | op=DoWork | result={} | required_bytes=0",
                        format_hresult(error)
                    ));
                    unsafe { xasync::complete(provider_data.async_block, error, 0) }
                }
            }
            S_OK
        }
        XAsyncOp::GetResult => {
            let context = unsafe { &*context };
            let Some(required_size) = context.required_size() else {
                return E_FAIL;
            };
            if provider_data.buffer.is_null() || provider_data.buffer_size < required_size {
                bridge_warn(&format!(
                    "XUser token async provider | op=GetResult | result=E_NOT_SUFFICIENT_BUFFER | provided_bytes={} | required_bytes={required_size}",
                    provider_data.buffer_size
                ));
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
            bridge_debug(&format!(
                "XUser token async provider | op=GetResult | result=S_OK | encoding={} | output_bytes={} | authorization=redacted | signature=redacted",
                if context.utf16 { "utf16" } else { "ansi" },
                required_size,
            ));
            S_OK
        }
        XAsyncOp::Cancel => {
            bridge_debug("XUser token async provider | op=Cancel | result=S_OK");
            S_OK
        }
        XAsyncOp::Cleanup => {
            bridge_debug("XUser token async provider | op=Cleanup");
            unsafe {
                drop(Box::from_raw(context));
            }
            S_OK
        }
    }
}

unsafe fn begin_token_request(
    user: XUserHandle,
    options: u32,
    method: &str,
    url: &str,
    headers: Vec<RequestHeader>,
    body_size: usize,
    body: *const c_void,
    async_block: *mut XAsyncBlock,
    utf16: bool,
) -> HResult {
    if user.is_null() || async_block.is_null() || (body_size != 0 && body.is_null()) {
        bridge_warn("XUser token/signature 请求参数无效 | result=E_POINTER");
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
        bridge_warn(&format!(
            "XUser token/signature 请求参数校验失败 | method_len={} | url_len={} | body_bytes={} | options=0x{options:08X} | result=E_INVALIDARG",
            method.len(),
            url.len(),
            body_size,
        ));
        return E_INVALIDARG;
    }

    let body = if body_size == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(body.cast::<u8>(), body_size) }
    };
    let context = match TokenContext::new(user, method, url, headers, body, utf16) {
        Ok(context) => Box::into_raw(Box::new(context)),
        Err(error) => {
            bridge_warn(&format!(
                "XUser token/signature context 创建失败 | result={}",
                format_hresult(error)
            ));
            return error;
        }
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
        bridge_warn(&format!(
            "XUser token/signature async begin 失败 | encoding={} | result={}",
            if utf16 { "utf16" } else { "ansi" },
            format_hresult(result)
        ));
        unsafe {
            drop(Box::from_raw(context));
        }
    } else {
        bridge_debug(&format!(
            "XUser token/signature async begin 成功 | encoding={} | result={}",
            if utf16 { "utf16" } else { "ansi" },
            format_hresult(result)
        ));
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
    if method.is_null()
        || url.is_null()
        || header_count > MAX_HEADER_COUNT
        || (header_count != 0 && headers.is_null())
    {
        return E_POINTER;
    }

    let headers = match unsafe { copy_ansi_headers(headers, header_count) } {
        Ok(headers) => headers,
        Err(error) => return error,
    };
    let method = match unsafe { CStr::from_ptr(method) }.to_str() {
        Ok(value) if value.len() <= MAX_METHOD_LENGTH && value.is_ascii() => value,
        _ => return E_INVALIDARG,
    };
    let url = match unsafe { CStr::from_ptr(url) }.to_str() {
        Ok(value) if value.len() <= MAX_URL_LENGTH && value.is_ascii() => value,
        _ => return E_INVALIDARG,
    };
    unsafe {
        begin_token_request(
            user,
            options,
            method,
            url,
            headers,
            body_size,
            body,
            async_block,
            false,
        )
    }
}

pub unsafe extern "system" fn get_token_and_signature_result_size(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: *mut usize,
) -> HResult {
    let result = unsafe { xasync::get_result_size(async_block, size) };
    if result < 0 {
        bridge_warn(&format!(
            "XUser token/signature result-size 失败 | result={}",
            format_hresult(result)
        ));
    } else if !size.is_null() {
        bridge_debug(&format!(
            "XUser token/signature result-size | required_bytes={}",
            unsafe { *size }
        ));
    }
    result
}

pub unsafe extern "system" fn get_token_and_signature_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenData,
    used: *mut usize,
) -> HResult {
    if async_block.is_null() || buffer.is_null() || data.is_null() {
        return E_POINTER;
    }
    let result =
        unsafe { xasync::get_result(async_block, token_identity(false), size, buffer, used) };
    if result >= 0 {
        unsafe {
            data.write(buffer.cast());
        }
        bridge_debug(&format!(
            "XUser token/signature result | encoding=ansi | result={} | buffer_bytes={} | used_bytes={} | authorization=redacted | signature=redacted",
            format_hresult(result),
            size,
            if used.is_null() { 0 } else { unsafe { *used } },
        ));
    } else {
        bridge_warn(&format!(
            "XUser token/signature result 失败 | encoding=ansi | result={} | buffer_bytes={size}",
            format_hresult(result)
        ));
    }
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
    if header_count > MAX_HEADER_COUNT || (header_count != 0 && headers.is_null()) {
        return E_POINTER;
    }

    let headers = match unsafe { copy_utf16_headers(headers, header_count) } {
        Ok(headers) => headers,
        Err(error) => return error,
    };
    let method = match unsafe { utf16_to_string_bounded(method, MAX_METHOD_LENGTH) } {
        Ok(value) if value.is_ascii() => value,
        Ok(_) => return E_INVALIDARG,
        Err(error) => return error,
    };
    let url = match unsafe { utf16_to_string_bounded(url, MAX_URL_LENGTH) } {
        Ok(value) if value.is_ascii() => value,
        Ok(_) => return E_INVALIDARG,
        Err(error) => return error,
    };
    unsafe {
        begin_token_request(
            user,
            options,
            &method,
            &url,
            headers,
            body_size,
            body,
            async_block,
            true,
        )
    }
}

pub unsafe extern "system" fn get_token_and_signature_utf16_result_size(
    interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: *mut usize,
) -> HResult {
    unsafe { get_token_and_signature_result_size(interface, async_block, size) }
}

pub unsafe extern "system" fn get_token_and_signature_utf16_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: usize,
    buffer: *mut c_void,
    data: *mut *mut TokenUtf16Data,
    used: *mut usize,
) -> HResult {
    if async_block.is_null() || buffer.is_null() || data.is_null() {
        return E_POINTER;
    }
    let result =
        unsafe { xasync::get_result(async_block, token_identity(true), size, buffer, used) };
    if result >= 0 {
        unsafe {
            data.write(buffer.cast());
        }
        bridge_debug(&format!(
            "XUser token/signature result | encoding=utf16 | result={} | buffer_bytes={} | used_bytes={} | authorization=redacted | signature=redacted",
            format_hresult(result),
            size,
            if used.is_null() { 0 } else { unsafe { *used } },
        ));
    } else {
        bridge_warn(&format!(
            "XUser token/signature result 失败 | encoding=utf16 | result={} | buffer_bytes={size}",
            format_hresult(result)
        ));
    }
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

fn format_hresult(result: HResult) -> String {
    format!("0x{:08X}", result as u32)
}

unsafe fn utf16_to_string_bounded(value: *const u16, max_units: usize) -> Result<String, HResult> {
    if value.is_null() {
        return Err(E_POINTER);
    }
    let mut length = 0usize;
    while length <= max_units && length < MAX_UTF16_INPUT_UNITS {
        if unsafe { value.add(length).read() } == 0 {
            return String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) })
                .map_err(|_| E_INVALIDARG);
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
    fn presence_uses_xbox_live_relying_party() {
        assert_eq!(
            relying_party_for_url(
                "https://userpresence.xboxlive.com/users/xuid(1)/devices/current"
            ),
            XBOX_LIVE_RP
        );
    }

    #[test]
    fn known_services_use_specific_tokens() {
        assert_eq!(
            relying_party_for_url("https://multiplayer.minecraft.net/authentication"),
            MULTIPLAYER_RP
        );
        assert_eq!(
            relying_party_for_url("https://b980a380.minecraft.playfabapi.com/Client/Login"),
            PLAYFAB_RP
        );
    }

    #[test]
    fn xbox_default_policy_does_not_sign_transport_headers() {
        let headers = vec![
            header("x-xbl-contract-version", "3"),
            header("content-type", "application/json"),
            header("accept-language", "zh-CN"),
        ];
        let policy = signing_policy_for_url(
            "https://userpresence.xboxlive.com/users/xuid(1)/devices/current/titles/current",
            &headers,
        );
        assert_eq!(policy.max_body_bytes, DEFAULT_XBOX_MAX_BODY_BYTES);
        assert!(policy.extra_header_names.is_empty());
    }

    #[test]
    fn policy_headers_are_case_insensitive_ordered_and_keep_missing_slots() {
        let headers = vec![
            header("content-type", "application/json"),
            header("x-xbl-contract-version", "3"),
        ];
        let policy_names = vec![
            "X-XBL-CONTRACT-VERSION".to_string(),
            "Accept-Language".to_string(),
            "Content-Type".to_string(),
        ];
        assert_eq!(
            select_policy_header_values(&headers, &policy_names),
            vec!["3", "", "application/json"]
        );
    }

    #[test]
    fn custom_endpoint_fallback_preserves_caller_header_order() {
        let headers = vec![
            header("content-type", "application/json"),
            header("authorization", "ignored"),
            header("x-custom-policy", "value"),
        ];
        let policy = signing_policy_for_url("https://api.example.test/path", &headers);
        assert_eq!(
            policy.extra_header_names,
            vec!["content-type", "x-custom-policy"]
        );
    }

    #[test]
    fn header_validation_rejects_injection() {
        assert_eq!(validate_header("x-test", "ok\r\nbad"), Err(E_INVALIDARG));
        assert_eq!(validate_header("bad header", "ok"), Err(E_INVALIDARG));
    }

    #[test]
    fn log_path_drops_query_parameters() {
        assert_eq!(safe_request_path("/path/to/resource?secret=value"), "/path/to/resource");
    }
}

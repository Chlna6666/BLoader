// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::{c_char, c_void};
use std::{ffi::CStr, mem, ptr};
use zeroize::{Zeroize, Zeroizing};

use super::{
    abi::{
        E_FAIL, E_INVALIDARG, E_NOT_SUFFICIENT_BUFFER, E_POINTER, HResult, S_OK,
        TokenData, TokenHeader, TokenUtf16Data, TokenUtf16Header, XAsyncBlock,
        XAsyncOp, XAsyncProviderData, XUserHandle,
    },
    session,
    xasync,
    xuser,
};

const TOKEN_OPTIONS_MASK: u32 = 0x03;
const MAX_METHOD_LENGTH: usize = 32;
const MAX_URL_LENGTH: usize = 32 * 1024;
const MAX_HEADER_COUNT: usize = 128;
const MAX_REQUEST_BODY_SIZE: usize = 64 * 1024 * 1024;
const MAX_UTF16_INPUT_UNITS: usize = 32 * 1024;
const TOKEN_NAME_ANSI: &[u8] = b"XUserGetTokenAndSignatureAsync\0";
const TOKEN_NAME_UTF16: &[u8] = b"XUserGetTokenAndSignatureUtf16Async\0";
static TOKEN_IDENTITY_ANSI: u8 = 0x41;
static TOKEN_IDENTITY_UTF16: u8 = 0x57;

const XBOX_LIVE_RP: &str = "http://xboxlive.com";
const PLAYFAB_RP: &str = "https://b980a380.minecraft.playfabapi.com/";
const MULTIPLAYER_RP: &str = "https://multiplayer.minecraft.net/";
const REALMS_RP: &str = "https://pocket.realms.minecraft.net/";
const LICENSING_RP: &str = "http://licensing.xboxlive.com";

struct TokenContext {
    utf16: bool,
    method: String,
    request_target: String,
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
        body: &[u8],
        utf16: bool,
    ) -> Result<Self, HResult> {
        if !xuser::valid_user(user) {
            return Err(E_INVALIDARG);
        }
        let relying_party = relying_party_for_url(url);
        let runtime = session().ok_or(E_FAIL)?;
        let token = runtime
            .token_for_relying_party(relying_party)
            .ok_or(E_FAIL)?;
        if token.expires_at <= now_epoch().saturating_add(30) {
            return Err(E_FAIL);
        }
        let request_target = request_target_from_url(url).ok_or(E_INVALIDARG)?;

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

        let signature_text = Zeroizing::new(
            session()
                .ok_or(E_FAIL)?
                .signing_key
                .sign_request(
                    &self.method,
                    &self.request_target,
                    authorization,
                    &[],
                    &self.body,
                )?,
        );
        self.body.zeroize();
        self.body.clear();

        self.signature = signature_text.as_bytes().to_vec();
        self.signature.push(0);
        self.signature_utf16 = signature_text.encode_utf16().collect();
        self.signature_utf16.push(0);
        self.prepared = true;
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
    body_size: usize,
    body: *const c_void,
    async_block: *mut XAsyncBlock,
    utf16: bool,
) -> HResult {
    if user.is_null() || async_block.is_null() || (body_size != 0 && body.is_null()) {
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
    let context = match TokenContext::new(user, method, url, body, utf16) {
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
        unsafe {
            drop(Box::from_raw(context));
        }
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
    if header_count != 0 {
        for header in unsafe { std::slice::from_raw_parts(headers, header_count) } {
            if header.name.is_null() || header.value.is_null() {
                return E_POINTER;
            }
            let _ = unsafe { CStr::from_ptr(header.name) };
            let _ = unsafe { CStr::from_ptr(header.value) };
        }
    }

    let method = match unsafe { CStr::from_ptr(method) }.to_str() {
        Ok(value) => value,
        Err(_) => return E_INVALIDARG,
    };
    let url = match unsafe { CStr::from_ptr(url) }.to_str() {
        Ok(value) => value,
        Err(_) => return E_INVALIDARG,
    };
    unsafe {
        begin_token_request(
            user,
            options,
            method,
            url,
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
    unsafe { xasync::get_result_size(async_block, size) }
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
    if header_count != 0 {
        for header in unsafe { std::slice::from_raw_parts(headers, header_count) } {
            if let Err(error) = unsafe { utf16_to_string(header.name) } {
                return error;
            }
            if let Err(error) = unsafe { utf16_to_string(header.value) } {
                return error;
            }
        }
    }

    let method = match unsafe { utf16_to_string(method) } {
        Ok(value) => value,
        Err(error) => return error,
    };
    let url = match unsafe { utf16_to_string(url) } {
        Ok(value) => value,
        Err(error) => return error,
    };
    unsafe {
        begin_token_request(
            user,
            options,
            &method,
            &url,
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
    }
    result
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

unsafe fn utf16_to_string(value: *const u16) -> Result<String, HResult> {
    if value.is_null() {
        return Err(E_POINTER);
    }
    let mut length = 0usize;
    while length < MAX_UTF16_INPUT_UNITS {
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
}

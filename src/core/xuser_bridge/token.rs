// SPDX-License-Identifier: GPL-3.0-only

use core::ffi::{c_char, c_void};
use minhook::MinHook;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    mem, ptr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use super::{
    abi::{
        E_FAIL, E_INVALIDARG, HResult, TokenData, TokenHeader, TokenUtf16Data,
        TokenUtf16Header, XAsyncBlock, XUserHandle,
    },
    bridge_debug, bridge_info, bridge_warn, session, xuser,
};

const ERROR_INVALID_DATA: u32 = 13;
const REWRITE_TICKET_TTL: Duration = Duration::from_secs(10);

static TOKEN_BRIDGE_INIT: OnceLock<Result<(), String>> = OnceLock::new();
static ORIGINAL_BCRYPT_HASH_DATA: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_BCRYPT_FINISH_HASH: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_BCRYPT_DESTROY_HASH: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WINHTTP_SEND_REQUEST: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WINHTTP_WRITE_DATA: AtomicUsize = AtomicUsize::new(0);
static HASH_REWRITES: OnceLock<Mutex<HashMap<usize, [u8; 32]>>> = OnceLock::new();
static REWRITE_TICKETS: OnceLock<Mutex<VecDeque<RewriteTicket>>> = OnceLock::new();

#[derive(Clone, Copy)]
struct RewriteTicket {
    fingerprint: [u8; 32],
    issued_at: Instant,
}

struct BodyRewrite {
    bytes: Vec<u8>,
    native_fingerprint: [u8; 32],
    native_token_len: usize,
    custom_token_len: usize,
}

type BCryptHashDataFn = unsafe extern "system" fn(*mut c_void, *mut u8, u32, u32) -> i32;
type BCryptFinishHashFn = unsafe extern "system" fn(*mut c_void, *mut u8, u32, u32) -> i32;
type BCryptDestroyHashFn = unsafe extern "system" fn(*mut c_void) -> i32;
type WinHttpSendRequestFn = unsafe extern "system" fn(
    *mut c_void,
    *const u16,
    u32,
    *mut c_void,
    u32,
    u32,
    usize,
) -> i32;
type WinHttpWriteDataFn =
    unsafe extern "system" fn(*mut c_void, *const c_void, u32, *mut u32) -> i32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn LoadLibraryW(file_name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn SetLastError(error: u32);
}

pub(crate) fn initialize_native_token_bridge() -> Result<(), String> {
    TOKEN_BRIDGE_INIT
        .get_or_init(|| unsafe { install_native_token_bridge() })
        .clone()
}

unsafe fn install_native_token_bridge() -> Result<(), String> {
    let bcrypt = unsafe { load_module("bcrypt.dll") }?;
    let winhttp = unsafe { load_module("winhttp.dll") }?;

    unsafe {
        hook_export(
            bcrypt,
            b"BCryptHashData\0",
            bcrypt_hash_data_hook as *mut c_void,
            &ORIGINAL_BCRYPT_HASH_DATA,
        )?;
        hook_export(
            bcrypt,
            b"BCryptFinishHash\0",
            bcrypt_finish_hash_hook as *mut c_void,
            &ORIGINAL_BCRYPT_FINISH_HASH,
        )?;
        hook_export(
            bcrypt,
            b"BCryptDestroyHash\0",
            bcrypt_destroy_hash_hook as *mut c_void,
            &ORIGINAL_BCRYPT_DESTROY_HASH,
        )?;
        hook_export(
            winhttp,
            b"WinHttpSendRequest\0",
            winhttp_send_request_hook as *mut c_void,
            &ORIGINAL_WINHTTP_SEND_REQUEST,
        )?;
        hook_export(
            winhttp,
            b"WinHttpWriteData\0",
            winhttp_write_data_hook as *mut c_void,
            &ORIGINAL_WINHTTP_WRITE_DATA,
        )?;
        MinHook::enable_all_hooks()
            .map_err(|status| format!("enable XSTS UToken hooks failed: {status:?}"))?;
    }

    bridge_info(
        "官方 XSTS UToken 注入桥已安装 | stages=BCryptHashData+WinHttpSendRequest/WriteData | mutation=UserTokens-only | final_xsts=official | signature=official",
    );
    Ok(())
}

unsafe fn load_module(name: &str) -> Result<*mut c_void, String> {
    let wide = wide(name);
    let mut module = unsafe { GetModuleHandleW(wide.as_ptr()) };
    if module.is_null() {
        module = unsafe { LoadLibraryW(wide.as_ptr()) };
    }
    if module.is_null() {
        Err(format!("failed to load {name}"))
    } else {
        Ok(module)
    }
}

unsafe fn hook_export(
    module: *mut c_void,
    name: &[u8],
    detour: *mut c_void,
    storage: &AtomicUsize,
) -> Result<(), String> {
    let target = unsafe { GetProcAddress(module, name.as_ptr()) };
    if target.is_null() {
        return Err(format!(
            "required native export is unavailable: {}",
            String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
        ));
    }
    let original = unsafe { MinHook::create_hook(target, detour) }
        .map_err(|status| format!("MinHook create failed: {status:?}"))?;
    storage.store(original as usize, Ordering::Release);
    Ok(())
}

unsafe extern "system" fn bcrypt_hash_data_hook(
    hash: *mut c_void,
    input: *mut u8,
    input_len: u32,
    flags: u32,
) -> i32 {
    let original: BCryptHashDataFn = unsafe { original_fn(&ORIGINAL_BCRYPT_HASH_DATA) };
    if hash.is_null() || input.is_null() || input_len == 0 {
        return unsafe { original(hash, input, input_len, flags) };
    }

    let source = unsafe { std::slice::from_raw_parts(input.cast_const(), input_len as usize) };
    if let Some(rewrite) = rewrite_xsts_user_token(source) {
        let rewritten_len = match u32::try_from(rewrite.bytes.len()) {
            Ok(value) => value,
            Err(_) => return unsafe { original(hash, input, input_len, flags) },
        };
        HASH_REWRITES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(hash as usize, rewrite.native_fingerprint);
        bridge_debug(&format!(
            "官方 XSTS 签名输入已替换 UToken | native_bytes={} | custom_bytes={} | stage=BCryptHashData | token_body=redacted",
            rewrite.native_token_len, rewrite.custom_token_len,
        ));
        return unsafe {
            original(
                hash,
                rewrite.bytes.as_ptr() as *mut u8,
                rewritten_len,
                flags,
            )
        };
    }

    unsafe { original(hash, input, input_len, flags) }
}

unsafe extern "system" fn bcrypt_finish_hash_hook(
    hash: *mut c_void,
    output: *mut u8,
    output_len: u32,
    flags: u32,
) -> i32 {
    let original: BCryptFinishHashFn = unsafe { original_fn(&ORIGINAL_BCRYPT_FINISH_HASH) };
    let status = unsafe { original(hash, output, output_len, flags) };
    let fingerprint = HASH_REWRITES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(hash as usize));
    if status >= 0 {
        if let Some(fingerprint) = fingerprint {
            let mut tickets = REWRITE_TICKETS
                .get_or_init(|| Mutex::new(VecDeque::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            purge_expired_tickets(&mut tickets);
            tickets.push_back(RewriteTicket {
                fingerprint,
                issued_at: Instant::now(),
            });
            bridge_debug("官方 XSTS UToken 签名票据已生成 | signature=official | token_body=redacted");
        }
    }
    status
}

unsafe extern "system" fn bcrypt_destroy_hash_hook(hash: *mut c_void) -> i32 {
    HASH_REWRITES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(hash as usize));
    let original: BCryptDestroyHashFn = unsafe { original_fn(&ORIGINAL_BCRYPT_DESTROY_HASH) };
    unsafe { original(hash) }
}

unsafe extern "system" fn winhttp_send_request_hook(
    request: *mut c_void,
    headers: *const u16,
    headers_len: u32,
    optional: *mut c_void,
    optional_len: u32,
    total_len: u32,
    context: usize,
) -> i32 {
    let original: WinHttpSendRequestFn = unsafe { original_fn(&ORIGINAL_WINHTTP_SEND_REQUEST) };
    if optional.is_null() || optional_len == 0 {
        return unsafe {
            original(
                request, headers, headers_len, optional, optional_len, total_len, context,
            )
        };
    }

    let source = unsafe { std::slice::from_raw_parts(optional.cast::<u8>(), optional_len as usize) };
    let Some(rewrite) = rewrite_xsts_user_token(source) else {
        return unsafe {
            original(
                request, headers, headers_len, optional, optional_len, total_len, context,
            )
        };
    };
    if !consume_rewrite_ticket(rewrite.native_fingerprint) {
        bridge_warn(
            "阻止未配对的 XSTS UToken HTTP 重写 | reason=no-matching-official-signature-ticket | action=fail-closed",
        );
        unsafe { SetLastError(ERROR_INVALID_DATA) };
        return 0;
    }
    let rewritten_len = match u32::try_from(rewrite.bytes.len()) {
        Ok(value) => value,
        Err(_) => {
            unsafe { SetLastError(ERROR_INVALID_DATA) };
            return 0;
        }
    };
    let rewritten_total = total_len
        .checked_sub(optional_len)
        .and_then(|base| base.checked_add(rewritten_len))
        .unwrap_or(rewritten_len);
    bridge_info(&format!(
        "官方 XSTS HTTP 请求已替换 UToken | native_bytes={} | custom_bytes={} | stage=WinHttpSendRequest | DToken=preserved | TToken=preserved | signature=official | token_body=redacted",
        rewrite.native_token_len, rewrite.custom_token_len,
    ));
    unsafe {
        original(
            request,
            headers,
            headers_len,
            rewrite.bytes.as_ptr() as *mut c_void,
            rewritten_len,
            rewritten_total,
            context,
        )
    }
}

unsafe extern "system" fn winhttp_write_data_hook(
    request: *mut c_void,
    buffer: *const c_void,
    bytes_to_write: u32,
    bytes_written: *mut u32,
) -> i32 {
    let original: WinHttpWriteDataFn = unsafe { original_fn(&ORIGINAL_WINHTTP_WRITE_DATA) };
    if buffer.is_null() || bytes_to_write == 0 {
        return unsafe { original(request, buffer, bytes_to_write, bytes_written) };
    }
    let source = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), bytes_to_write as usize) };
    let Some(rewrite) = rewrite_xsts_user_token(source) else {
        return unsafe { original(request, buffer, bytes_to_write, bytes_written) };
    };
    if rewrite.bytes.len() != source.len() {
        bridge_warn(
            "阻止流式 XSTS UToken 重写 | reason=token-length-changed-after-total-length-committed | action=fail-closed",
        );
        unsafe { SetLastError(ERROR_INVALID_DATA) };
        return 0;
    }
    if !consume_rewrite_ticket(rewrite.native_fingerprint) {
        bridge_warn(
            "阻止未配对的流式 XSTS UToken 重写 | reason=no-matching-official-signature-ticket | action=fail-closed",
        );
        unsafe { SetLastError(ERROR_INVALID_DATA) };
        return 0;
    }
    bridge_info(
        "官方流式 XSTS HTTP 请求已替换 UToken | stage=WinHttpWriteData | signature=official | token_body=redacted",
    );
    unsafe {
        original(
            request,
            rewrite.bytes.as_ptr().cast(),
            bytes_to_write,
            bytes_written,
        )
    }
}

fn rewrite_xsts_user_token(source: &[u8]) -> Option<BodyRewrite> {
    let runtime = session()?;
    let custom = runtime.custom_user_token()?;
    let marker = find_bytes(source, b"\"UserTokens\"")?;
    if find_bytes(source, b"\"RelyingParty\"").is_none()
        || find_bytes(source, b"\"DeviceToken\"").is_none()
        || find_bytes(source, b"\"TitleToken\"").is_none()
    {
        return None;
    }

    let array_start = source[marker..]
        .iter()
        .position(|byte| *byte == b'[')?
        .checked_add(marker)?;
    let token_start = source[array_start + 1..]
        .iter()
        .position(|byte| *byte == b'"')?
        .checked_add(array_start + 2)?;
    let token_end = source[token_start..]
        .iter()
        .position(|byte| *byte == b'"')?
        .checked_add(token_start)?;
    if token_end <= token_start {
        return None;
    }

    let native = &source[token_start..token_end];
    if native == custom.as_bytes() {
        return None;
    }
    let native_fingerprint: [u8; 32] = Sha256::digest(native).into();
    let mut bytes = Vec::with_capacity(
        source.len()
            .saturating_sub(native.len())
            .saturating_add(custom.len()),
    );
    bytes.extend_from_slice(&source[..token_start]);
    bytes.extend_from_slice(custom.as_bytes());
    bytes.extend_from_slice(&source[token_end..]);
    Some(BodyRewrite {
        bytes,
        native_fingerprint,
        native_token_len: native.len(),
        custom_token_len: custom.len(),
    })
}

fn consume_rewrite_ticket(fingerprint: [u8; 32]) -> bool {
    let mut tickets = REWRITE_TICKETS
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    purge_expired_tickets(&mut tickets);
    let Some(position) = tickets
        .iter()
        .position(|ticket| ticket.fingerprint == fingerprint)
    else {
        return false;
    };
    tickets.remove(position);
    true
}

fn purge_expired_tickets(tickets: &mut VecDeque<RewriteTicket>) {
    let now = Instant::now();
    tickets.retain(|ticket| now.duration_since(ticket.issued_at) <= REWRITE_TICKET_TTL);
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

unsafe fn original_fn<T>(storage: &AtomicUsize) -> T
where
    T: Copy,
{
    let address = storage.load(Ordering::Acquire);
    debug_assert_ne!(address, 0);
    unsafe { mem::transmute_copy(&address) }
}

fn native_token_interface() -> Result<*mut c_void, HResult> {
    xuser::native_base_interface().ok_or(E_FAIL)
}

fn native_token_slot(index: usize) -> Result<usize, HResult> {
    xuser::native_base_slot(index).ok_or(E_FAIL)
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
    if let Err(error) = initialize_native_token_bridge() {
        bridge_warn(&format!(
            "官方 XSTS UToken 注入桥初始化失败 | reason={error} | action=fail-closed"
        ));
        return E_FAIL;
    }
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
    let function: Function = unsafe { mem::transmute(native_token_slot(23)?) };
    unsafe {
        function(
            native_token_interface()?,
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
    }
}

pub unsafe extern "system" fn get_token_and_signature_result_size(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: *mut usize,
) -> HResult {
    type Function = unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut usize) -> HResult;
    let function: Function = unsafe { mem::transmute(native_token_slot(24)?) };
    unsafe { function(native_token_interface()?, async_block, size) }
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
    let function: Function = unsafe { mem::transmute(native_token_slot(25)?) };
    unsafe {
        function(
            native_token_interface()?,
            async_block,
            size,
            buffer,
            data,
            used,
        )
    }
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
    if let Err(error) = initialize_native_token_bridge() {
        bridge_warn(&format!(
            "官方 UTF16 XSTS UToken 注入桥初始化失败 | reason={error} | action=fail-closed"
        ));
        return E_FAIL;
    }
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
    let function: Function = unsafe { mem::transmute(native_token_slot(26)?) };
    unsafe {
        function(
            native_token_interface()?,
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
    }
}

pub unsafe extern "system" fn get_token_and_signature_utf16_result_size(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: *mut usize,
) -> HResult {
    type Function = unsafe extern "system" fn(*mut c_void, *mut XAsyncBlock, *mut usize) -> HResult;
    let function: Function = unsafe { mem::transmute(native_token_slot(27)?) };
    unsafe { function(native_token_interface()?, async_block, size) }
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
    let function: Function = unsafe { mem::transmute(native_token_slot(28)?) };
    unsafe {
        function(
            native_token_interface()?,
            async_block,
            size,
            buffer,
            data,
            used,
        )
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xsts_user_token_rewrite_requires_device_and_title_context() {
        assert!(find_bytes(b"{\"UserTokens\":[]}", b"\"UserTokens\"").is_some());
        assert!(find_bytes(b"{}", b"\"UserTokens\"").is_none());
    }
}

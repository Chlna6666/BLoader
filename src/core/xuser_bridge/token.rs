// SPDX-License-Identifier: GPL-3.0-only

use core::ffi::{c_char, c_void};
use minhook::MinHook;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    ffi::CStr,
    mem,
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
const XUSER_TOKEN_FORCE_REFRESH: u32 = 0x01;
const REWRITE_TICKET_TTL: Duration = Duration::from_secs(10);
const MAX_URL_BYTES: usize = 32 * 1024;

const XBOX_LIVE_RP: &str = "http://xboxlive.com";
const PLAYFAB_RP: &str = "https://b980a380.minecraft.playfabapi.com/";
const MULTIPLAYER_RP: &str = "https://multiplayer.minecraft.net/";
const REALMS_RP: &str = "https://pocket.realms.minecraft.net/";
const LICENSING_RP: &str = "http://licensing.xboxlive.com";

static TOKEN_BRIDGE_INIT: OnceLock<Result<(), String>> = OnceLock::new();
static ORIGINAL_BCRYPT_HASH_DATA: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_BCRYPT_FINISH_HASH: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_BCRYPT_DESTROY_HASH: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WINHTTP_SEND_REQUEST: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WINHTTP_WRITE_DATA: AtomicUsize = AtomicUsize::new(0);
static HASH_REWRITES: OnceLock<Mutex<HashMap<usize, HashRewrite>>> = OnceLock::new();
static REWRITE_TICKETS: OnceLock<Mutex<VecDeque<RewriteTicket>>> = OnceLock::new();
static RP_REWRITE_GENERATIONS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
static PENDING_TOKEN_REQUESTS: OnceLock<Mutex<HashMap<usize, PendingTokenRequest>>> = OnceLock::new();

#[derive(Clone)]
struct HashRewrite {
    fingerprint: [u8; 32],
    relying_party: String,
}

#[derive(Clone)]
struct RewriteTicket {
    fingerprint: [u8; 32],
    relying_party: String,
    issued_at: Instant,
}

struct BodyRewrite {
    bytes: Vec<u8>,
    native_fingerprint: [u8; 32],
    relying_party: String,
    native_token_len: usize,
    custom_token_len: usize,
    changed: bool,
}

struct PendingTokenRequest {
    relying_party: String,
    start_generation: u64,
    require_rewrite: bool,
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

macro_rules! hresult_try {
    ($expression:expr) => {
        match $expression {
            Ok(value) => value,
            Err(error) => return error,
        }
    };
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
            bcrypt_hash_data_hook as *const () as *mut c_void,
            &ORIGINAL_BCRYPT_HASH_DATA,
        )?;
        hook_export(
            bcrypt,
            b"BCryptFinishHash\0",
            bcrypt_finish_hash_hook as *const () as *mut c_void,
            &ORIGINAL_BCRYPT_FINISH_HASH,
        )?;
        hook_export(
            bcrypt,
            b"BCryptDestroyHash\0",
            bcrypt_destroy_hash_hook as *const () as *mut c_void,
            &ORIGINAL_BCRYPT_DESTROY_HASH,
        )?;
        hook_export(
            winhttp,
            b"WinHttpSendRequest\0",
            winhttp_send_request_hook as *const () as *mut c_void,
            &ORIGINAL_WINHTTP_SEND_REQUEST,
        )?;
        hook_export(
            winhttp,
            b"WinHttpWriteData\0",
            winhttp_write_data_hook as *const () as *mut c_void,
            &ORIGINAL_WINHTTP_WRITE_DATA,
        )?;
        MinHook::enable_all_hooks()
            .map_err(|status| format!("enable XSTS UToken hooks failed: {status:?}"))?;
    }

    bridge_info(
        "官方 XSTS UToken 注入桥已安装 | stages=BCryptHashData+WinHttpSendRequest/WriteData | mutation=UserTokens-only | cache_guard=per-relying-party | final_xsts=official | signature=official",
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

    let source = unsafe { std::slice::from_raw_parts(input as *const u8, input_len as usize) };
    if let Some(rewrite) = rewrite_xsts_user_token(source) {
        let rewritten_len = match u32::try_from(rewrite.bytes.len()) {
            Ok(value) => value,
            Err(_) => return unsafe { original(hash, input, input_len, flags) },
        };
        HASH_REWRITES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                hash as usize,
                HashRewrite {
                    fingerprint: rewrite.native_fingerprint,
                    relying_party: rewrite.relying_party.clone(),
                },
            );
        bridge_debug(&format!(
            "官方 XSTS 签名输入已绑定自定义 UToken | rp={} | native_bytes={} | custom_bytes={} | changed={} | stage=BCryptHashData | token_body=redacted",
            rewrite.relying_party,
            rewrite.native_token_len,
            rewrite.custom_token_len,
            rewrite.changed,
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
    let rewrite = HASH_REWRITES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(hash as usize));
    if status >= 0 {
        if let Some(rewrite) = rewrite {
            let mut tickets = REWRITE_TICKETS
                .get_or_init(|| Mutex::new(VecDeque::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            purge_expired_tickets(&mut tickets);
            tickets.push_back(RewriteTicket {
                fingerprint: rewrite.fingerprint,
                relying_party: rewrite.relying_party.clone(),
                issued_at: Instant::now(),
            });
            bridge_debug(&format!(
                "官方 XSTS UToken 签名票据已生成 | rp={} | signature=official | token_body=redacted",
                rewrite.relying_party,
            ));
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

    let source = unsafe { std::slice::from_raw_parts(optional as *const u8, optional_len as usize) };
    let Some(rewrite) = rewrite_xsts_user_token(source) else {
        return unsafe {
            original(
                request, headers, headers_len, optional, optional_len, total_len, context,
            )
        };
    };
    if !consume_rewrite_ticket(rewrite.native_fingerprint, &rewrite.relying_party) {
        bridge_warn(&format!(
            "阻止未配对的 XSTS UToken HTTP 重写 | rp={} | reason=no-matching-official-signature-ticket | action=fail-closed",
            rewrite.relying_party,
        ));
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
    let result = unsafe {
        original(
            request,
            headers,
            headers_len,
            rewrite.bytes.as_ptr() as *mut c_void,
            rewritten_len,
            rewritten_total,
            context,
        )
    };
    if result != 0 {
        note_successful_rewrite(&rewrite.relying_party);
        bridge_info(&format!(
            "官方 XSTS HTTP 请求已绑定自定义 UToken | rp={} | native_bytes={} | custom_bytes={} | changed={} | stage=WinHttpSendRequest | DToken=preserved | TToken=preserved | signature=official | token_body=redacted",
            rewrite.relying_party,
            rewrite.native_token_len,
            rewrite.custom_token_len,
            rewrite.changed,
        ));
    }
    result
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
    let source = unsafe { std::slice::from_raw_parts(buffer as *const u8, bytes_to_write as usize) };
    let Some(rewrite) = rewrite_xsts_user_token(source) else {
        return unsafe { original(request, buffer, bytes_to_write, bytes_written) };
    };
    if rewrite.bytes.len() != source.len() {
        bridge_warn(&format!(
            "阻止流式 XSTS UToken 重写 | rp={} | reason=token-length-changed-after-total-length-committed | action=fail-closed",
            rewrite.relying_party,
        ));
        unsafe { SetLastError(ERROR_INVALID_DATA) };
        return 0;
    }
    if !consume_rewrite_ticket(rewrite.native_fingerprint, &rewrite.relying_party) {
        bridge_warn(&format!(
            "阻止未配对的流式 XSTS UToken 重写 | rp={} | reason=no-matching-official-signature-ticket | action=fail-closed",
            rewrite.relying_party,
        ));
        unsafe { SetLastError(ERROR_INVALID_DATA) };
        return 0;
    }
    let result = unsafe {
        original(
            request,
            rewrite.bytes.as_ptr() as *const c_void,
            bytes_to_write,
            bytes_written,
        )
    };
    if result != 0 {
        note_successful_rewrite(&rewrite.relying_party);
        bridge_info(&format!(
            "官方流式 XSTS HTTP 请求已绑定自定义 UToken | rp={} | changed={} | stage=WinHttpWriteData | signature=official | token_body=redacted",
            rewrite.relying_party, rewrite.changed,
        ));
    }
    result
}

fn rewrite_xsts_user_token(source: &[u8]) -> Option<BodyRewrite> {
    let runtime = session()?;
    let custom = runtime.custom_user_token()?;
    let marker = find_bytes(source, b"\"UserTokens\"")?;
    if find_bytes(source, b"\"DeviceToken\"").is_none()
        || find_bytes(source, b"\"TitleToken\"").is_none()
    {
        return None;
    }
    let relying_party = extract_json_string_after_key(source, b"\"RelyingParty\"")?;

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
    let native_fingerprint: [u8; 32] = Sha256::digest(native).into();
    let changed = native != custom.as_bytes();
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
        relying_party,
        native_token_len: native.len(),
        custom_token_len: custom.len(),
        changed,
    })
}

fn extract_json_string_after_key(source: &[u8], key: &[u8]) -> Option<String> {
    let key_start = find_bytes(source, key)?;
    let after_key = key_start.checked_add(key.len())?;
    let colon = source[after_key..]
        .iter()
        .position(|byte| *byte == b':')?
        .checked_add(after_key)?;
    let quote = source[colon + 1..]
        .iter()
        .position(|byte| *byte == b'"')?
        .checked_add(colon + 1)?;
    let value_start = quote.checked_add(1)?;
    let mut index = value_start;
    while index < source.len() {
        match source[index] {
            b'"' => return std::str::from_utf8(&source[value_start..index]).ok().map(str::to_string),
            b'\\' => return None,
            _ => index += 1,
        }
    }
    None
}

fn consume_rewrite_ticket(fingerprint: [u8; 32], relying_party: &str) -> bool {
    let mut tickets = REWRITE_TICKETS
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    purge_expired_tickets(&mut tickets);
    let Some(position) = tickets.iter().position(|ticket| {
        ticket.fingerprint == fingerprint && ticket.relying_party == relying_party
    }) else {
        return false;
    };
    tickets.remove(position);
    true
}

fn purge_expired_tickets(tickets: &mut VecDeque<RewriteTicket>) {
    let now = Instant::now();
    tickets.retain(|ticket| now.duration_since(ticket.issued_at) <= REWRITE_TICKET_TTL);
}

fn note_successful_rewrite(relying_party: &str) {
    let mut generations = RP_REWRITE_GENERATIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generation = generations.entry(relying_party.to_string()).or_insert(0);
    *generation = generation.saturating_add(1);
}

fn rewrite_generation(relying_party: &str) -> u64 {
    RP_REWRITE_GENERATIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(relying_party)
        .copied()
        .unwrap_or(0)
}

fn begin_custom_token_request(
    async_block: *mut XAsyncBlock,
    relying_party: String,
    caller_options: u32,
) -> Result<u32, HResult> {
    if async_block.is_null() {
        return Err(E_INVALIDARG);
    }
    if session().and_then(|runtime| runtime.custom_user_token()).is_none() {
        bridge_warn(
            "拒绝官方 Token 请求 | reason=custom-utoken-expired-or-unavailable | action=fail-closed",
        );
        return Err(E_FAIL);
    }

    let start_generation = rewrite_generation(&relying_party);
    let caller_forced = caller_options & XUSER_TOKEN_FORCE_REFRESH != 0;
    let require_rewrite = caller_forced || start_generation == 0;
    let effective_options = if start_generation == 0 {
        caller_options | XUSER_TOKEN_FORCE_REFRESH
    } else {
        caller_options
    };

    PENDING_TOKEN_REQUESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            async_block as usize,
            PendingTokenRequest {
                relying_party: relying_party.clone(),
                start_generation,
                require_rewrite,
            },
        );

    if start_generation == 0 {
        bridge_debug(&format!(
            "首次请求该 RelyingParty；强制官方 Runtime 刷新 XSTS 以建立自定义用户缓存 | rp={relying_party} | options=0x{effective_options:02X}"
        ));
    }
    Ok(effective_options)
}

fn cancel_pending_request(async_block: *mut XAsyncBlock) {
    if async_block.is_null() {
        return;
    }
    PENDING_TOKEN_REQUESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(async_block as usize));
}

fn verify_completed_request(async_block: *mut XAsyncBlock, native_status: HResult) -> HResult {
    if async_block.is_null() {
        return native_status;
    }
    let pending = PENDING_TOKEN_REQUESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(async_block as usize));
    let Some(pending) = pending else {
        return native_status;
    };
    if native_status < 0 || !pending.require_rewrite {
        return native_status;
    }

    let current_generation = rewrite_generation(&pending.relying_party);
    if current_generation <= pending.start_generation {
        bridge_warn(&format!(
            "拒绝原生 XSTS 结果 | rp={} | reason=custom-utoken-rewrite-not-observed | native_result=0x{:08X} | action=fail-closed",
            pending.relying_party,
            native_status as u32,
        ));
        return E_FAIL;
    }
    native_status
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
    let url_text = hresult_try!(unsafe { ansi_url(url) });
    let relying_party = relying_party_for_url(&url_text);
    let effective_options = hresult_try!(begin_custom_token_request(
        async_block,
        relying_party,
        options,
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
    let status = unsafe {
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
    if status < 0 {
        cancel_pending_request(async_block);
    }
    status
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
    let native_status = unsafe { function(interface, async_block, size, buffer, data, used) };
    verify_completed_request(async_block, native_status)
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
    let url_text = hresult_try!(unsafe { utf16_url(url) });
    let relying_party = relying_party_for_url(&url_text);
    let effective_options = hresult_try!(begin_custom_token_request(
        async_block,
        relying_party,
        options,
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
    let status = unsafe {
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
    if status < 0 {
        cancel_pending_request(async_block);
    }
    status
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
    let native_status = unsafe { function(interface, async_block, size, buffer, data, used) };
    verify_completed_request(async_block, native_status)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_xsts_user_token_marker() {
        assert!(find_bytes(b"{\"UserTokens\":[]}", b"\"UserTokens\"").is_some());
        assert!(find_bytes(b"{}", b"\"UserTokens\"").is_none());
    }

    #[test]
    fn extracts_relying_party_without_json_allocation() {
        let body = br#"{"RelyingParty":"http://xboxlive.com","Properties":{}}"#;
        assert_eq!(
            extract_json_string_after_key(body, b"\"RelyingParty\"").as_deref(),
            Some(XBOX_LIVE_RP),
        );
    }

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
    }
}

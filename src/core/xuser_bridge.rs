// SPDX-License-Identifier: GPL-3.0-or-later
//
// XUser ABI layout and XAsync provider ordering are adapted from WineGDK and
// the former standalone Chlna6666/xgameruntime implementation. The bridge is
// activated only after an authenticated, process-scoped BMCBL pipe session is
// received. Without that session this module does not install any hook.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use core::ffi::{c_char, c_void};
use minhook::MinHook;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    ffi::CStr,
    mem,
    ptr,
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::runtime::foundation::logging;

const PIPE_MAGIC: &[u8; 8] = b"BMCBLXU1";
const PIPE_VERSION: u32 = 1;
const PIPE_HEADER_SIZE: usize = 80;
const MAX_PAYLOAD_SIZE: usize = 256 * 1024;
const MIN_TOKEN_REMAINING_SECONDS: u64 = 30;
const WINDOWS_TO_UNIX_EPOCH_SECONDS: u64 = 11_644_473_600;
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;
const SIGNATURE_POLICY_VERSION: u32 = 1;
const SIGNATURE_SIZE: usize = 64;
const SIGNATURE_HEADER_SIZE: usize = 4 + 8 + SIGNATURE_SIZE;

const GENERIC_READ: u32 = 0x8000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
const BCRYPT_ECDSA_PRIVATE_P256_MAGIC: u32 = 0x3253_4345;

const S_OK: i32 = 0;
const E_FAIL: i32 = 0x8000_4005_u32 as i32;
const E_POINTER: i32 = 0x8000_4003_u32 as i32;
const E_NOINTERFACE: i32 = 0x8000_4002_u32 as i32;
const E_NOTIMPL: i32 = 0x8000_4001_u32 as i32;
const E_INVALIDARG: i32 = 0x8007_0057_u32 as i32;
const E_NOT_SUFFICIENT_BUFFER: i32 = 0x8007_007a_u32 as i32;

const XUSER_STATE_SIGNED_IN: u32 = 0;
const XUSER_AGE_GROUP_UNKNOWN: u32 = 0;
const XUSER_AGE_GROUP_CHILD: u32 = 1;
const XUSER_AGE_GROUP_TEEN: u32 = 2;
const XUSER_AGE_GROUP_ADULT: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

impl Guid {
    const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self {
            data1,
            data2,
            data3,
            data4,
        }
    }
}

const IID_IUNKNOWN: Guid = Guid::new(
    0x0000_0000,
    0x0000,
    0x0000,
    [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
);
const CLSID_XUSER_IMPL: Guid = Guid::new(
    0x01ac_d177,
    0x91f9,
    0x4763,
    [0xa3, 0x8e, 0xcc, 0xbb, 0x55, 0xce, 0x32, 0xe0],
);
const IID_IXUSER_BASE: Guid = CLSID_XUSER_IMPL;
const IID_IXUSER_ADD_WITH_UI: Guid = Guid::new(
    0xeb9b_f948,
    0x18dc,
    0x4d82,
    [0xbb, 0xcc, 0x40, 0xe0, 0xa8, 0x09, 0xc4, 0xc0],
);
const IID_IXUSER_MSA: Guid = Guid::new(
    0x1bf2_f8c5,
    0xd507,
    0x4e52,
    [0xbb, 0x05, 0xf7, 0x26, 0xd0, 0xe7, 0x11, 0x61],
);
const IID_IXUSER_STORE: Guid = Guid::new(
    0x0794_15e3,
    0x6727,
    0x437f,
    [0x8e, 0x9d, 0x8f, 0x8f, 0x9b, 0x24, 0x39, 0xf7],
);
const IID_IXUSER_PLATFORM: Guid = Guid::new(
    0x26f3_c674,
    0xa2fe,
    0x44fa,
    [0xb6, 0xc4, 0xa3, 0x23, 0xbc, 0x94, 0xff, 0x53],
);
const IID_IXUSER_SIGN_OUT: Guid = Guid::new(
    0x5131_d685,
    0x4394,
    0x4ee6,
    [0x8c, 0x18, 0xbf, 0xb5, 0xd4, 0xae, 0xf1, 0xff],
);
const IID_IXUSER_GAMERTAG: Guid = Guid::new(
    0xcef4_fac0,
    0x7676,
    0x4a94,
    [0xa1, 0x19, 0x4c, 0x43, 0xf9, 0xeb, 0x5b, 0x74],
);
const CLSID_XTHREADING_IMPL: Guid = Guid::new(
    0x073b_7dcb,
    0x1fcf,
    0x4030,
    [0x94, 0xbe, 0xe3, 0xc9, 0xeb, 0x62, 0x34, 0x28],
);
const IID_IXTHREADING_IMPL: Guid = CLSID_XTHREADING_IMPL;

#[repr(C)]
struct ProcessEntry32W {
    size: u32,
    usage: u32,
    process_id: u32,
    default_heap_id: usize,
    module_id: u32,
    threads: u32,
    parent_process_id: u32,
    priority_class_base: i32,
    flags: u32,
    exe_file: [u16; 260],
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn CreateFileW(
        name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut c_void,
    ) -> *mut c_void;
    fn ReadFile(
        file: *mut c_void,
        buffer: *mut c_void,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetNamedPipeServerProcessId(pipe: *mut c_void, server_process_id: *mut u32) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
    fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn LoadLibraryExW(file_name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptOpenAlgorithmProvider(
        algorithm: *mut *mut c_void,
        algorithm_id: *const u16,
        implementation: *const u16,
        flags: u32,
    ) -> i32;
    fn BCryptImportKeyPair(
        algorithm: *mut c_void,
        import_key: *mut c_void,
        blob_type: *const u16,
        key: *mut *mut c_void,
        input: *mut u8,
        input_size: u32,
        flags: u32,
    ) -> i32;
    fn BCryptSignHash(
        key: *mut c_void,
        padding_info: *mut c_void,
        input: *mut u8,
        input_size: u32,
        output: *mut u8,
        output_size: u32,
        result_size: *mut u32,
        flags: u32,
    ) -> i32;
    fn BCryptDestroyKey(key: *mut c_void) -> i32;
    fn BCryptCloseAlgorithmProvider(algorithm: *mut c_void, flags: u32) -> i32;
}

struct OwnedHandle(*mut c_void);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 as isize != -1 {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct TokenRecord {
    token: String,
    user_hash: String,
    relying_party: String,
    expires_at: u64,
}

struct SigningKey {
    algorithm: *mut c_void,
    key: *mut c_void,
}

unsafe impl Send for SigningKey {}
unsafe impl Sync for SigningKey {}

impl Drop for SigningKey {
    fn drop(&mut self) {
        unsafe {
            if !self.key.is_null() {
                BCryptDestroyKey(self.key);
            }
            if !self.algorithm.is_null() {
                BCryptCloseAlgorithmProvider(self.algorithm, 0);
            }
        }
    }
}

impl SigningKey {
    fn import(mut blob: Vec<u8>) -> Result<Self, String> {
        if blob.len() != 104
            || u32::from_le_bytes(blob[0..4].try_into().unwrap())
                != BCRYPT_ECDSA_PRIVATE_P256_MAGIC
            || u32::from_le_bytes(blob[4..8].try_into().unwrap()) != 32
        {
            blob.zeroize();
            return Err("invalid P-256 BCRYPT private key blob".to_string());
        }

        let algorithm_name = wide("ECDSA_P256");
        let blob_type = wide("ECCPRIVATEBLOB");
        let mut algorithm = ptr::null_mut();
        let status = unsafe {
            BCryptOpenAlgorithmProvider(
                &mut algorithm,
                algorithm_name.as_ptr(),
                ptr::null(),
                0,
            )
        };
        if status != 0 {
            blob.zeroize();
            return Err(format!("BCryptOpenAlgorithmProvider failed: 0x{status:08X}"));
        }

        let mut key = ptr::null_mut();
        let status = unsafe {
            BCryptImportKeyPair(
                algorithm,
                ptr::null_mut(),
                blob_type.as_ptr(),
                &mut key,
                blob.as_mut_ptr(),
                blob.len() as u32,
                0,
            )
        };
        blob.zeroize();
        if status != 0 {
            unsafe {
                BCryptCloseAlgorithmProvider(algorithm, 0);
            }
            return Err(format!("BCryptImportKeyPair failed: 0x{status:08X}"));
        }

        Ok(Self { algorithm, key })
    }

    fn sign_hash(&self, digest: &mut [u8; 32]) -> Result<[u8; SIGNATURE_SIZE], i32> {
        let mut signature = [0u8; SIGNATURE_SIZE];
        let mut written = 0u32;
        let status = unsafe {
            BCryptSignHash(
                self.key,
                ptr::null_mut(),
                digest.as_mut_ptr(),
                digest.len() as u32,
                signature.as_mut_ptr(),
                signature.len() as u32,
                &mut written,
                0,
            )
        };
        if status != 0 || written as usize != signature.len() {
            signature.zeroize();
            return Err(if status != 0 { status } else { E_FAIL });
        }
        Ok(signature)
    }
}

struct Session {
    xuid: u64,
    local_id: XUserLocalId,
    gamertag: String,
    age_group: u32,
    privileges: Vec<u32>,
    tokens: Vec<TokenRecord>,
    signing_key: SigningKey,
}

unsafe impl Send for Session {}
unsafe impl Sync for Session {}

static SESSION: OnceLock<Session> = OnceLock::new();
static ORIGINAL_QUERY_API: OnceLock<usize> = OnceLock::new();

pub fn initialize_before_mods() {
    if SESSION.get().is_some() {
        return;
    }

    let mut payload = match receive_session_payload() {
        Ok(Some(payload)) => payload,
        Ok(None) => {
            logging::scoped_info_message(
                "xuser-bridge",
                "BMCBL session absent; QueryApiImpl hook not installed",
            );
            return;
        }
        Err(error) => {
            logging::scoped_warn_message(
                "xuser-bridge",
                &format!("session rejected; official XUser remains active | reason={error}"),
            );
            return;
        }
    };

    let session = match parse_session(&payload) {
        Ok(session) => session,
        Err(error) => {
            payload.zeroize();
            logging::scoped_warn_message(
                "xuser-bridge",
                &format!("pre-authentication rejected; official XUser remains active | reason={error}"),
            );
            return;
        }
    };
    payload.zeroize();

    if SESSION.set(session).is_err() {
        logging::scoped_warn_message(
            "xuser-bridge",
            "session was already initialized; refusing replacement",
        );
        return;
    }

    match install_query_api_hook() {
        Ok(()) => logging::scoped_info_message(
            "xuser-bridge",
            "authenticated Win32 session accepted; only official QueryApiImpl is hooked",
        ),
        Err(error) => logging::scoped_error_message(
            "xuser-bridge",
            &format!("QueryApiImpl hook failed; custom session disabled | reason={error}"),
        ),
    }
}

fn receive_session_payload() -> Result<Option<Vec<u8>>, String> {
    let current_pid = unsafe { GetCurrentProcessId() };
    let pipe_name = format!(r"\\.\pipe\BMCBL.XUser.{current_pid}");
    let pipe_name = wide(&pipe_name);
    let handle = unsafe {
        CreateFileW(
            pipe_name.as_ptr(),
            GENERIC_READ,
            0,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle.is_null() || handle as isize == -1 {
        return Ok(None);
    }
    let pipe = OwnedHandle(handle);

    let mut server_pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut server_pid) } == 0 {
        return Err("unable to identify pipe server process".to_string());
    }
    let parent_pid = parent_process_id(current_pid)
        .ok_or_else(|| "unable to identify launcher parent process".to_string())?;
    if server_pid != parent_pid {
        return Err("pipe server is not the Minecraft parent process".to_string());
    }

    let mut header = [0u8; PIPE_HEADER_SIZE];
    read_exact(pipe.0, &mut header)?;
    if &header[0..8] != PIPE_MAGIC {
        return Err("invalid pipe magic".to_string());
    }
    let version = read_u32(&header, 8);
    let target_pid = read_u32(&header, 12);
    let launcher_pid = read_u32(&header, 16);
    let issued_at = read_u64(&header, 24);
    let expires_at = read_u64(&header, 32);
    let payload_len = read_u32(&header, 40) as usize;
    let expected_digest: [u8; 32] = header[48..80].try_into().unwrap();

    if version != PIPE_VERSION
        || target_pid != current_pid
        || launcher_pid != server_pid
        || payload_len == 0
        || payload_len > MAX_PAYLOAD_SIZE
    {
        return Err("invalid session header".to_string());
    }
    let now = now_epoch();
    if issued_at > now.saturating_add(30)
        || expires_at <= now
        || expires_at.saturating_sub(issued_at) > 120
    {
        return Err("session transport window expired".to_string());
    }

    let mut payload = vec![0u8; payload_len];
    read_exact(pipe.0, &mut payload)?;
    let digest: [u8; 32] = Sha256::digest(&payload).into();
    if digest != expected_digest {
        payload.zeroize();
        return Err("session payload digest mismatch".to_string());
    }
    Ok(Some(payload))
}

fn parent_process_id(current_pid: u32) -> Option<u32> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot as isize == -1 {
        return None;
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry: ProcessEntry32W = unsafe { mem::zeroed() };
    entry.size = mem::size_of::<ProcessEntry32W>() as u32;
    if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
        return None;
    }
    loop {
        if entry.process_id == current_pid {
            return Some(entry.parent_process_id);
        }
        if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
            return None;
        }
    }
}

fn read_exact(handle: *mut c_void, output: &mut [u8]) -> Result<(), String> {
    let mut offset = 0usize;
    while offset < output.len() {
        let mut read = 0u32;
        let remaining = (output.len() - offset).min(u32::MAX as usize) as u32;
        let ok = unsafe {
            ReadFile(
                handle,
                output[offset..].as_mut_ptr().cast(),
                remaining,
                &mut read,
                ptr::null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            return Err("pipe closed before the complete session was received".to_string());
        }
        offset += read as usize;
    }
    Ok(())
}

fn parse_session(payload: &[u8]) -> Result<Session, String> {
    let document: Value = serde_json::from_slice(payload)
        .map_err(|_| "pre-authentication payload is not valid JSON".to_string())?;
    let object = document
        .as_object()
        .ok_or_else(|| "pre-authentication payload is not an object".to_string())?;

    let private_blob = required_string(object, "ecc_private_blob_b64")?;
    let mut private_blob = STANDARD
        .decode(private_blob)
        .map_err(|_| "invalid private key encoding".to_string())?;
    let signing_key = SigningKey::import(mem::take(&mut private_blob))?;

    let xuid_text = required_string(object, "xbl_xuid")?;
    let xuid = xuid_text
        .parse::<u64>()
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| "invalid XUID".to_string())?;
    let user_hash = required_string(object, "xbl_uhs")?.to_string();
    let local_id = user_hash.parse::<u64>().unwrap_or(xuid);
    let gamertag = required_string(object, "xbl_gamertag")?.to_string();
    if gamertag.trim().is_empty() {
        return Err("empty gamertag".to_string());
    }

    let age_group = match optional_string(object, "xbl_age_group")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "child" => XUSER_AGE_GROUP_CHILD,
        "teen" | "teenager" => XUSER_AGE_GROUP_TEEN,
        "adult" => XUSER_AGE_GROUP_ADULT,
        _ => XUSER_AGE_GROUP_UNKNOWN,
    };
    let mut privileges = optional_string(object, "xbl_privileges")
        .unwrap_or_default()
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter_map(|value| value.parse::<u32>().ok())
        .collect::<Vec<_>>();
    privileges.sort_unstable();
    privileges.dedup();

    let mut tokens = Vec::new();
    push_token(
        object,
        &mut tokens,
        "xbl_token",
        "xbl_uhs",
        None,
        "xbl_token_expiry_epoch",
        "http://xboxlive.com",
    )?;
    push_token(
        object,
        &mut tokens,
        "sisu_token",
        "sisu_uhs",
        Some("sisu_rp"),
        "sisu_expiry_epoch",
        "https://b980a380.minecraft.playfabapi.com/",
    )?;
    push_token(
        object,
        &mut tokens,
        "mp_token",
        "mp_uhs",
        Some("mp_rp"),
        "mp_expiry_epoch",
        "https://multiplayer.minecraft.net/",
    )?;
    push_token(
        object,
        &mut tokens,
        "realms_token",
        "realms_uhs",
        Some("realms_rp"),
        "realms_expiry_epoch",
        "https://pocket.realms.minecraft.net/",
    )?;
    push_optional_token(
        object,
        &mut tokens,
        "lic_token",
        "lic_uhs",
        Some("lic_rp"),
        "lic_expiry_epoch",
        "http://licensing.xboxlive.com",
    )?;

    Ok(Session {
        xuid,
        local_id: XUserLocalId { value: local_id },
        gamertag,
        age_group,
        privileges,
        tokens,
        signing_key,
    })
}

fn push_token(
    object: &serde_json::Map<String, Value>,
    tokens: &mut Vec<TokenRecord>,
    token_key: &str,
    hash_key: &str,
    relying_party_key: Option<&str>,
    expiry_key: &str,
    default_relying_party: &str,
) -> Result<(), String> {
    let token = required_string(object, token_key)?.to_string();
    let user_hash = required_string(object, hash_key)?.to_string();
    let relying_party = relying_party_key
        .and_then(|key| optional_string(object, key))
        .unwrap_or(default_relying_party)
        .to_string();
    let expires_at = required_epoch(object, expiry_key)?;
    if token.is_empty()
        || user_hash.is_empty()
        || relying_party.is_empty()
        || expires_at <= now_epoch().saturating_add(MIN_TOKEN_REMAINING_SECONDS)
    {
        return Err(format!("invalid or expired token: {token_key}"));
    }
    tokens.push(TokenRecord {
        token,
        user_hash,
        relying_party,
        expires_at,
    });
    Ok(())
}

fn push_optional_token(
    object: &serde_json::Map<String, Value>,
    tokens: &mut Vec<TokenRecord>,
    token_key: &str,
    hash_key: &str,
    relying_party_key: Option<&str>,
    expiry_key: &str,
    default_relying_party: &str,
) -> Result<(), String> {
    if optional_string(object, token_key).is_none() {
        return Ok(());
    }
    push_token(
        object,
        tokens,
        token_key,
        hash_key,
        relying_party_key,
        expiry_key,
        default_relying_party,
    )
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    optional_string(object, key).ok_or_else(|| format!("missing field: {key}"))
}

fn optional_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str).filter(|value| !value.is_empty())
}

fn required_epoch(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<u64, String> {
    let value = object
        .get(key)
        .ok_or_else(|| format!("missing field: {key}"))?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .ok_or_else(|| format!("invalid epoch field: {key}"))
}

fn install_query_api_hook() -> Result<(), String> {
    let module_name = wide("xgameruntime.dll");
    let mut module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    if module.is_null() {
        module = unsafe {
            LoadLibraryExW(
                module_name.as_ptr(),
                ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
    }
    if module.is_null() {
        return Err("official System32 xgameruntime.dll is unavailable".to_string());
    }
    let target = unsafe { GetProcAddress(module, b"QueryApiImpl\0".as_ptr()) };
    if target.is_null() {
        return Err("official xgameruntime.dll does not export QueryApiImpl".to_string());
    }

    let trampoline = unsafe {
        MinHook::create_hook(
            target,
            query_api_hook as *const () as *mut c_void,
        )
    }
    .map_err(|status| format!("MinHook create failed: {status:?}"))?;
    ORIGINAL_QUERY_API
        .set(trampoline as usize)
        .map_err(|_| "original QueryApiImpl was already set".to_string())?;
    unsafe { MinHook::enable_all_hooks() }
        .map_err(|status| format!("MinHook enable failed: {status:?}"))?;
    Ok(())
}

type QueryApiImplFn =
    unsafe extern "system" fn(*const Guid, *const Guid, *mut *mut c_void) -> i32;

unsafe extern "system" fn query_api_hook(
    runtime_class_id: *const Guid,
    interface_id: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    if runtime_class_id.is_null() || interface_id.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe {
        out.write(ptr::null_mut());
    }
    if unsafe { *runtime_class_id } == CLSID_XUSER_IMPL {
        return unsafe { query_xuser_interface(interface_id, out) };
    }
    unsafe { call_original_query(runtime_class_id, interface_id, out) }
}

unsafe fn call_original_query(
    runtime_class_id: *const Guid,
    interface_id: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    let Some(address) = ORIGINAL_QUERY_API.get().copied() else {
        return E_FAIL;
    };
    let function: QueryApiImplFn = unsafe { mem::transmute(address) };
    unsafe { function(runtime_class_id, interface_id, out) }
}

#[repr(u32)]
#[derive(Clone, Copy)]
enum XAsyncOp {
    Begin = 0,
    DoWork = 1,
    GetResult = 2,
    Cancel = 3,
    Cleanup = 4,
}

#[repr(C)]
struct XAsyncBlock {
    queue: *mut c_void,
    context: *mut c_void,
    callback: Option<unsafe extern "system" fn(*mut XAsyncBlock)>,
    internal: [usize; 4],
}

#[repr(C)]
struct XAsyncProviderData {
    async_block: *mut XAsyncBlock,
    buffer_size: usize,
    buffer: *mut c_void,
    context: *mut c_void,
}

type XAsyncProvider = unsafe extern "system" fn(XAsyncOp, *const XAsyncProviderData) -> i32;

#[repr(C)]
struct XThreadingInterface {
    vtable: *const XThreadingVtable,
}

#[repr(C)]
struct XThreadingVtable {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut XThreadingInterface) -> u32,
    async_get_status: usize,
    async_get_result_size:
        unsafe extern "system" fn(*mut XThreadingInterface, *mut XAsyncBlock, *mut usize) -> i32,
    async_cancel: usize,
    async_run: usize,
    async_begin: unsafe extern "system" fn(
        *mut XThreadingInterface,
        *mut XAsyncBlock,
        *mut c_void,
        *const c_void,
        *const c_char,
        XAsyncProvider,
    ) -> i32,
    padding: usize,
    async_schedule:
        unsafe extern "system" fn(*mut XThreadingInterface, *mut XAsyncBlock, u32) -> i32,
    async_complete:
        unsafe extern "system" fn(*mut XThreadingInterface, *mut XAsyncBlock, i32, usize),
    async_get_result: unsafe extern "system" fn(
        *mut XThreadingInterface,
        *mut XAsyncBlock,
        *const c_void,
        usize,
        *mut c_void,
        *mut usize,
    ) -> i32,
}

struct ThreadingHandle(*mut XThreadingInterface);

impl ThreadingHandle {
    fn acquire() -> Result<Self, i32> {
        let mut interface = ptr::null_mut();
        let status = unsafe {
            call_original_query(
                &CLSID_XTHREADING_IMPL,
                &IID_IXTHREADING_IMPL,
                &mut interface,
            )
        };
        if status < 0 {
            return Err(status);
        }
        if interface.is_null() {
            return Err(E_POINTER);
        }
        Ok(Self(interface.cast()))
    }

    fn vtable(&self) -> &XThreadingVtable {
        unsafe { &*(*self.0).vtable }
    }
}

impl Drop for ThreadingHandle {
    fn drop(&mut self) {
        unsafe {
            (self.vtable().release)(self.0);
        }
    }
}

unsafe fn xasync_begin(
    async_block: *mut XAsyncBlock,
    context: *mut c_void,
    identity: *const c_void,
    identity_name: *const c_char,
    provider: XAsyncProvider,
) -> i32 {
    if async_block.is_null() || identity.is_null() || identity_name.is_null() {
        return E_POINTER;
    }
    let Ok(threading) = ThreadingHandle::acquire() else {
        return E_FAIL;
    };
    unsafe {
        (threading.vtable().async_begin)(
            threading.0,
            async_block,
            context,
            identity,
            identity_name,
            provider,
        )
    }
}

unsafe fn xasync_schedule(async_block: *mut XAsyncBlock, delay_ms: u32) -> i32 {
    let Ok(threading) = ThreadingHandle::acquire() else {
        return E_FAIL;
    };
    unsafe { (threading.vtable().async_schedule)(threading.0, async_block, delay_ms) }
}

unsafe fn xasync_complete(async_block: *mut XAsyncBlock, result: i32, required_size: usize) {
    if let Ok(threading) = ThreadingHandle::acquire() {
        unsafe {
            (threading.vtable().async_complete)(threading.0, async_block, result, required_size);
        }
    }
}

unsafe fn xasync_get_result_size(async_block: *mut XAsyncBlock, size: *mut usize) -> i32 {
    if async_block.is_null() || size.is_null() {
        return E_POINTER;
    }
    let Ok(threading) = ThreadingHandle::acquire() else {
        return E_FAIL;
    };
    unsafe { (threading.vtable().async_get_result_size)(threading.0, async_block, size) }
}

unsafe fn xasync_get_result(
    async_block: *mut XAsyncBlock,
    identity: *const c_void,
    buffer_size: usize,
    buffer: *mut c_void,
    used: *mut usize,
) -> i32 {
    if async_block.is_null() || identity.is_null() || (buffer_size != 0 && buffer.is_null()) {
        return E_POINTER;
    }
    let Ok(threading) = ThreadingHandle::acquire() else {
        return E_FAIL;
    };
    unsafe {
        (threading.vtable().async_get_result)(
            threading.0,
            async_block,
            identity,
            buffer_size,
            buffer,
            used,
        )
    }
}

#[repr(C)]
#[derive(Clone, Copy, Eq, PartialEq)]
struct XUserLocalId {
    value: u64,
}

type XUserHandle = *mut c_void;

#[repr(C)]
struct XUserVtable {
    slots: [usize; 50],
}

#[repr(C)]
struct XUserGamertagVtable {
    slots: [usize; 4],
}

#[repr(C)]
struct XUserGamertagInterface {
    vtable: *const XUserGamertagVtable,
}

#[repr(C)]
struct XUserObject {
    vtable: *const XUserVtable,
    gamertag: XUserGamertagInterface,
}

unsafe impl Send for XUserObject {}
unsafe impl Sync for XUserObject {}

static USER_VTABLE: OnceLock<XUserVtable> = OnceLock::new();
static GAMERTAG_VTABLE: OnceLock<XUserGamertagVtable> = OnceLock::new();
static USER_OBJECT: OnceLock<XUserObject> = OnceLock::new();
static XUSER_ADD_IDENTITY: u8 = 0;
const XUSER_ADD_NAME: &[u8] = b"XUserAddAsync\0";

fn user_object() -> Option<&'static XUserObject> {
    SESSION.get()?;
    Some(USER_OBJECT.get_or_init(|| XUserObject {
        vtable: user_vtable(),
        gamertag: XUserGamertagInterface {
            vtable: gamertag_vtable(),
        },
    }))
}

fn provider_interface() -> Option<*mut c_void> {
    Some(user_object()? as *const XUserObject as *mut c_void)
}

fn gamertag_interface() -> Option<*mut c_void> {
    let object = user_object()?;
    Some(&object.gamertag as *const XUserGamertagInterface as *mut c_void)
}

unsafe fn query_xuser_interface(iid: *const Guid, out: *mut *mut c_void) -> i32 {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe {
        out.write(ptr::null_mut());
    }
    let iid = unsafe { &*iid };
    if [
        IID_IUNKNOWN,
        IID_IXUSER_BASE,
        IID_IXUSER_ADD_WITH_UI,
        IID_IXUSER_MSA,
        IID_IXUSER_STORE,
        IID_IXUSER_PLATFORM,
        IID_IXUSER_SIGN_OUT,
    ]
    .contains(iid)
    {
        if let Some(interface) = provider_interface() {
            unsafe {
                out.write(interface);
            }
            return S_OK;
        }
    }
    if *iid == IID_IXUSER_GAMERTAG {
        if let Some(interface) = gamertag_interface() {
            unsafe {
                out.write(interface);
            }
            return S_OK;
        }
    }
    E_NOINTERFACE
}

unsafe extern "system" fn user_query_interface(
    _interface: *mut c_void,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    unsafe { query_xuser_interface(iid, out) }
}

unsafe extern "system" fn add_ref(_interface: *mut c_void) -> u32 {
    2
}
unsafe extern "system" fn release(_interface: *mut c_void) -> u32 {
    1
}
unsafe extern "system" fn duplicate_handle(
    _interface: *mut c_void,
    user: XUserHandle,
    duplicated: *mut XUserHandle,
) -> i32 {
    if duplicated.is_null() {
        return E_POINTER;
    }
    let Some(provider) = provider_interface() else {
        return E_FAIL;
    };
    if !user.is_null() && user != provider {
        return E_INVALIDARG;
    }
    unsafe {
        duplicated.write(provider);
    }
    S_OK
}
unsafe extern "system" fn close_user_handle(_interface: *mut c_void, _user: XUserHandle) {}
unsafe extern "system" fn compare_users(
    _interface: *mut c_void,
    user1: XUserHandle,
    user2: XUserHandle,
) -> i32 {
    i32::from(user1 != user2)
}
unsafe extern "system" fn get_max_users(_interface: *mut c_void, output: *mut u32) -> i32 {
    if output.is_null() {
        return E_POINTER;
    }
    unsafe {
        output.write(1);
    }
    S_OK
}

struct XUserAddContext {
    handle: usize,
}

unsafe extern "system" fn xuser_add_provider(
    operation: XAsyncOp,
    data: *const XAsyncProviderData,
) -> i32 {
    if data.is_null() {
        return E_POINTER;
    }
    let data = unsafe { &*data };
    let context = data.context.cast::<XUserAddContext>();
    if context.is_null() {
        return E_POINTER;
    }
    match operation {
        XAsyncOp::Begin => unsafe { xasync_schedule(data.async_block, 0) },
        XAsyncOp::DoWork => {
            unsafe {
                xasync_complete(
                    data.async_block,
                    S_OK,
                    mem::size_of::<XUserHandle>(),
                );
            }
            S_OK
        }
        XAsyncOp::GetResult => {
            if data.buffer.is_null() || data.buffer_size < mem::size_of::<XUserHandle>() {
                return E_NOT_SUFFICIENT_BUFFER;
            }
            unsafe {
                data.buffer
                    .cast::<XUserHandle>()
                    .write((*context).handle as XUserHandle);
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

unsafe extern "system" fn add_async(
    _interface: *mut c_void,
    _options: u32,
    async_block: *mut XAsyncBlock,
) -> i32 {
    let Some(handle) = provider_interface() else {
        return E_FAIL;
    };
    let context = Box::into_raw(Box::new(XUserAddContext {
        handle: handle as usize,
    }));
    let result = unsafe {
        xasync_begin(
            async_block,
            context.cast(),
            (&XUSER_ADD_IDENTITY as *const u8).cast(),
            XUSER_ADD_NAME.as_ptr().cast(),
            xuser_add_provider,
        )
    };
    if result < 0 {
        unsafe {
            drop(Box::from_raw(context));
        }
    }
    result
}

unsafe extern "system" fn add_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    user: *mut XUserHandle,
) -> i32 {
    if user.is_null() {
        return E_POINTER;
    }
    unsafe {
        xasync_get_result(
            async_block,
            (&XUSER_ADD_IDENTITY as *const u8).cast(),
            mem::size_of::<XUserHandle>(),
            user.cast(),
            ptr::null_mut(),
        )
    }
}

fn valid_user(user: XUserHandle) -> bool {
    provider_interface().is_some_and(|provider| user == provider)
}

unsafe extern "system" fn get_local_id(
    _interface: *mut c_void,
    user: XUserHandle,
    output: *mut XUserLocalId,
) -> i32 {
    if output.is_null() || !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        output.write(SESSION.get().unwrap().local_id);
    }
    S_OK
}
unsafe extern "system" fn find_by_local_id(
    _interface: *mut c_void,
    local_id: XUserLocalId,
    output: *mut XUserHandle,
) -> i32 {
    if output.is_null() {
        return E_POINTER;
    }
    let session = SESSION.get().unwrap();
    if local_id != session.local_id {
        return E_FAIL;
    }
    unsafe {
        output.write(provider_interface().unwrap());
    }
    S_OK
}
unsafe extern "system" fn get_id(
    _interface: *mut c_void,
    user: XUserHandle,
    output: *mut u64,
) -> i32 {
    if output.is_null() || !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        output.write(SESSION.get().unwrap().xuid);
    }
    S_OK
}
unsafe extern "system" fn find_by_id(
    _interface: *mut c_void,
    id: u64,
    output: *mut XUserHandle,
) -> i32 {
    if output.is_null() {
        return E_POINTER;
    }
    if id != SESSION.get().unwrap().xuid {
        return E_FAIL;
    }
    unsafe {
        output.write(provider_interface().unwrap());
    }
    S_OK
}
unsafe extern "system" fn get_is_guest(
    _interface: *mut c_void,
    user: XUserHandle,
    output: *mut u8,
) -> i32 {
    if output.is_null() || !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        output.write(0);
    }
    S_OK
}
unsafe extern "system" fn get_state(
    _interface: *mut c_void,
    user: XUserHandle,
    output: *mut u32,
) -> i32 {
    if output.is_null() || !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        output.write(XUSER_STATE_SIGNED_IN);
    }
    S_OK
}
unsafe extern "system" fn get_age_group(
    _interface: *mut c_void,
    user: XUserHandle,
    output: *mut u32,
) -> i32 {
    if output.is_null() || !valid_user(user) {
        return E_INVALIDARG;
    }
    unsafe {
        output.write(SESSION.get().unwrap().age_group);
    }
    S_OK
}
unsafe extern "system" fn check_privilege(
    _interface: *mut c_void,
    user: XUserHandle,
    _options: u32,
    privilege: i32,
    allowed: *mut u8,
    reason: *mut u32,
) -> i32 {
    if allowed.is_null() || reason.is_null() || !valid_user(user) {
        return E_INVALIDARG;
    }
    let allowed_value = privilege >= 0
        && SESSION
            .get()
            .unwrap()
            .privileges
            .contains(&(privilege as u32));
    unsafe {
        allowed.write(u8::from(allowed_value));
        reason.write(0);
    }
    S_OK
}

unsafe extern "system" fn get_gamertag(
    _interface: *mut c_void,
    user: XUserHandle,
    component: u32,
    size: usize,
    output: *mut c_char,
    used: *mut usize,
) -> i32 {
    if output.is_null() || !valid_user(user) {
        return E_INVALIDARG;
    }
    let value = match component {
        0 | 1 | 3 => SESSION.get().unwrap().gamertag.as_str(),
        2 => "",
        _ => return E_INVALIDARG,
    };
    let required = value.len() + 1;
    if !used.is_null() {
        unsafe {
            used.write(required);
        }
    }
    if size < required {
        return E_NOT_SUFFICIENT_BUFFER;
    }
    unsafe {
        ptr::copy_nonoverlapping(value.as_ptr(), output.cast(), value.len());
        output.add(value.len()).write(0);
    }
    S_OK
}

unsafe extern "system" fn stub_hresult(_interface: *mut c_void) -> i32 {
    E_NOTIMPL
}
unsafe extern "system" fn stub_boolean(_interface: *mut c_void) -> u8 {
    0
}
unsafe extern "system" fn stub_void(_interface: *mut c_void) {}

#[repr(C)]
struct TokenData {
    token_size: usize,
    signature_size: usize,
    token: *const c_char,
    signature: *const c_char,
}
#[repr(C)]
struct TokenHeader {
    name: *const c_char,
    value: *const c_char,
}
#[repr(C)]
struct TokenUtf16Data {
    token_count: usize,
    signature_count: usize,
    token: *const u16,
    signature: *const u16,
}
#[repr(C)]
struct TokenUtf16Header {
    name: *const u16,
    value: *const u16,
}

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

impl Drop for TokenContext {
    fn drop(&mut self) {
        self.body.zeroize();
        self.authorization.zeroize();
        self.authorization_utf16.zeroize();
        self.signature.zeroize();
        self.signature_utf16.zeroize();
    }
}

impl TokenContext {
    fn new(user: XUserHandle, method: &str, url: &str, body: &[u8], utf16: bool) -> Result<Self, i32> {
        if !valid_user(user) {
            return Err(E_INVALIDARG);
        }
        let token = select_token(url).ok_or(E_FAIL)?;
        if token.expires_at <= now_epoch().saturating_add(MIN_TOKEN_REMAINING_SECONDS) {
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

    fn prepare(&mut self) -> Result<(), i32> {
        if self.prepared {
            return Ok(());
        }
        let authorization = self
            .authorization
            .strip_suffix(&[0])
            .and_then(|value| std::str::from_utf8(value).ok())
            .ok_or(E_INVALIDARG)?;
        let signature = sign_request(
            &self.method,
            &self.request_target,
            authorization,
            &self.body,
        )?;
        self.body.zeroize();
        self.body.clear();
        self.signature = signature.as_bytes().to_vec();
        self.signature.push(0);
        self.signature_utf16 = signature.encode_utf16().collect();
        self.signature_utf16.push(0);
        self.prepared = true;
        Ok(())
    }

    fn required_size(&self) -> Option<usize> {
        if !self.prepared {
            return None;
        }
        if self.utf16 {
            mem::size_of::<TokenUtf16Data>()
                .checked_add((self.authorization_utf16.len() + self.signature_utf16.len()) * 2)
        } else {
            mem::size_of::<TokenData>()
                .checked_add(self.authorization.len() + self.signature.len())
        }
    }
}

static TOKEN_IDENTITY_ANSI: u8 = 0x41;
static TOKEN_IDENTITY_UTF16: u8 = 0x57;
const TOKEN_NAME_ANSI: &[u8] = b"XUserGetTokenAndSignatureAsync\0";
const TOKEN_NAME_UTF16: &[u8] = b"XUserGetTokenAndSignatureUtf16Async\0";

fn token_identity(utf16: bool) -> *const c_void {
    if utf16 {
        (&TOKEN_IDENTITY_UTF16 as *const u8).cast()
    } else {
        (&TOKEN_IDENTITY_ANSI as *const u8).cast()
    }
}

unsafe extern "system" fn token_provider(
    operation: XAsyncOp,
    data: *const XAsyncProviderData,
) -> i32 {
    if data.is_null() {
        return E_POINTER;
    }
    let data = unsafe { &*data };
    let context = data.context.cast::<TokenContext>();
    if context.is_null() {
        return E_POINTER;
    }
    match operation {
        XAsyncOp::Begin => unsafe { xasync_schedule(data.async_block, 0) },
        XAsyncOp::DoWork => {
            let context = unsafe { &mut *context };
            match context.prepare().and_then(|_| context.required_size().ok_or(E_FAIL)) {
                Ok(size) => unsafe { xasync_complete(data.async_block, S_OK, size) },
                Err(error) => unsafe { xasync_complete(data.async_block, error, 0) },
            }
            S_OK
        }
        XAsyncOp::GetResult => {
            let context = unsafe { &*context };
            let Some(required) = context.required_size() else {
                return E_FAIL;
            };
            if data.buffer.is_null() || data.buffer_size < required {
                return E_NOT_SUFFICIENT_BUFFER;
            }
            if context.utf16 {
                let output = data.buffer.cast::<TokenUtf16Data>();
                let token = unsafe { output.add(1).cast::<u16>() };
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
                    output.write(TokenUtf16Data {
                        token_count: context.authorization_utf16.len() * 2,
                        signature_count: context.signature_utf16.len() * 2,
                        token,
                        signature,
                    });
                }
            } else {
                let output = data.buffer.cast::<TokenData>();
                let token = unsafe { output.add(1).cast::<u8>() };
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
                    output.write(TokenData {
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
    header_count: usize,
    body_size: usize,
    body: *const c_void,
    async_block: *mut XAsyncBlock,
    utf16: bool,
) -> i32 {
    if user.is_null() || async_block.is_null() || (body_size != 0 && body.is_null()) {
        return E_POINTER;
    }
    if options & !0x03 != 0
        || method.is_empty()
        || method.len() > 32
        || !method.is_ascii()
        || url.is_empty()
        || url.len() > 32 * 1024
        || !url.is_ascii()
        || header_count > 128
        || body_size > 64 * 1024 * 1024
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
        xasync_begin(
            async_block,
            context.cast(),
            token_identity(utf16),
            if utf16 {
                TOKEN_NAME_UTF16.as_ptr().cast()
            } else {
                TOKEN_NAME_ANSI.as_ptr().cast()
            },
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

unsafe extern "system" fn token_async(
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
) -> i32 {
    if method.is_null() || url.is_null() || (header_count != 0 && headers.is_null()) {
        return E_POINTER;
    }
    for header in unsafe { std::slice::from_raw_parts(headers, header_count) } {
        if header.name.is_null() || header.value.is_null() {
            return E_POINTER;
        }
    }
    let Ok(method) = unsafe { CStr::from_ptr(method) }.to_str() else {
        return E_INVALIDARG;
    };
    let Ok(url) = unsafe { CStr::from_ptr(url) }.to_str() else {
        return E_INVALIDARG;
    };
    unsafe {
        begin_token_request(
            user,
            options,
            method,
            url,
            header_count,
            body_size,
            body,
            async_block,
            false,
        )
    }
}

unsafe extern "system" fn token_result_size(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: *mut usize,
) -> i32 {
    unsafe { xasync_get_result_size(async_block, size) }
}

unsafe extern "system" fn token_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: usize,
    buffer: *mut c_void,
    output: *mut *mut TokenData,
    used: *mut usize,
) -> i32 {
    if output.is_null() || buffer.is_null() {
        return E_POINTER;
    }
    let result = unsafe {
        xasync_get_result(async_block, token_identity(false), size, buffer, used)
    };
    if result >= 0 {
        unsafe {
            output.write(buffer.cast());
        }
    }
    result
}

unsafe fn utf16_string(value: *const u16) -> Result<String, i32> {
    if value.is_null() {
        return Err(E_POINTER);
    }
    let mut length = 0usize;
    while length < 32 * 1024 {
        if unsafe { value.add(length).read() } == 0 {
            return String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) })
                .map_err(|_| E_INVALIDARG);
        }
        length += 1;
    }
    Err(E_INVALIDARG)
}

unsafe extern "system" fn token_utf16_async(
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
) -> i32 {
    if header_count != 0 && headers.is_null() {
        return E_POINTER;
    }
    for header in unsafe { std::slice::from_raw_parts(headers, header_count) } {
        if header.name.is_null() || header.value.is_null() {
            return E_POINTER;
        }
    }
    let method = match unsafe { utf16_string(method) } {
        Ok(value) => value,
        Err(error) => return error,
    };
    let url = match unsafe { utf16_string(url) } {
        Ok(value) => value,
        Err(error) => return error,
    };
    unsafe {
        begin_token_request(
            user,
            options,
            &method,
            &url,
            header_count,
            body_size,
            body,
            async_block,
            true,
        )
    }
}

unsafe extern "system" fn token_utf16_result(
    _interface: *mut c_void,
    async_block: *mut XAsyncBlock,
    size: usize,
    buffer: *mut c_void,
    output: *mut *mut TokenUtf16Data,
    used: *mut usize,
) -> i32 {
    if output.is_null() || buffer.is_null() {
        return E_POINTER;
    }
    let result = unsafe {
        xasync_get_result(async_block, token_identity(true), size, buffer, used)
    };
    if result >= 0 {
        unsafe {
            output.write(buffer.cast());
        }
    }
    result
}

fn select_token(url: &str) -> Option<&'static TokenRecord> {
    let relying_party = relying_party_for_url(url);
    SESSION
        .get()?
        .tokens
        .iter()
        .find(|token| token.relying_party == relying_party)
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
        "http://licensing.xboxlive.com"
    } else if host == "playfabapi.com" || host.ends_with(".playfabapi.com") {
        "https://b980a380.minecraft.playfabapi.com/"
    } else if host == "multiplayer.minecraft.net" || host.ends_with(".multiplayer.minecraft.net") {
        "https://multiplayer.minecraft.net/"
    } else if matches!(
        host.as_str(),
        "pocket.realms.minecraft.net"
            | "bedrock.frontend.realms.minecraft-services.net"
            | "bedrock.frontendlegacy.realms.minecraft-services.net"
    ) {
        "https://pocket.realms.minecraft.net/"
    } else {
        "http://xboxlive.com"
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

fn sign_request(method: &str, target: &str, authorization: &str, body: &[u8]) -> Result<String, i32> {
    let timestamp = current_filetime();
    let mut hasher = Sha256::new();
    hasher.update(SIGNATURE_POLICY_VERSION.to_be_bytes());
    hasher.update([0]);
    hasher.update(timestamp.to_be_bytes());
    hasher.update([0]);
    hasher.update(method.as_bytes());
    hasher.update([0]);
    hasher.update(target.as_bytes());
    hasher.update([0]);
    hasher.update(authorization.as_bytes());
    hasher.update([0]);
    // Current Xbox title-service policies used by Minecraft do not require
    // additional policy headers. The request body is signed exactly as sent.
    hasher.update(body);
    hasher.update([0]);
    let mut digest: [u8; 32] = hasher.finalize().into();
    let mut signature = SESSION
        .get()
        .ok_or(E_FAIL)?
        .signing_key
        .sign_hash(&mut digest)?;
    digest.zeroize();

    let mut header = [0u8; SIGNATURE_HEADER_SIZE];
    header[0..4].copy_from_slice(&SIGNATURE_POLICY_VERSION.to_be_bytes());
    header[4..12].copy_from_slice(&timestamp.to_be_bytes());
    header[12..].copy_from_slice(&signature);
    signature.zeroize();
    let encoded = STANDARD.encode(header);
    header.zeroize();
    Ok(encoded)
}

fn user_vtable() -> *const XUserVtable {
    USER_VTABLE.get_or_init(|| XUserVtable {
        slots: [
            user_query_interface as usize,
            add_ref as usize,
            release as usize,
            duplicate_handle as usize,
            close_user_handle as usize,
            compare_users as usize,
            get_max_users as usize,
            add_async as usize,
            add_result as usize,
            get_local_id as usize,
            find_by_local_id as usize,
            get_id as usize,
            find_by_id as usize,
            get_is_guest as usize,
            get_state as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            get_age_group as usize,
            check_privilege as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            token_async as usize,
            token_result_size as usize,
            token_result as usize,
            token_utf16_async as usize,
            token_result_size as usize,
            token_utf16_result as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_boolean as usize,
            stub_hresult as usize,
            stub_void as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_boolean as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_hresult as usize,
            stub_boolean as usize,
            stub_hresult as usize,
            stub_hresult as usize,
        ],
    })
}

fn gamertag_vtable() -> *const XUserGamertagVtable {
    GAMERTAG_VTABLE.get_or_init(|| XUserGamertagVtable {
        slots: [
            user_query_interface as usize,
            add_ref as usize,
            release as usize,
            get_gamertag as usize,
        ],
    })
}

fn current_filetime() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    WINDOWS_TO_UNIX_EPOCH_SECONDS
        .saturating_add(duration.as_secs())
        .saturating_mul(FILETIME_TICKS_PER_SECOND)
        .saturating_add(u64::from(duration.subsec_nanos()) / 100)
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_header_shape_is_stable() {
        assert_eq!(SIGNATURE_HEADER_SIZE, 76);
        assert_eq!(STANDARD.encode([0u8; SIGNATURE_HEADER_SIZE]).len(), 104);
    }

    #[test]
    fn xuser_vtable_has_expected_slot_count() {
        assert_eq!(mem::size_of::<XUserVtable>(), 50 * mem::size_of::<usize>());
    }

    #[test]
    fn presence_uses_xbox_live_relying_party() {
        assert_eq!(
            relying_party_for_url("https://userpresence.xboxlive.com/users/xuid(1)/devices/current"),
            "http://xboxlive.com"
        );
    }
}

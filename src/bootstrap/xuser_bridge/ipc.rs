// SPDX-License-Identifier: GPL-3.0-or-later

use base64::{Engine as _, engine::general_purpose::STANDARD};
use core::ffi::c_void;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{mem, ptr, time::{SystemTime, UNIX_EPOCH}};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::{
    abi::{
        XUserLocalId, XUSER_AGE_GROUP_ADULT, XUSER_AGE_GROUP_CHILD,
        XUSER_AGE_GROUP_TEEN, XUSER_AGE_GROUP_UNKNOWN,
    },
    crypto::SigningKey,
};

const PIPE_MAGIC: &[u8; 8] = b"BMCBLXU1";
const PIPE_VERSION: u32 = 1;
const PIPE_HEADER_SIZE: usize = 80;
const MAX_PAYLOAD_SIZE: usize = 256 * 1024;
const MIN_TOKEN_REMAINING_SECONDS: u64 = 30;

const GENERIC_READ: u32 = 0x8000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

const XBOX_LIVE_RP: &str = "http://xboxlive.com";
const PLAYFAB_RP: &str = "https://b980a380.minecraft.playfabapi.com/";
const MULTIPLAYER_RP: &str = "https://multiplayer.minecraft.net/";
const REALMS_RP: &str = "https://pocket.realms.minecraft.net/";
const LICENSING_RP: &str = "http://licensing.xboxlive.com";

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
}

struct OwnedHandle(*mut c_void);

impl OwnedHandle {
    fn new(handle: *mut c_void) -> Option<Self> {
        (!handle.is_null() && handle as isize != -1).then_some(Self(handle))
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 as isize != -1 {
            unsafe {
                let _ = CloseHandle(self.0);
            }
            self.0 = ptr::null_mut();
        }
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct TokenRecord {
    pub token: String,
    pub user_hash: String,
    pub relying_party: String,
    #[zeroize(skip)]
    pub expires_at: u64,
}

pub struct Session {
    pub xuid: u64,
    pub local_id: XUserLocalId,
    pub gamertag: String,
    pub age_group: u32,
    pub privileges: Vec<u32>,
    pub tokens: Vec<TokenRecord>,
    pub signing_key: SigningKey,
}

unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Session {
    pub fn token_for_relying_party(&self, relying_party: &str) -> Option<&TokenRecord> {
        self.tokens
            .iter()
            .find(|token| token.relying_party == relying_party)
    }
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct WireDocument {
    ecc_private_blob_b64: String,
    xbl_xuid: String,
    xbl_gamertag: String,
    #[serde(default)]
    xbl_age_group: Option<String>,
    #[serde(default)]
    xbl_privileges: Option<String>,

    xbl_token: String,
    xbl_uhs: String,
    xbl_token_expiry_epoch: String,

    sisu_token: String,
    sisu_uhs: String,
    #[serde(default)]
    sisu_rp: Option<String>,
    sisu_expiry_epoch: String,

    mp_token: String,
    mp_uhs: String,
    #[serde(default)]
    mp_rp: Option<String>,
    mp_expiry_epoch: String,

    realms_token: String,
    realms_uhs: String,
    #[serde(default)]
    realms_rp: Option<String>,
    realms_expiry_epoch: String,

    #[serde(default)]
    lic_token: Option<String>,
    #[serde(default)]
    lic_uhs: Option<String>,
    #[serde(default)]
    lic_rp: Option<String>,
    #[serde(default)]
    lic_expiry_epoch: Option<String>,
}

/// Opens exactly one BMCBL pipe named for the current Minecraft PID. If the
/// pipe does not exist, this returns immediately and the caller must leave the
/// official Microsoft XUser implementation untouched.
pub fn receive_session() -> Result<Option<Session>, String> {
    let mut payload = match receive_payload()? {
        Some(payload) => payload,
        None => return Ok(None),
    };
    let result = parse_session(&payload).map(Some);
    payload.zeroize();
    result
}

fn receive_payload() -> Result<Option<Vec<u8>>, String> {
    let current_pid = unsafe { GetCurrentProcessId() };
    let pipe_name = wide(&format!(r"\\.\pipe\BMCBL.XUser.{current_pid}"));
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
    let Some(pipe) = OwnedHandle::new(handle) else {
        return Ok(None);
    };

    let mut server_pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut server_pid) } == 0 {
        return Err("unable to identify BMCBL pipe server PID".to_string());
    }
    let parent_pid = parent_process_id(current_pid)
        .ok_or_else(|| "unable to identify Minecraft parent PID".to_string())?;
    if server_pid != parent_pid {
        return Err("pipe server is not the Minecraft parent process".to_string());
    }

    let mut header = [0u8; PIPE_HEADER_SIZE];
    read_exact(pipe.0, &mut header)?;
    if &header[..8] != PIPE_MAGIC {
        return Err("invalid XUser pipe magic".to_string());
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
        return Err("invalid XUser transport header".to_string());
    }

    let now = now_epoch();
    if issued_at > now.saturating_add(30)
        || expires_at <= now
        || expires_at.saturating_sub(issued_at) > 120
    {
        return Err("XUser transport window expired".to_string());
    }

    let mut payload = vec![0u8; payload_len];
    if let Err(error) = read_exact(pipe.0, &mut payload) {
        payload.zeroize();
        return Err(error);
    }
    let digest: [u8; 32] = Sha256::digest(&payload).into();
    if digest != expected_digest {
        payload.zeroize();
        return Err("XUser payload digest mismatch".to_string());
    }
    Ok(Some(payload))
}

fn parse_session(payload: &[u8]) -> Result<Session, String> {
    let document: WireDocument = serde_json::from_slice(payload)
        .map_err(|_| "pre-authentication payload is not valid JSON".to_string())?;

    let mut private_blob = STANDARD
        .decode(&document.ecc_private_blob_b64)
        .map_err(|_| "invalid P-256 private key encoding".to_string())?;
    let signing_key = SigningKey::import_private_blob(mem::take(&mut private_blob))?;
    private_blob.zeroize();

    let xuid = parse_nonzero_decimal(&document.xbl_xuid)
        .ok_or_else(|| "invalid XUID".to_string())?;
    let local_id = parse_nonzero_decimal(&document.xbl_uhs).unwrap_or(xuid);
    if document.xbl_gamertag.trim().is_empty() {
        return Err("empty gamertag".to_string());
    }

    let age_group = match document
        .xbl_age_group
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "child" => XUSER_AGE_GROUP_CHILD,
        "teen" | "teenager" => XUSER_AGE_GROUP_TEEN,
        "adult" => XUSER_AGE_GROUP_ADULT,
        _ => XUSER_AGE_GROUP_UNKNOWN,
    };
    let mut privileges = document
        .xbl_privileges
        .as_deref()
        .unwrap_or_default()
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter_map(|value| value.parse::<u32>().ok())
        .collect::<Vec<_>>();
    privileges.sort_unstable();
    privileges.dedup();

    let mut tokens = vec![
        token(
            &document.xbl_token,
            &document.xbl_uhs,
            XBOX_LIVE_RP,
            &document.xbl_token_expiry_epoch,
        )?,
        token(
            &document.sisu_token,
            &document.sisu_uhs,
            checked_rp(document.sisu_rp.as_deref(), PLAYFAB_RP)?,
            &document.sisu_expiry_epoch,
        )?,
        token(
            &document.mp_token,
            &document.mp_uhs,
            checked_rp(document.mp_rp.as_deref(), MULTIPLAYER_RP)?,
            &document.mp_expiry_epoch,
        )?,
        token(
            &document.realms_token,
            &document.realms_uhs,
            checked_rp(document.realms_rp.as_deref(), REALMS_RP)?,
            &document.realms_expiry_epoch,
        )?,
    ];

    match (
        document.lic_token.as_deref(),
        document.lic_uhs.as_deref(),
        document.lic_expiry_epoch.as_deref(),
    ) {
        (None, None, None) => {}
        (Some(value), Some(hash), Some(expiry)) => tokens.push(token(
            value,
            hash,
            checked_rp(document.lic_rp.as_deref(), LICENSING_RP)?,
            expiry,
        )?),
        _ => return Err("incomplete licensing token".to_string()),
    }

    Ok(Session {
        xuid,
        local_id: XUserLocalId { value: local_id },
        gamertag: document.xbl_gamertag.clone(),
        age_group,
        privileges,
        tokens,
        signing_key,
    })
}

fn token(
    value: &str,
    user_hash: &str,
    relying_party: &str,
    expiry: &str,
) -> Result<TokenRecord, String> {
    let expires_at = expiry
        .parse::<u64>()
        .map_err(|_| "invalid token expiry".to_string())?;
    if value.is_empty()
        || parse_nonzero_decimal(user_hash).is_none()
        || relying_party.is_empty()
        || expires_at <= now_epoch().saturating_add(MIN_TOKEN_REMAINING_SECONDS)
    {
        return Err("invalid or expired Xbox token".to_string());
    }
    Ok(TokenRecord {
        token: value.to_string(),
        user_hash: user_hash.to_string(),
        relying_party: relying_party.to_string(),
        expires_at,
    })
}

fn checked_rp<'a>(provided: Option<&'a str>, expected: &'a str) -> Result<&'a str, String> {
    match provided {
        Some(value) if value != expected => Err("unexpected Xbox relying party".to_string()),
        Some(value) => Ok(value),
        None => Ok(expected),
    }
}

fn parse_nonzero_decimal(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|value| *value != 0)
}

fn parent_process_id(current_pid: u32) -> Option<u32> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    let snapshot = OwnedHandle::new(snapshot)?;
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
            return Err("pipe closed before the full XUser session was received".to_string());
        }
        offset += read as usize;
    }
    Ok(())
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
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_identifiers_are_strict() {
        assert_eq!(parse_nonzero_decimal("123"), Some(123));
        assert_eq!(parse_nonzero_decimal("0"), None);
        assert_eq!(parse_nonzero_decimal("12x"), None);
    }

    #[test]
    fn relying_parties_cannot_be_replaced() {
        assert!(checked_rp(Some(MULTIPLAYER_RP), MULTIPLAYER_RP).is_ok());
        assert!(checked_rp(Some(XBOX_LIVE_RP), MULTIPLAYER_RP).is_err());
    }
}

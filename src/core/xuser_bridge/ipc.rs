// SPDX-License-Identifier: GPL-3.0-only

use core::ffi::c_void;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    mem, ptr,
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::{
    abi::{
        XUSER_AGE_GROUP_ADULT, XUSER_AGE_GROUP_CHILD, XUSER_AGE_GROUP_TEEN,
        XUSER_AGE_GROUP_UNKNOWN, XUserLocalId,
    },
    bridge_debug, bridge_info,
};

const PIPE_MAGIC: &[u8; 8] = b"BMCBLXU1";
const PIPE_VERSION: u32 = 1;
const PIPE_HEADER_SIZE: usize = 80;
const MAX_PAYLOAD_SIZE: usize = 256 * 1024;
const MIN_TOKEN_REMAINING_SECONDS: u64 = 30;
const AUTH_MODE: &str = "official-runtime-user-token-v4";

const GENERIC_READ: u32 = 0x8000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

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
pub struct UserToken {
    pub token: String,
    #[zeroize(skip)]
    pub expires_at: u64,
}

/// Secret-free route summary used only by startup diagnostics.
pub struct TokenRecord {
    pub relying_party: String,
    pub expires_at: u64,
}

pub struct Session {
    pub xuid: u64,
    pub local_id: XUserLocalId,
    pub gamertag: String,
    pub age_group: u32,
    pub privileges: Vec<u32>,
    pub tokens: Vec<TokenRecord>,
    pub user_token: UserToken,
    /// Non-secret BMCBL observation of the Windows Xbox identity. This value
    /// only gates whether native AddDefaultUserSilently may be attempted; any
    /// returned native handle is independently verified by GetId before use.
    pub native_system_xuid_hint: Option<u64>,
}

unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Session {
    pub fn custom_user_token(&self) -> Option<&str> {
        (self.user_token.expires_at > now_epoch().saturating_add(MIN_TOKEN_REMAINING_SECONDS))
            .then_some(self.user_token.token.as_str())
    }

    pub fn native_same_account_hint(&self) -> bool {
        self.native_system_xuid_hint == Some(self.xuid)
    }
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct WireDocument {
    #[serde(default)]
    auth_mode: Option<String>,
    xbl_xuid: String,
    xbl_gamertag: String,
    #[serde(default)]
    xbl_age_group: Option<String>,
    #[serde(default)]
    xbl_privileges: Option<String>,
    user_token: String,
    user_token_expiry_epoch: String,
    #[serde(default)]
    native_system_xuid_hint: Option<String>,
}

/// Opens exactly one BMCBL pipe named for the current Minecraft PID. If the
/// pipe does not exist, this returns immediately and the caller must leave the
/// official Microsoft XUser implementation untouched.
pub fn receive_session() -> Result<Option<Session>, String> {
    let mut payload = match receive_payload()? {
        Some(payload) => payload,
        None => return Ok(None),
    };
    bridge_debug(&format!(
        "BMCBL XUser pipe payload 已完整接收 | payload_bytes={} | next=parse-and-zeroize",
        payload.len()
    ));
    let result = parse_session(&payload).map(Some);
    payload.zeroize();
    bridge_debug("BMCBL XUser pipe 原始 payload 已 zeroize");
    result
}

fn receive_payload() -> Result<Option<Vec<u8>>, String> {
    let current_pid = unsafe { GetCurrentProcessId() };
    let pipe_name_text = format!(r"\\.\pipe\BMCBL.XUser.{current_pid}");
    bridge_debug(&format!(
        "BMCBL XUser pipe 探测 | minecraft_pid={current_pid} | pipe={pipe_name_text}"
    ));
    let pipe_name = wide(&pipe_name_text);
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
        bridge_debug("BMCBL XUser pipe 不存在 | action=official-xuser-passthrough");
        return Ok(None);
    };
    bridge_info(&format!(
        "BMCBL XUser pipe 已连接 | minecraft_pid={current_pid} | transport=named-pipe | access=read-once"
    ));

    let mut server_pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut server_pid) } == 0 {
        return Err("unable to identify BMCBL pipe server PID".to_string());
    }
    let parent_pid = parent_process_id(current_pid)
        .ok_or_else(|| "unable to identify Minecraft parent PID".to_string())?;
    bridge_debug(&format!(
        "BMCBL XUser pipe peer 验证 | server_pid={server_pid} | minecraft_parent_pid={parent_pid}"
    ));
    if server_pid != parent_pid {
        return Err(format!(
            "pipe server is not the Minecraft parent process | server_pid={server_pid} parent_pid={parent_pid}"
        ));
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

    bridge_debug(&format!(
        "BMCBL XUser transport header | protocol={version} | target_pid={target_pid} | launcher_pid={launcher_pid} | issued_at={issued_at} | expires_at={expires_at} | payload_bytes={payload_len} | digest=sha256-redacted"
    ));
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
        return Err(format!(
            "XUser transport window expired | now={now} issued_at={issued_at} expires_at={expires_at}"
        ));
    }
    bridge_debug(&format!(
        "BMCBL XUser transport window accepted | remaining={}s | max_window=120s",
        expires_at.saturating_sub(now)
    ));

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
    bridge_info(&format!(
        "BMCBL XUser transport verified | peer_pid={server_pid} | payload_bytes={payload_len} | digest=SHA-256-ok | secrets_logged=false"
    ));
    Ok(Some(payload))
}

fn parse_session(payload: &[u8]) -> Result<Session, String> {
    let document: WireDocument = serde_json::from_slice(payload).map_err(|error| {
        let kind = match error.classify() {
            serde_json::error::Category::Data => "utoken-v4-schema-mismatch",
            serde_json::error::Category::Syntax => "json-syntax-invalid",
            serde_json::error::Category::Eof => "json-truncated",
            serde_json::error::Category::Io => "json-io-error",
        };
        format!(
            "BMCBL XUser payload decode failed | kind={kind} | line={} | column={} | expected_auth_mode={AUTH_MODE}",
            error.line(),
            error.column(),
        )
    })?;
    if document.auth_mode.as_deref() != Some(AUTH_MODE) {
        return Err(format!(
            "BMCBL XUser payload protocol mismatch | expected_auth_mode={AUTH_MODE} | received_auth_mode={}",
            document.auth_mode.as_deref().unwrap_or("<missing>")
        ));
    }

    let xuid = parse_nonzero_decimal(&document.xbl_xuid)
        .ok_or_else(|| "invalid XUID".to_string())?;
    if document.xbl_gamertag.trim().is_empty() {
        return Err("empty gamertag".to_string());
    }
    let expires_at = document
        .user_token_expiry_epoch
        .parse::<u64>()
        .map_err(|_| "invalid UToken expiry".to_string())?;
    let now = now_epoch();
    if document.user_token.is_empty()
        || expires_at <= now.saturating_add(MIN_TOKEN_REMAINING_SECONDS)
    {
        return Err("invalid or expired Xbox UToken".to_string());
    }
    let native_system_xuid_hint = match document.native_system_xuid_hint.as_deref() {
        Some(value) if !value.is_empty() => Some(
            parse_nonzero_decimal(value)
                .ok_or_else(|| "invalid native system XUID hint".to_string())?,
        ),
        _ => None,
    };

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

    let native_relation = match native_system_xuid_hint {
        Some(native) if native == xuid => "same",
        Some(_) => "different",
        None => "none",
    };
    bridge_debug(&format!(
        "BMCBL XUser UToken-only v4 session parsed | xbox_xuid={xuid} | gamertag_chars={} | privilege_count={} | user_token_remaining={}s | native_system_identity={native_relation} | MSA=not-transferred | DToken=official-runtime | TToken=official-runtime | final_xsts=official-runtime | signature=official-runtime | secrets_logged=false",
        document.xbl_gamertag.chars().count(),
        privileges.len(),
        expires_at.saturating_sub(now),
    ));

    Ok(Session {
        xuid,
        local_id: XUserLocalId { value: xuid },
        gamertag: document.xbl_gamertag.clone(),
        age_group,
        privileges,
        tokens: vec![TokenRecord {
            relying_party: "xasu-user-token".to_string(),
            expires_at,
        }],
        user_token: UserToken {
            token: document.user_token.clone(),
            expires_at,
        },
        native_system_xuid_hint,
    })
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
    fn utoken_v4_wire_document_accepts_system_hint() {
        let payload = serde_json::json!({
            "auth_mode": AUTH_MODE,
            "xbl_xuid": "2535413569375435",
            "xbl_gamertag": "BMCBLTest",
            "xbl_age_group": "Adult",
            "xbl_privileges": "185 254",
            "user_token": "test-user-token",
            "user_token_expiry_epoch": "4102444800",
            "native_system_xuid_hint": "2535413569375435"
        });
        let encoded = serde_json::to_vec(&payload).unwrap();
        let document: WireDocument = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(document.auth_mode.as_deref(), Some(AUTH_MODE));
        assert_eq!(document.xbl_gamertag, "BMCBLTest");
        assert_eq!(document.user_token, "test-user-token");
        assert_eq!(
            document.native_system_xuid_hint.as_deref(),
            Some("2535413569375435")
        );
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::c_void;
use serde_json::Value;
use std::{
    ptr,
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

use super::{bridge_error, bridge_info, bridge_warn, session};

const XBOX_LIVE_RP: &str = "http://xboxlive.com";
const PRESENCE_HOST: &str = "userpresence.xboxlive.com";
const PRESENCE_CONTRACT_VERSION: &str = "3";
const PRESENCE_ENDPOINT_SUFFIX: &str = "/devices/current/titles/current";
const MAX_CAPTURED_BODY_SIZE: usize = 256 * 1024;
const XBOX_SIGNATURE_MAX_BODY_BYTES: usize = 8 * 1024;

const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const INTERNET_DEFAULT_HTTPS_PORT: u16 = 443;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

static RELAY_STARTED: AtomicBool = AtomicBool::new(false);
static RELAY_QUEUE: OnceLock<RelayQueue> = OnceLock::new();

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}

#[link(name = "winhttp")]
unsafe extern "system" {
    fn WinHttpOpen(
        user_agent: *const u16,
        access_type: u32,
        proxy_name: *const u16,
        proxy_bypass: *const u16,
        flags: u32,
    ) -> *mut c_void;
    fn WinHttpConnect(
        session: *mut c_void,
        server_name: *const u16,
        server_port: u16,
        reserved: u32,
    ) -> *mut c_void;
    fn WinHttpOpenRequest(
        connect: *mut c_void,
        verb: *const u16,
        object_name: *const u16,
        version: *const u16,
        referrer: *const u16,
        accept_types: *const *const u16,
        flags: u32,
    ) -> *mut c_void;
    fn WinHttpSetTimeouts(
        request: *mut c_void,
        resolve_timeout: i32,
        connect_timeout: i32,
        send_timeout: i32,
        receive_timeout: i32,
    ) -> i32;
    fn WinHttpSendRequest(
        request: *mut c_void,
        headers: *const u16,
        headers_length: u32,
        optional: *mut c_void,
        optional_length: u32,
        total_length: u32,
        context: usize,
    ) -> i32;
    fn WinHttpReceiveResponse(request: *mut c_void, reserved: *mut c_void) -> i32;
    fn WinHttpQueryHeaders(
        request: *mut c_void,
        info_level: u32,
        name: *const u16,
        buffer: *mut c_void,
        buffer_length: *mut u32,
        index: *mut u32,
    ) -> i32;
    fn WinHttpCloseHandle(handle: *mut c_void) -> i32;
}

struct RelayRequest {
    method: &'static str,
    path: String,
    body: Zeroizing<Vec<u8>>,
    state: Option<String>,
    rich_presence: bool,
}

struct RelayQueue {
    pending: Mutex<Option<RelayRequest>>,
    ready: Condvar,
}

struct WinHttpHandle(*mut c_void);

impl WinHttpHandle {
    fn new(handle: *mut c_void, operation: &str) -> Result<Self, String> {
        if handle.is_null() {
            Err(win32_error(operation))
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
            self.0 = ptr::null_mut();
        }
    }
}

/// Observes the request that XSAPI asks the custom XUser provider to sign.
/// This layer is reached even when XSAPI is statically linked into Minecraft
/// and therefore has no module export that MinHook can discover.
pub(crate) fn observe_token_request(method: &str, url: &str, body: &[u8]) {
    let Some((host, path)) = parse_https_target(url) else {
        return;
    };
    if !host.eq_ignore_ascii_case(PRESENCE_HOST) || !path.ends_with(PRESENCE_ENDPOINT_SUFFIX) {
        return;
    }

    let method = if method.eq_ignore_ascii_case("POST") {
        "POST"
    } else if method.eq_ignore_ascii_case("DELETE") {
        "DELETE"
    } else {
        return;
    };

    if body.len() > MAX_CAPTURED_BODY_SIZE {
        bridge_warn(&format!(
            "Token Presence Relay 拒绝过大的 Presence 正文 | method={method} | body_bytes={} | limit={MAX_CAPTURED_BODY_SIZE}",
            body.len()
        ));
        return;
    }

    let (state, rich_presence) = if method == "POST" {
        let Ok(json) = serde_json::from_slice::<Value>(body) else {
            bridge_warn(&format!(
                "Token Presence Relay 捕获到非 JSON Presence 正文；不进行独立重放 | body_bytes={}",
                body.len()
            ));
            return;
        };
        if !json.is_object() {
            bridge_warn("Token Presence Relay 捕获到非对象 Presence JSON；不进行独立重放");
            return;
        }
        (
            json.get("state")
                .and_then(Value::as_str)
                .map(sanitize_state),
            json.pointer("/activity/richPresence").is_some(),
        )
    } else {
        (None, false)
    };

    let Some(runtime) = session() else {
        return;
    };
    ensure_started();

    // Never trust or reuse the XUID embedded in the observed URL. The relay is
    // always rewritten to the account authenticated by the BMCBL pipe session.
    let canonical_path = format!(
        "/users/xuid({})/devices/current/titles/current",
        runtime.xuid
    );
    enqueue(RelayRequest {
        method,
        path: canonical_path,
        body: Zeroizing::new(body.to_vec()),
        state: state.clone(),
        rich_presence,
    });

    bridge_info(&format!(
        "Token Presence Relay 已捕获 XSAPI Presence 请求 | method={method} | state={} | rich_presence={rich_presence} | body_bytes={} | source_path={}",
        state.as_deref().unwrap_or("<none>"),
        body.len(),
        sanitize_path(&path),
    ));
}

fn ensure_started() {
    if RELAY_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    RELAY_QUEUE.get_or_init(|| RelayQueue {
        pending: Mutex::new(None),
        ready: Condvar::new(),
    });

    bridge_info(
        "Token Presence Relay 已启用；不依赖 XblPresenceSetPresenceAsync 导出，将从 XUser Token 请求捕获并重放 Rich Presence",
    );
    if let Err(error) = thread::Builder::new()
        .name("bloader-token-presence-relay".to_string())
        .spawn(relay_loop)
    {
        RELAY_STARTED.store(false, Ordering::Release);
        bridge_error(&format!(
            "Token Presence Relay 线程启动失败 | reason={error}"
        ));
    }
}

fn enqueue(request: RelayRequest) {
    let Some(queue) = RELAY_QUEUE.get() else {
        return;
    };
    let mut pending = match queue.pending.lock() {
        Ok(pending) => pending,
        Err(poisoned) => poisoned.into_inner(),
    };
    // Latest-wins: replacing an older request immediately zeroizes its body.
    *pending = Some(request);
    queue.ready.notify_one();
}

fn relay_loop() {
    loop {
        let request = {
            let Some(queue) = RELAY_QUEUE.get() else {
                return;
            };
            let mut pending = match queue.pending.lock() {
                Ok(pending) => pending,
                Err(poisoned) => poisoned.into_inner(),
            };
            while pending.is_none() {
                pending = match queue.ready.wait(pending) {
                    Ok(pending) => pending,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
            pending.take().expect("Presence relay request disappeared")
        };

        match send_request(&request) {
            Ok(status) => bridge_info(&format!(
                "Token Presence Relay 已提交 Xbox 状态 | http_status={status} | method={} | state={} | rich_presence={} | body_bytes={}",
                request.method,
                request.state.as_deref().unwrap_or("<none>"),
                request.rich_presence,
                request.body.len(),
            )),
            Err(error) => bridge_warn(&format!(
                "Token Presence Relay Xbox 状态提交失败 | method={} | state={} | rich_presence={} | body_bytes={} | reason={error}",
                request.method,
                request.state.as_deref().unwrap_or("<none>"),
                request.rich_presence,
                request.body.len(),
            )),
        }
    }
}

fn send_request(request: &RelayRequest) -> Result<u32, String> {
    let runtime = session().ok_or_else(|| "XUser session unavailable".to_string())?;
    let token = runtime
        .token_for_relying_party(XBOX_LIVE_RP)
        .ok_or_else(|| "Xbox Live relying-party token unavailable".to_string())?;
    if token.expires_at <= now_epoch().saturating_add(30) {
        return Err("Xbox Live token expired or has less than 30 seconds remaining".to_string());
    }

    let authorization = Zeroizing::new(format!(
        "XBL3.0 x={};{}",
        token.user_hash, token.token
    ));
    let body_to_sign = &request.body[..request.body.len().min(XBOX_SIGNATURE_MAX_BODY_BYTES)];
    let signature = Zeroizing::new(
        runtime
            .signing_key
            .sign_request(
                request.method,
                &request.path,
                &authorization,
                &[],
                body_to_sign,
            )
            .map_err(|status| {
                format!("P-256 request signing failed: 0x{:08X}", status as u32)
            })?,
    );

    let headers = if request.method == "POST" {
        Zeroizing::new(format!(
            "Authorization: {}\r\nSignature: {}\r\nx-xbl-contract-version: {}\r\nContent-Type: application/json\r\nAccept: application/json\r\n",
            &*authorization, &*signature, PRESENCE_CONTRACT_VERSION,
        ))
    } else {
        Zeroizing::new(format!(
            "Authorization: {}\r\nSignature: {}\r\nx-xbl-contract-version: {}\r\nAccept: application/json\r\n",
            &*authorization, &*signature, PRESENCE_CONTRACT_VERSION,
        ))
    };

    winhttp_request(request.method, &request.path, &headers, &request.body)
}

fn winhttp_request(
    method: &str,
    path: &str,
    headers: &str,
    body: &[u8],
) -> Result<u32, String> {
    let user_agent = wide(&format!(
        "BLoader Token Presence Relay/{}",
        env!("CARGO_PKG_VERSION")
    ));
    let host = wide(PRESENCE_HOST);
    let verb = wide(method);
    let object_name = wide(path);
    let wide_headers = wide(headers);

    let session = WinHttpHandle::new(
        unsafe {
            WinHttpOpen(
                user_agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                ptr::null(),
                ptr::null(),
                0,
            )
        },
        "WinHttpOpen",
    )?;
    let connect = WinHttpHandle::new(
        unsafe {
            WinHttpConnect(
                session.0,
                host.as_ptr(),
                INTERNET_DEFAULT_HTTPS_PORT,
                0,
            )
        },
        "WinHttpConnect",
    )?;
    let request = WinHttpHandle::new(
        unsafe {
            WinHttpOpenRequest(
                connect.0,
                verb.as_ptr(),
                object_name.as_ptr(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        },
        "WinHttpOpenRequest",
    )?;

    unsafe {
        let _ = WinHttpSetTimeouts(request.0, 5_000, 8_000, 8_000, 12_000);
    }
    let body_length = u32::try_from(body.len())
        .map_err(|_| "Presence request body exceeds WinHTTP limits".to_string())?;
    let header_length = u32::try_from(wide_headers.len().saturating_sub(1))
        .map_err(|_| "Presence request headers exceed WinHTTP limits".to_string())?;
    let body_pointer = if body.is_empty() {
        ptr::null_mut()
    } else {
        body.as_ptr() as *mut c_void
    };

    if unsafe {
        WinHttpSendRequest(
            request.0,
            wide_headers.as_ptr(),
            header_length,
            body_pointer,
            body_length,
            body_length,
            0,
        )
    } == 0
    {
        return Err(win32_error("WinHttpSendRequest"));
    }
    if unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) } == 0 {
        return Err(win32_error("WinHttpReceiveResponse"));
    }

    let mut status = 0u32;
    let mut status_size = core::mem::size_of::<u32>() as u32;
    if unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            ptr::null(),
            (&mut status as *mut u32).cast(),
            &mut status_size,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(win32_error("WinHttpQueryHeaders(status)"));
    }

    if (200..300).contains(&status) {
        Ok(status)
    } else {
        Err(format!("Xbox Presence service returned HTTP {status}"))
    }
}

fn parse_https_target(url: &str) -> Option<(String, String)> {
    let authority_and_path = url.strip_prefix("https://")?;
    let authority_end = authority_and_path
        .char_indices()
        .find_map(|(index, character)| matches!(character, '/' | '?' | '#').then_some(index))
        .unwrap_or(authority_and_path.len());
    let authority = &authority_and_path[..authority_end];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if let Some(value) = host_port.strip_prefix('[') {
        value.split_once(']')?.0
    } else {
        host_port.split_once(':').map_or(host_port, |(host, _)| host)
    };
    if host.is_empty() {
        return None;
    }

    let suffix = &authority_and_path[authority_end..];
    let suffix = suffix.split_once('#').map_or(suffix, |(value, _)| value);
    let path = if suffix.is_empty() {
        "/".to_string()
    } else if suffix.starts_with('?') {
        format!("/{suffix}")
    } else {
        suffix.to_string()
    };
    Some((host.to_ascii_lowercase(), path))
}

fn sanitize_state(value: &str) -> String {
    let value = value
        .chars()
        .filter(|character| !character.is_control())
        .take(32)
        .collect::<String>();
    if value.is_empty() {
        "<empty>".to_string()
    } else {
        value
    }
}

fn sanitize_path(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn win32_error(operation: &str) -> String {
    let code = unsafe { GetLastError() };
    format!("{operation} failed with Win32 error {code}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_presence_target_without_exposing_query_fragment() {
        let (host, path) = parse_https_target(
            "https://userpresence.xboxlive.com/users/xuid(1)/devices/current/titles/current",
        )
        .unwrap();
        assert_eq!(host, PRESENCE_HOST);
        assert!(path.ends_with(PRESENCE_ENDPOINT_SUFFIX));
    }

    #[test]
    fn rejects_non_https_targets() {
        assert!(parse_https_target("http://userpresence.xboxlive.com/path").is_none());
    }

    #[test]
    fn presence_json_shape_is_detectable() {
        let json: Value = serde_json::from_slice(
            br#"{"state":"active","activity":{"richPresence":{"id":"playingMap"}}}"#,
        )
        .unwrap();
        assert_eq!(json["state"], "active");
        assert!(json.pointer("/activity/richPresence").is_some());
    }
}

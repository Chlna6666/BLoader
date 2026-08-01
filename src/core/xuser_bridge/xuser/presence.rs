// SPDX-License-Identifier: GPL-3.0-or-later

use core::ffi::{c_char, c_void};
use minhook::MinHook;
use serde_json::{Map, Value};
use std::{
    collections::HashSet,
    ptr,
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

use super::super::{
    abi::{
        E_INVALIDARG, E_POINTER, HResult, S_OK, XAsyncBlock, XAsyncOp,
        XAsyncProviderData,
    },
    bridge_error, bridge_info, bridge_warn, session, xasync,
};

const XBL_SCID_LENGTH: usize = 40;
const MAX_PRESENCE_ID_LENGTH: usize = 256;
const MAX_PRESENCE_TOKEN_LENGTH: usize = 256;
const MAX_PRESENCE_TOKEN_COUNT: usize = 32;
const XBOX_LIVE_RP: &str = "http://xboxlive.com";
const PRESENCE_HOST: &str = "userpresence.xboxlive.com";
const PRESENCE_CONTRACT_VERSION: &str = "3";

const TH32CS_SNAPMODULE: u32 = 0x0000_0008;
const TH32CS_SNAPMODULE32: u32 = 0x0000_0010;
const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const INTERNET_DEFAULT_HTTPS_PORT: u16 = 443;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

static BRIDGE_STARTED: AtomicBool = AtomicBool::new(false);
static UPDATE_QUEUE: OnceLock<UpdateQueue> = OnceLock::new();
static HOOKED_TARGETS: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
static PRESENCE_ASYNC_IDENTITY: u8 = 0x50;
const PRESENCE_ASYNC_NAME: &[u8] = b"BLoaderPresenceBridge\0";

#[repr(C)]
struct XblPresenceRichPresenceIds {
    scid: [c_char; XBL_SCID_LENGTH],
    presence_id: *const c_char,
    presence_token_ids: *const *const c_char,
    presence_token_ids_count: usize,
}

#[repr(C)]
struct ModuleEntry32W {
    size: u32,
    module_id: u32,
    process_id: u32,
    global_usage: u32,
    process_usage: u32,
    base_address: *mut u8,
    base_size: u32,
    module: *mut c_void,
    module_name: [u16; 256],
    executable_path: [u16; 260],
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
    fn Module32FirstW(snapshot: *mut c_void, entry: *mut ModuleEntry32W) -> i32;
    fn Module32NextW(snapshot: *mut c_void, entry: *mut ModuleEntry32W) -> i32;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
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

#[derive(Clone, Debug)]
struct RichPresence {
    scid: String,
    presence_id: String,
    token_ids: Vec<String>,
}

#[derive(Clone, Debug)]
struct PresenceUpdate {
    active: bool,
    rich: Option<RichPresence>,
}

struct PresenceAsyncContext {
    update: Option<PresenceUpdate>,
}

struct UpdateQueue {
    pending: Mutex<Option<PresenceUpdate>>,
    ready: Condvar,
}

struct OwnedKernelHandle(*mut c_void);

impl OwnedKernelHandle {
    fn new(handle: *mut c_void) -> Option<Self> {
        (!handle.is_null() && handle as isize != -1).then_some(Self(handle))
    }
}

impl Drop for OwnedKernelHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 as isize != -1 {
            unsafe {
                let _ = CloseHandle(self.0);
            }
            self.0 = ptr::null_mut();
        }
    }
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

pub fn ensure_started() {
    if session().is_none()
        || BRIDGE_STARTED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }

    UPDATE_QUEUE.get_or_init(|| UpdateQueue {
        pending: Mutex::new(None),
        ready: Condvar::new(),
    });
    HOOKED_TARGETS.get_or_init(|| Mutex::new(HashSet::new()));

    bridge_info(
        "Presence Bridge 启动；将接管 XblPresenceSetPresenceAsync，并使用 BMCBL 账户独立提交 Xbox Rich Presence",
    );

    if let Err(error) = thread::Builder::new()
        .name("bloader-presence-sender".to_string())
        .spawn(presence_sender_loop)
    {
        bridge_error(&format!("Presence Bridge 发送线程启动失败 | reason={error}"));
        return;
    }

    if let Err(error) = thread::Builder::new()
        .name("bloader-presence-hook".to_string())
        .spawn(presence_hook_loop)
    {
        bridge_error(&format!("Presence Bridge Hook 线程启动失败 | reason={error}"));
    }
}

unsafe extern "system" fn presence_set_hook(
    xbl_context: *mut c_void,
    is_user_active: u8,
    rich_presence_ids: *const XblPresenceRichPresenceIds,
    async_block: *mut XAsyncBlock,
) -> HResult {
    if xbl_context.is_null() || async_block.is_null() {
        return E_INVALIDARG;
    }

    let update = unsafe { capture_update(is_user_active != 0, rich_presence_ids) };
    let context = Box::into_raw(Box::new(PresenceAsyncContext {
        update: Some(update),
    }));
    let result = unsafe {
        xasync::begin(
            async_block,
            context.cast(),
            (&PRESENCE_ASYNC_IDENTITY as *const u8).cast(),
            PRESENCE_ASYNC_NAME.as_ptr().cast(),
            presence_async_provider,
        )
    };
    if result < 0 {
        unsafe {
            drop(Box::from_raw(context));
        }
    }
    result
}

unsafe extern "system" fn presence_async_provider(
    operation: XAsyncOp,
    provider_data: *const XAsyncProviderData,
) -> HResult {
    if provider_data.is_null() {
        return E_POINTER;
    }
    let provider_data = unsafe { &*provider_data };
    let context = provider_data.context.cast::<PresenceAsyncContext>();
    if context.is_null() {
        return E_POINTER;
    }

    match operation {
        XAsyncOp::Begin => unsafe { xasync::schedule(provider_data.async_block, 0) },
        XAsyncOp::DoWork => {
            if let Some(update) = unsafe { &mut *context }.update.take() {
                enqueue_update(update);
            }
            unsafe {
                xasync::complete(provider_data.async_block, S_OK, 0);
            }
            S_OK
        }
        XAsyncOp::GetResult | XAsyncOp::Cancel => S_OK,
        XAsyncOp::Cleanup => {
            unsafe {
                drop(Box::from_raw(context));
            }
            S_OK
        }
    }
}

fn presence_hook_loop() {
    let mut scan_count = 0u64;
    loop {
        scan_count = scan_count.saturating_add(1);
        match discover_presence_exports() {
            Ok(exports) => install_presence_hooks(exports),
            Err(error) if scan_count == 1 || scan_count % 120 == 0 => {
                bridge_warn(&format!(
                    "Presence Bridge 暂时无法枚举 XSAPI 模块；将继续重试 | reason={error}"
                ));
            }
            Err(_) => {}
        }

        let has_hook = HOOKED_TARGETS
            .get()
            .and_then(|targets| targets.lock().ok().map(|targets| !targets.is_empty()))
            .unwrap_or(false);
        thread::sleep(if has_hook {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(50)
        });
    }
}

fn discover_presence_exports() -> Result<Vec<(usize, String)>, String> {
    let snapshot = unsafe {
        CreateToolhelp32Snapshot(
            TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32,
            GetCurrentProcessId(),
        )
    };
    let snapshot = OwnedKernelHandle::new(snapshot)
        .ok_or_else(|| win32_error("CreateToolhelp32Snapshot"))?;

    let mut entry: ModuleEntry32W = unsafe { core::mem::zeroed() };
    entry.size = core::mem::size_of::<ModuleEntry32W>() as u32;
    if unsafe { Module32FirstW(snapshot.0, &mut entry) } == 0 {
        return Err(win32_error("Module32FirstW"));
    }

    let mut exports = Vec::new();
    loop {
        let target = unsafe {
            GetProcAddress(
                entry.module,
                b"XblPresenceSetPresenceAsync\0".as_ptr(),
            )
        };
        if !target.is_null() {
            exports.push((target as usize, utf16_array(&entry.module_name)));
        }

        entry.size = core::mem::size_of::<ModuleEntry32W>() as u32;
        if unsafe { Module32NextW(snapshot.0, &mut entry) } == 0 {
            break;
        }
    }
    Ok(exports)
}

fn install_presence_hooks(exports: Vec<(usize, String)>) {
    let Some(targets) = HOOKED_TARGETS.get() else {
        return;
    };
    let mut targets = match targets.lock() {
        Ok(targets) => targets,
        Err(poisoned) => poisoned.into_inner(),
    };

    let mut created = 0usize;
    for (address, module_name) in exports {
        if targets.contains(&address) {
            continue;
        }
        let result = unsafe {
            MinHook::create_hook(
                address as *mut c_void,
                presence_set_hook as *const () as *mut c_void,
            )
        };
        match result {
            Ok(trampoline) => {
                targets.insert(address);
                created += 1;
                bridge_info(&format!(
                    "Presence Bridge 已定位并接管 XSAPI 导出 | module={module_name} | XblPresenceSetPresenceAsync=0x{address:X} | trampoline=0x{:X}",
                    trampoline as usize,
                ));
            }
            Err(status) => bridge_warn(&format!(
                "Presence Bridge 无法 Hook XSAPI 导出 | module={module_name} | address=0x{address:X} | status={status:?}"
            )),
        }
    }

    if (!targets.is_empty() || created != 0)
        && unsafe { MinHook::enable_all_hooks() }.is_err()
    {
        bridge_error("Presence Bridge MinHook 启用失败；Rich Presence 将继续使用官方 XSAPI 路径");
    }
}

unsafe fn capture_update(
    active: bool,
    rich_presence_ids: *const XblPresenceRichPresenceIds,
) -> PresenceUpdate {
    if rich_presence_ids.is_null() {
        return PresenceUpdate { active, rich: None };
    }

    let source = unsafe { &*rich_presence_ids };
    let scid = fixed_ascii(&source.scid);
    let presence_id = unsafe { pointer_ascii(source.presence_id, MAX_PRESENCE_ID_LENGTH) };
    let Some(scid) = scid else {
        bridge_warn("Presence Bridge 收到无效 SCID；本次仅提交 active/inactive 状态");
        return PresenceUpdate { active, rich: None };
    };
    let Some(presence_id) = presence_id else {
        bridge_warn("Presence Bridge 收到无效 Presence ID；本次仅提交 active/inactive 状态");
        return PresenceUpdate { active, rich: None };
    };

    let requested_count = source.presence_token_ids_count;
    if requested_count != 0 && source.presence_token_ids.is_null() {
        bridge_warn("Presence Bridge 的 Presence Token 数量非零但数组为空；忽略 Token 参数");
    }
    let count = requested_count.min(MAX_PRESENCE_TOKEN_COUNT);
    if requested_count > MAX_PRESENCE_TOKEN_COUNT {
        bridge_warn(&format!(
            "Presence Bridge Presence Token 数量超过上限；已截断 | requested={requested_count} | limit={MAX_PRESENCE_TOKEN_COUNT}"
        ));
    }

    let mut token_ids = Vec::with_capacity(count);
    if !source.presence_token_ids.is_null() {
        for index in 0..count {
            let token = unsafe { source.presence_token_ids.add(index).read() };
            if let Some(token) = unsafe { pointer_ascii(token, MAX_PRESENCE_TOKEN_LENGTH) } {
                token_ids.push(token);
            } else {
                bridge_warn(&format!(
                    "Presence Bridge 忽略无效 Presence Token | index={index}"
                ));
            }
        }
    }

    PresenceUpdate {
        active,
        rich: Some(RichPresence {
            scid,
            presence_id,
            token_ids,
        }),
    }
}

fn enqueue_update(update: PresenceUpdate) {
    let Some(queue) = UPDATE_QUEUE.get() else {
        return;
    };
    let mut pending = match queue.pending.lock() {
        Ok(pending) => pending,
        Err(poisoned) => poisoned.into_inner(),
    };
    *pending = Some(update);
    queue.ready.notify_one();
}

fn presence_sender_loop() {
    loop {
        let update = {
            let Some(queue) = UPDATE_QUEUE.get() else {
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
            pending.take().unwrap()
        };

        let rich = update.rich.is_some();
        let token_count = update
            .rich
            .as_ref()
            .map(|rich| rich.token_ids.len())
            .unwrap_or(0);
        match send_presence(&update) {
            Ok(status) => bridge_info(&format!(
                "Presence Bridge 已提交 Xbox 状态 | http_status={status} | active={} | rich_presence={rich} | token_count={token_count}",
                update.active,
            )),
            Err(error) => bridge_warn(&format!(
                "Presence Bridge Xbox 状态提交失败 | active={} | rich_presence={rich} | token_count={token_count} | reason={error}",
                update.active,
            )),
        }
    }
}

fn send_presence(update: &PresenceUpdate) -> Result<u32, String> {
    let runtime = session().ok_or_else(|| "XUser session unavailable".to_string())?;
    let token = runtime
        .token_for_relying_party(XBOX_LIVE_RP)
        .ok_or_else(|| "Xbox Live relying-party token unavailable".to_string())?;
    if token.expires_at <= now_epoch().saturating_add(30) {
        return Err("Xbox Live token expired or has less than 30 seconds remaining".to_string());
    }

    let request_path = format!(
        "/users/xuid({})/devices/current/titles/current",
        runtime.xuid
    );
    let body = serialize_update(update)?;
    let authorization = Zeroizing::new(format!(
        "XBL3.0 x={};{}",
        token.user_hash, token.token
    ));
    let signature = Zeroizing::new(
        runtime
            .signing_key
            .sign_request("POST", &request_path, &authorization, &[], &body)
            .map_err(|status| format!("P-256 request signing failed: 0x{:08X}", status as u32))?,
    );
    let headers = Zeroizing::new(format!(
        "Authorization: {}\r\nSignature: {}\r\nx-xbl-contract-version: {}\r\nContent-Type: application/json\r\nAccept: application/json\r\n",
        &*authorization, &*signature, PRESENCE_CONTRACT_VERSION,
    ));

    winhttp_post(&request_path, &headers, &body)
}

fn serialize_update(update: &PresenceUpdate) -> Result<Vec<u8>, String> {
    let mut root = Map::new();
    root.insert(
        "state".to_string(),
        Value::String(if update.active { "active" } else { "inactive" }.to_string()),
    );

    if let Some(rich) = &update.rich {
        let mut rich_presence = Map::new();
        rich_presence.insert("id".to_string(), Value::String(rich.presence_id.clone()));
        rich_presence.insert("scid".to_string(), Value::String(rich.scid.clone()));
        if !rich.token_ids.is_empty() {
            rich_presence.insert(
                "params".to_string(),
                Value::Array(
                    rich.token_ids
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }

        let mut activity = Map::new();
        activity.insert("richPresence".to_string(), Value::Object(rich_presence));
        root.insert("activity".to_string(), Value::Object(activity));
    }

    serde_json::to_vec(&Value::Object(root))
        .map_err(|error| format!("unable to serialize Presence payload: {error}"))
}

fn winhttp_post(path: &str, headers: &str, body: &[u8]) -> Result<u32, String> {
    let user_agent = wide(&format!(
        "BLoader Presence Bridge/{}",
        env!("CARGO_PKG_VERSION")
    ));
    let host = wide(PRESENCE_HOST);
    let verb = wide("POST");
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

fn fixed_ascii(value: &[c_char]) -> Option<String> {
    let length = value
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(value.len());
    ascii_identifier(value[..length].iter().map(|value| *value as u8))
}

unsafe fn pointer_ascii(value: *const c_char, limit: usize) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let mut bytes = Vec::new();
    for index in 0..limit {
        let byte = unsafe { value.add(index).read() } as u8;
        if byte == 0 {
            return ascii_identifier(bytes.into_iter());
        }
        bytes.push(byte);
    }
    None
}

fn ascii_identifier(bytes: impl Iterator<Item = u8>) -> Option<String> {
    let bytes = bytes.collect::<Vec<_>>();
    if bytes.is_empty()
        || bytes
            .iter()
            .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
    {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn utf16_array(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..length])
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
    use super::{PresenceUpdate, RichPresence, serialize_update};

    #[test]
    fn serializes_the_same_shape_as_xsapi_title_request() {
        let body = serialize_update(&PresenceUpdate {
            active: true,
            rich: Some(RichPresence {
                scid: "00000000-0000-0000-0000-000000000001".to_string(),
                presence_id: "playingMap".to_string(),
                token_ids: vec!["overworld".to_string(), "normal".to_string()],
            }),
        })
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["state"], "active");
        assert_eq!(json["activity"]["richPresence"]["id"], "playingMap");
        assert_eq!(
            json["activity"]["richPresence"]["params"][0],
            "overworld"
        );
    }

    #[test]
    fn state_only_update_omits_activity() {
        let body = serialize_update(&PresenceUpdate {
            active: false,
            rich: None,
        })
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["state"], "inactive");
        assert!(json.get("activity").is_none());
    }
}

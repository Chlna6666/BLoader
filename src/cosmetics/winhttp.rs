//! Native WinHTTP responses: bounded sync buffering, async callback capture,
//! and opt-in virtual local Persona ZIP resources. No TLS interception.
//! BLoader 移植版：日志走 BLoader logging（scope=cosmetics），钩子经 `minhook`
//! crate 安装，运行时装配由 `super::mod` 在 bootstrap 阶段完成。
use super::response::{Config, Mode};
use super::{TextureMode, codec, debug, diagnostics, local_capes, persona};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use crate::platform::logging;
use minhook::MinHook;

type H = *mut c_void;
type B = i32;
const ASYNC_FLAG: u32 = 0x1000_0000;
const QUERY_NUMBER: u32 = 0x2000_0000;
const QUERY_CONTENT_TYPE: u32 = 1;
const QUERY_CUSTOM: u32 = 65535;
const QUERY_VERSION: u32 = 18;
const QUERY_STATUS_TEXT: u32 = 20;
const QUERY_CONTENT_LENGTH: u32 = 5;
const QUERY_STATUS_CODE: u32 = 19;
const QUERY_CONTENT_ENCODING: u32 = 29;
const QUERY_TRANSFER_ENCODING: u32 = 63;
const QUERY_RAW_HEADERS: u32 = 21;
const QUERY_RAW_HEADERS_CRLF: u32 = 22;
const ERROR_WINHTTP_HEADER_NOT_FOUND: u32 = 12150;
const ERROR_IO_PENDING: u32 = 997;
const STATUS_HEADERS: u32 = 0x0002_0000;
const STATUS_AVAILABLE: u32 = 0x0004_0000;
const STATUS_READ: u32 = 0x0008_0000;
const STATUS_REQUEST_ERROR: u32 = 0x0020_0000;
const STATUS_SEND_COMPLETE: u32 = 0x0040_0000;
const STATUS_HANDLE_CLOSING: u32 = 0x0000_0800;
const ADDITIONAL_CALLBACK_FLAGS: u32 =
    STATUS_HEADERS | STATUS_READ | STATUS_REQUEST_ERROR | STATUS_HANDLE_CLOSING;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const REQUEST_BODY_MAX: usize = 128 * 1024;
const READ_CHUNK: usize = 16 * 1024;

// Async responses are captured by a dedicated, single-reader worker.
// Never issue WinHttpReadData recursively inside the game's WinHTTP callback.

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> H;
    fn GetProcAddress(module: H, name: *const u8) -> H;
    fn SetLastError(code: u32);
    fn GetLastError() -> u32;
}

type OpenFn = unsafe extern "system" fn(*const u16, u32, *const u16, *const u16, u32) -> H;
type ConnectFn = unsafe extern "system" fn(H, *const u16, u16, u32) -> H;
type OpenRequestFn = unsafe extern "system" fn(
    H,
    *const u16,
    *const u16,
    *const u16,
    *const u16,
    *const *const u16,
    u32,
) -> H;
type SendFn = unsafe extern "system" fn(H, *const u16, u32, H, u32, u32, usize) -> B;
type WriteFn = unsafe extern "system" fn(H, *const c_void, u32, *mut u32) -> B;
type ReceiveFn = unsafe extern "system" fn(H, *mut c_void) -> B;
type ReadFn = unsafe extern "system" fn(H, H, u32, *mut u32) -> B;
type AvailableFn = unsafe extern "system" fn(H, *mut u32) -> B;
type HeadersFn = unsafe extern "system" fn(H, u32, *const u16, H, *mut u32, *mut u32) -> B;
type CloseFn = unsafe extern "system" fn(H) -> B;
type StatusCallback = unsafe extern "system" fn(H, usize, u32, H, u32);
type SetCallbackFn = unsafe extern "system" fn(H, H, u32, usize) -> H;

static OPEN: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static CONNECT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static OPEN_REQUEST: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static SEND: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static WRITE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static RECEIVE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static READ: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static AVAILABLE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static HEADERS: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static CLOSE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static SET_CALLBACK: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

macro_rules! original {
    ($slot:ident, $type:ty) => {{
        // Each trampoline is stored before the hook group is enabled.
        unsafe { std::mem::transmute::<H, $type>($slot.load(Ordering::Acquire)) }
    }};
}

struct Runtime {
    cfg: Config,
    root: PathBuf,
    cape_roots: Vec<PathBuf>,
    texture_mode: TextureMode,
    locale: String,
}
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
/// config.json 的 enable_cosmetics 总开关（热重载可关闭；重新开启需重启游戏）。
static MASTER_ENABLED: AtomicBool = AtomicBool::new(false);

/// BLoader 移植：日志统一走 BLoader logging（scope=cosmetics）。
/// 默认日志保持安静——`logger()` 承载的全部遥测明细都落在 debug 级
/// （log_level=debug/trace 才可见）；info 级只保留启动状态与扫描摘要，
/// 失败包单独以 warn 输出。作者自用，无需独立 DLL 时代的开源研究式遥测。
fn logger(message: &str) {
    logging::scoped_debug_message("cosmetics", message);
}
fn info_log(message: &str) {
    logging::scoped_info_message("cosmetics", message);
}
fn warn_log(message: &str) {
    logging::scoped_warn_message("cosmetics", message);
}
fn debug_log(message: &str) {
    if RUNTIME.get().is_some_and(|r| r.cfg.debug.verbose) {
        logging::scoped_debug_message("cosmetics", &format!("[DEBUG] {message}"));
    }
}
fn debug_json(request_id: u64, stage: &str, bytes: &[u8]) {
    if RUNTIME
        .get()
        .is_some_and(|r| r.cfg.debug.verbose && r.cfg.debug.response_shape)
    {
        logging::scoped_debug_message(
            "cosmetics",
            &format!(
                "[DEBUG] request #{request_id}: {stage} {}",
                debug::json_summary(bytes)
            ),
        );
    }
}

#[derive(Default)]
struct Tables {
    sessions: HashMap<usize, bool>,
    connections: HashMap<usize, (String, bool)>,
    requests: HashMap<usize, Request>,
    callbacks: HashMap<usize, CallbackRegistration>,
}
#[derive(Clone, Copy)]
struct CallbackRegistration {
    address: usize,
    flags: u32,
}
struct Request {
    id: u64,
    diagnostic_route: Option<String>,
    catalog_checked: bool,
    body_overflow: bool,
    intercept_enabled: bool,
    bridge_installed: bool,
    host: String,
    path: String,
    method: String,
    async_session: bool,
    request_body: Vec<u8>,
    replay: Option<Replay>,
    capture: Option<AsyncCapture>,
    /// Set once the async prefetch delivered HEADERS to the game: from then
    /// on the game reads only the replay, and ReceiveResponse is a no-op.
    async_prefetch_done: bool,
    synthetic: bool,
    status: u32,
    callback_context: usize,
}
struct AsyncCapture {
    bytes: Vec<u8>,
    buffer: Box<[u8; READ_CHUNK]>,
    limit: usize,
    encoding: codec::Encoding,
    events: mpsc::Sender<AsyncReadEvent>,
    io_guard: Arc<Mutex<()>>,
    cancelled: Arc<AtomicBool>,
}
enum AsyncReadEvent {
    Chunk,
    Eof,
    TooLarge,
    Failed {
        context: usize,
        payload: Vec<u8>,
    },
}
struct Replay {
    bytes: Vec<u8>,
    // HEAD advertises the GET representation's length but sends no body.
    head_content_length: Option<usize>,
    position: usize,
    complete: bool,
    replaced: bool,
    catalog: bool,
    zip: bool,
    png: bool,
    strip_encoding: bool,
    status: u32,
    delivery_logged: bool,
}
static TABLES: OnceLock<Mutex<Tables>> = OnceLock::new();
fn tables() -> &'static Mutex<Tables> {
    TABLES.get_or_init(|| Mutex::new(Tables::default()))
}
fn read_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let mut n = 0usize;
        while n < 2048 && *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}
fn state_key(handle: H) -> usize {
    handle as usize
}
fn should_intercept(cfg: &Config, host: &str, path: &str, method: &str) -> bool {
    MASTER_ENABLED.load(Ordering::Acquire)
        && cfg.enabled
        && (host.eq_ignore_ascii_case(persona::LOCAL_HOST)
            || cfg.match_rule(host, path, method).is_some())
}

/// BLoader 移植：由 `super::install` 在装钩子之前装配运行时
/// （规则配置、Cosmetics 数据目录、披风扫描根、纹理模式）。
pub fn set_runtime(
    cfg: Config,
    root: PathBuf,
    cape_roots: Vec<PathBuf>,
    texture_mode: TextureMode,
    locale: String,
) -> Result<(), String> {
    RUNTIME
        .set(Runtime {
            cfg,
            root,
            cape_roots,
            texture_mode,
            locale,
        })
        .map_err(|_| "cosmetics runtime already initialized".to_string())
}

/// config.json 总开关热重载：关闭后立即停止拦截；重新开启需重启游戏。
pub fn set_master_enabled(enabled: bool) {
    let previous = MASTER_ENABLED.swap(enabled, Ordering::SeqCst);
    if previous != enabled {
        logger(&format!(
            "master_enabled={enabled} (config.json enable_cosmetics)"
        ));
    }
}

/// 后台扫描线程入口：解析披风根并输出扫描摘要。
/// 默认日志只保留一行结果摘要（info）与失败包警告（warn）；
/// 横幅、逐包注册、规则清单等明细全部为 debug 级遥测。
pub fn worker_startup() {
    let Some(runtime) = RUNTIME.get() else { return };
    let texture_mode = runtime.texture_mode;
    let roots = runtime.cape_roots.clone();
    let registry = local_capes::initialize(&roots, texture_mode, &runtime.locale);
    logger("============================================================");
    logger(&format!(
        "BLoader {} built-in cosmetics | origin=Chlna6666/Cosmetics | license=GPL-3.0-only",
        crate::platform::build_info::VERSION
    ));
    logger("Source: https://github.com/Chlna6666/Cosmetics");
    logger(
        "Scope: local cape catalog, virtual thumbnails and Persona download; no Minecraft skin/equip hook",
    );
    logger(&format!(
        "logs=bloader latest.log scope=cosmetics texture_mode={}",
        texture_mode.as_str()
    ));
    logger(&format!(
        "local_capes ZIP_SCAN complete discovered={} validated={} rejected={} skipped={} roots={} registered_items={} registered_assets={} native_persona={} generated_persona={} in_game_loaded=UNVERIFIED",
        registry.discovered_packs,
        registry.accepted,
        registry.rejected,
        registry
            .discovered_packs
            .saturating_sub(registry.accepted + registry.rejected),
        registry.discovered_roots,
        local_capes::count(),
        registry.asset_count(),
        registry.native_payloads,
        registry.generated_persona_payloads
    ));
    for diagnostic in &registry.diagnostics {
        // 失败包是用户需要看到的唯一逐包信息（带原因）；其余明细走 debug。
        if let Some(reason) = diagnostic.strip_prefix("CAPE_REJECTED ") {
            warn_log(&format!("cape pack rejected: {reason}"));
        } else {
            logger(&format!("local_capes: {diagnostic}"));
        }
    }
    info_log(&format!(
        "cape scan discovered={} loaded={} failed={} items={}",
        registry.discovered_packs,
        registry.accepted,
        registry.rejected,
        local_capes::count()
    ));
    for (_index, item) in local_capes::items().iter().enumerate() {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let name = item.get("title").and_then(|v| v.as_str()).unwrap_or("?");
        let author = item
            .get("creatorName")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let pack_id = item
            .pointer("/packIdentity/0/uuid")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let pack_version = item
            .pointer("/packIdentity/0/version")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        logger(&format!(
            "local_capes ITEM_REGISTERED name={name} author={author} item_uuid={id} pack_uuid={pack_id} pack_version={pack_version} game_loaded=UNVERIFIED"
        ));
    }
    logger(&format!(
        "session_start build_version={} module=WinHTTP built_into=BLoader fail_open=true",
        crate::platform::build_info::VERSION
    ));
    logger(&format!(
        "loaded_rules={} enabled_rules={} diagnostic_fields=redacted",
        runtime.cfg.rules.len(),
        runtime.cfg.rules.iter().filter(|r| r.enabled).count()
    ));
    for rule in &runtime.cfg.rules {
        logger(&format!(
            "rule={} enabled={} method={} mode={:?} local_only={}",
            rule.name, rule.enabled, rule.method, rule.mode, rule.local_only
        ));
    }
    if !runtime.cfg.enabled {
        info_log(
            "SAFE_MODE: cosmetics.json enabled=false; WinHTTP hooks NOT installed; all requests remain native",
        );
        return;
    }
    logger("official_cape_preview: client-only UI projection, not authenticated entitlements");
    logger(
        "No third-party catalog or entitlement service is contacted; original account receipts remain untouched",
    );
    logger(&format!(
        "debug verbose={} response_shape={}; raw response bodies and credentials are never written",
        runtime.cfg.debug.verbose, runtime.cfg.debug.response_shape
    ));
}

pub unsafe fn install() -> Result<(), String> {
    let dll_name: Vec<u16> = "winhttp.dll".encode_utf16().chain(Some(0)).collect();
    let module = LoadLibraryW(dll_name.as_ptr());
    if module.is_null() {
        return Err("winhttp.dll not available".into());
    }
    let defs: &[(&[u8], H, &AtomicPtr<c_void>)] = &[
        (b"WinHttpOpen\0", on_open as H, &OPEN),
        (b"WinHttpConnect\0", on_connect as H, &CONNECT),
        (b"WinHttpOpenRequest\0", on_open_request as H, &OPEN_REQUEST),
        (b"WinHttpSendRequest\0", on_send as H, &SEND),
        (b"WinHttpWriteData\0", on_write as H, &WRITE),
        (b"WinHttpReceiveResponse\0", on_receive as H, &RECEIVE),
        (b"WinHttpReadData\0", on_read as H, &READ),
        (
            b"WinHttpQueryDataAvailable\0",
            on_available as H,
            &AVAILABLE,
        ),
        (b"WinHttpQueryHeaders\0", on_headers as H, &HEADERS),
        (b"WinHttpCloseHandle\0", on_close as H, &CLOSE),
        (
            b"WinHttpSetStatusCallback\0",
            on_set_callback as H,
            &SET_CALLBACK,
        ),
    ];
    let mut installed: Vec<H> = Vec::new();
    for (symbol, detour, slot) in defs {
        let addr = GetProcAddress(module, symbol.as_ptr());
        if addr.is_null() {
            return Err(format!("WinHTTP export missing: {:?}", symbol));
        }
        match MinHook::create_hook(addr as *mut c_void, *detour as *mut c_void) {
            Ok(trampoline) => {
                slot.store(trampoline, Ordering::Release);
                installed.push(addr);
            }
            Err(error) => {
                return Err(format!("MH_CreateHook failed for {:?}: {error:?}", symbol));
            }
        }
    }
    if let Err(error) = MinHook::enable_all_hooks() {
        return Err(format!("MH_ApplyQueued failed: {error:?}"));
    }
    Ok(())
}

unsafe extern "system" fn on_open(
    agent: *const u16,
    access: u32,
    proxy: *const u16,
    bypass: *const u16,
    flags: u32,
) -> H {
    let real = original!(OPEN, OpenFn);
    let h = real(agent, access, proxy, bypass, flags);
    if !h.is_null() {
        tables()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sessions
            .insert(state_key(h), flags & ASYNC_FLAG != 0);
    }
    h
}
unsafe extern "system" fn on_connect(
    session: H,
    server: *const u16,
    port: u16,
    reserved: u32,
) -> H {
    let real = original!(CONNECT, ConnectFn);
    let h = real(session, server, port, reserved);
    if !h.is_null() {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        let is_async = state
            .sessions
            .get(&state_key(session))
            .copied()
            .unwrap_or(true);
        state
            .connections
            .insert(state_key(h), (read_wide(server), is_async));
        if let Some(cb) = state.callbacks.get(&state_key(session)).copied() {
            state.callbacks.insert(state_key(h), cb);
        }
    }
    h
}
unsafe extern "system" fn on_open_request(
    connection: H,
    verb: *const u16,
    path: *const u16,
    version: *const u16,
    referer: *const u16,
    accepts: *const *const u16,
    flags: u32,
) -> H {
    let real = original!(OPEN_REQUEST, OpenRequestFn);
    let h = real(connection, verb, path, version, referer, accepts, flags);
    if !h.is_null() {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        let (host, async_session) = state
            .connections
            .get(&state_key(connection))
            .cloned()
            .unwrap_or_else(|| (String::new(), true));
        let method = if verb.is_null() {
            "GET".into()
        } else {
            read_wide(verb)
        };
        let path = read_wide(path);
        let id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let diagnostic_route = RUNTIME.get().and_then(|rt| {
            rt.cfg.observe_rule(&host, &path, &method).map(|rule| {
                format!(
                    "{} ({})",
                    rule.name,
                    if rule.enabled { "enabled" } else { "disabled" }
                )
            })
        });
        let is_commerce_host = RUNTIME.get().is_some_and(|rt| {
            rt.cfg
                .rules
                .iter()
                .any(|rule| rule.host.eq_ignore_ascii_case(&host))
        });
        let (host_for_diagnostics, path_for_diagnostics, method_for_diagnostics) =
            (host.clone(), path.clone(), method.clone());
        // Disabled rules must not alter WinHTTP callbacks or response behavior.
        let intercept_enabled = RUNTIME
            .get()
            .is_some_and(|rt| should_intercept(&rt.cfg, &host, &path, &method));
        let parent_callback = if intercept_enabled && async_session {
            state.callbacks.get(&state_key(connection)).copied()
        } else {
            None
        };
        let req = Request {
            id,
            diagnostic_route: diagnostic_route.clone(),
            catalog_checked: false,
            body_overflow: false,
            intercept_enabled,
            bridge_installed: false,
            host,
            path,
            method,
            async_session,
            request_body: Vec::new(),
            replay: None,
            capture: None,
            synthetic: false,
            status: 200,
            callback_context: 0,
            async_prefetch_done: false,
        };
        state.requests.insert(state_key(h), req);
        drop(state);
        // WinHTTP callbacks configured on session/connection are inherited
        // unchanged. Re-register our bridge ONLY on a selected request handle.
        // This leaves Catalog/Search and all other marketplace requests alone.
        if let Some(cb) = parent_callback.filter(|cb| cb.address != 0) {
            let result = original!(SET_CALLBACK, SetCallbackFn)(
                h,
                status_bridge as H,
                cb.flags | ADDITIONAL_CALLBACK_FLAGS,
                0,
            );
            if result as usize != usize::MAX {
                let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
                state.callbacks.insert(state_key(h), cb);
                if let Some(req) = state.requests.get_mut(&state_key(h)) {
                    req.bridge_installed = true;
                }
                drop(state);
                debug_log(&format!(
                    "request #{id}: callback bridge installed on target request only"
                ));
            } else {
                logger(&format!(
                    "request #{id}: target callback bridge installation failed; original WinHTTP handler retained"
                ));
            }
        }
        if host_for_diagnostics.eq_ignore_ascii_case(persona::LOCAL_HOST) {
            let asset_kind = if path_for_diagnostics.starts_with("/personabinary/") {
                "persona_zip"
            } else if path_for_diagnostics.starts_with("/thumbnails/") {
                "thumbnail_png"
            } else {
                "unknown_asset"
            };
            logger(&format!(
                "request #{id}: LOCAL_ASSET_REQUEST type={asset_kind} method={} async={async_session} uri=https://{}{}",
                debug::redact_route_method(&method_for_diagnostics),
                persona::LOCAL_HOST,
                path_for_diagnostics.split(['?', '#']).next().unwrap_or("")
            ));
        }
        if let Some(name) = diagnostic_route {
            logger(&format!(
                "request #{id}: route matched {name}; async={async_session}"
            ));
        } else if is_commerce_host {
            let route = diagnostics::route_label(
                &host_for_diagnostics,
                &path_for_diagnostics,
                &method_for_diagnostics,
            );
            logger(&format!(
                "request #{id}: unmatched commerce request {route}; async={async_session}; action=passthrough"
            ));
        }
    }
    h
}

// The caller's callback is stored separately; our callback is only a bridge.
// No application callback is invoked while the state mutex is held.
unsafe fn notify_game(h: H, context: usize, status: u32, info: H, len: u32) {
    let reg = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.callbacks.get(&state_key(h)).copied()
    };
    if let Some(reg) = reg {
        if reg.address != 0 && reg.flags & status != 0 {
            let f: StatusCallback = std::mem::transmute(reg.address);
            f(h, context, status, info, len);
        }
    }
}
unsafe extern "system" fn on_set_callback(h: H, callback: H, flags: u32, reserved: usize) -> H {
    let real = original!(SET_CALLBACK, SetCallbackFn);
    let enabled = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state
            .requests
            .get(&state_key(h))
            .is_some_and(|req| req.intercept_enabled)
    };
    let is_normal = !callback.is_null() && callback as usize != usize::MAX;
    if !enabled {
        // Parent session/connection and all irrelevant requests remain
        // exactly as the game registered them: original callback, original
        // notification mask. Only observe for inheritance by eligible children.
        let previous = real(h, callback, flags, reserved);
        if previous as usize != usize::MAX {
            let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
            if is_normal {
                state.callbacks.insert(
                    state_key(h),
                    CallbackRegistration {
                        address: callback as usize,
                        flags,
                    },
                );
            } else {
                state.callbacks.remove(&state_key(h));
            }
        }
        return previous;
    }
    let old = {
        tables()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .callbacks
            .get(&state_key(h))
            .copied()
    };
    let replacement = if is_normal {
        status_bridge as H
    } else {
        callback
    };
    let effective_flags = if is_normal {
        flags | ADDITIONAL_CALLBACK_FLAGS
    } else {
        flags
    };
    let previous = real(h, replacement, effective_flags, reserved);
    if previous as usize == usize::MAX {
        return previous;
    }
    let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
    if is_normal {
        state.callbacks.insert(
            state_key(h),
            CallbackRegistration {
                address: callback as usize,
                flags,
            },
        );
    } else {
        state.callbacks.remove(&state_key(h));
    }
    if let Some(req) = state.requests.get_mut(&state_key(h)) {
        req.bridge_installed = is_normal;
    }
    // Never expose the internal bridge pointer to the original caller.
    if previous == status_bridge as H {
        old.map(|cb| cb.address as H).unwrap_or(ptr::null_mut())
    } else {
        previous
    }
}
unsafe fn invoke_headers(h: H, context: usize) {
    notify_game(h, context, STATUS_HEADERS, ptr::null_mut(), 0);
}
fn eligible_local_catalog(
    body: &[u8],
    declared_len: usize,
    overflow: bool,
    max_bytes: usize,
    lookup: impl FnOnce(&str) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    if overflow || declared_len == 0 || body.len() != declared_len {
        return None;
    }
    let id = super::response::catalog_id(body).ok()?;
    let data = lookup(&id)?;
    if data.is_empty() || data.len() > max_bytes {
        return None;
    }
    Some(data)
}

unsafe fn catalog_fast_response(h: H) -> bool {
    let Some(runtime) = RUNTIME.get() else {
        return false;
    };
    // Never diagnose the same item twice (ReceiveResponse and HEADERS callback).
    let found = {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        let Some(r) = state.requests.get_mut(&state_key(h)) else {
            return false;
        };
        if r.catalog_checked {
            return r
                .replay
                .as_ref()
                .is_some_and(|rep| rep.replaced && rep.catalog);
        }
        let Some(rule) = runtime.cfg.match_rule(&r.host, &r.path, &r.method) else {
            return false;
        };
        if !matches!(rule.mode, Mode::CatalogItem | Mode::ReplaceJson) {
            return false;
        }
        r.catalog_checked = true;
        (r.id, rule, r.request_body.clone(), r.body_overflow)
    };
    let (id, rule, body, overflow) = found;
    let is_local = super::response::catalog_id(&body)
        .ok()
        .and_then(|product| local_capes::catalog(&product))
        .is_some();
    let structure = super::response::catalog_request_state(&body);
    let shape = diagnostics::catalog_shape(&body);
    logger(&format!(
        "request #{id}: catalog lookup={} body_bytes={} body_state={structure} body_shape={shape} overflow={overflow}",
        rule.name,
        body.len()
    ));
    debug_log(&format!(
        "request #{id}: CATALOG_REQUEST_CAPTURED json_parse_state={structure} recognized_shape={shape} bytes={}",
        body.len()
    ));
    if overflow {
        return false;
    }
    // CatalogItem should only replace a registered local ID. Unknown official
    // item requests stay on the official PlayFab response path.
    if rule.mode == Mode::CatalogItem && structure != "valid_itemid" {
        logger(&format!(
            "request #{id}: catalog not modified; check request body capture"
        ));
        return false;
    }
    match super::response::overlay(&runtime.root, &rule, &body, b"") {
        Ok(Some(bytes)) if bytes.len() <= runtime.cfg.max_capture_bytes => {
            // Do not lock tables recursively: a previous diagnostic tried to
            // reacquire this same non-reentrant Mutex, freezing the game.
            let byte_count = bytes.len();
            let registered = {
                let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(r) = state.requests.get_mut(&state_key(h)) {
                    r.replay = Some(Replay {
                        bytes,
                        position: 0,
                        head_content_length: None,
                        complete: true,
                        replaced: true,
                        catalog: true,
                        zip: false,
                        png: false,
                        strip_encoding: true,
                        status: 200,
                        delivery_logged: false,
                    });
                    true
                } else {
                    false
                }
            };
            if !registered {
                return false;
            }
            logger(&format!(
                "request #{id}: CATALOG_RESPONSE_READY source={} route={} bytes={byte_count}",
                if is_local {
                    "local_zip"
                } else {
                    "external_file"
                },
                rule.name
            ));
            debug_log(&format!(
                "request #{id}: CATALOG_RESPONSE_REPLACED source={} (not an equip success)",
                if is_local {
                    "local_zip"
                } else {
                    "external_file"
                }
            ));
            true
        }
        Ok(None) => {
            logger(&format!(
                "request #{id}: catalog item not registered locally; passthrough"
            ));
            debug_log(&format!(
                "request #{id}: CATALOG_RESPONSE_UNCHANGED original_server_response=true"
            ));
            false
        }
        Ok(Some(_)) => {
            logger(&format!(
                "request #{id}: catalog local response exceeds bound; passthrough"
            ));
            false
        }
        Err(e) => {
            logger(&format!(
                "request #{id}: catalog response rejected ({e}); passthrough"
            ));
            false
        }
    }
}
// A HEAD request is a representation metadata probe, not a download.
// Content-Length describes the GET payload, even with zero HEAD body bytes.
fn virtual_asset_replay(method: &str, data: Vec<u8>, status: u32, png: bool) -> Replay {
    let head = method.eq_ignore_ascii_case("HEAD");
    let declared = head.then_some(data.len());
    Replay {
        bytes: if head { Vec::new() } else { data },
        position: 0,
        head_content_length: declared,
        complete: true,
        replaced: true,
        catalog: false,
        zip: !png,
        png,
        strip_encoding: false,
        status,
        delivery_logged: false,
    }
}
fn virtual_asset_method_allowed(method: &str) -> bool {
    method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD")
}
unsafe extern "system" fn on_send(
    h: H,
    headers: *const u16,
    header_len: u32,
    optional: H,
    optional_len: u32,
    total_len: u32,
    ctx: usize,
) -> B {
    if !tables()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .requests
        .get(&state_key(h))
        .is_some_and(|req| req.intercept_enabled)
    {
        return original!(SEND, SendFn)(
            h,
            headers,
            header_len,
            optional,
            optional_len,
            total_len,
            ctx,
        );
    }
    let mut captured: Option<(u64, String, usize, usize, bool)> = None;
    {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(req) = state.requests.get_mut(&state_key(h)) {
            if optional_len > 0 && !optional.is_null() {
                let count = optional_len as usize;
                if count <= REQUEST_BODY_MAX.saturating_sub(req.request_body.len()) {
                    let bytes = std::slice::from_raw_parts(optional as *const u8, count);
                    req.request_body.extend_from_slice(bytes);
                } else {
                    req.body_overflow = true;
                }
            }
            if let Some(route) = req.diagnostic_route.as_ref() {
                captured = Some((
                    req.id,
                    route.clone(),
                    req.request_body.len(),
                    total_len as usize,
                    req.body_overflow,
                ));
            }
        }
    }
    if let Some((id, route, bytes, expected, overflow)) = captured {
        logger(&format!(
            "request #{id}: send {route}; captured_body_bytes={bytes}; declared_total_bytes={expected}; overflow={overflow}"
        ));
        debug_log(&format!(
            "request #{id}: REQUEST_CAPTURED route={route} captured={bytes} total={expected} complete={} overflow={overflow}",
            bytes == expected && !overflow
        ));
    }
    let virtual_resource = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get(&state_key(h)).and_then(|r| {
            if r.host.eq_ignore_ascii_case(persona::LOCAL_HOST) {
                Some((r.path.clone(), r.method.clone(), r.async_session))
            } else {
                None
            }
        })
    };
    if let Some((path, method, async_request)) = virtual_resource {
        let mut image = false;
        let (data, status) = if !virtual_asset_method_allowed(&method) {
            logger(&format!(
                "local virtual asset refused unsupported method {method}"
            ));
            (Vec::new(), 405)
        } else if let Some((bytes, kind)) = local_capes::asset(&path) {
            image = kind == local_capes::AssetType::Png;
            logger(&format!(
                "local cape virtual resource found kind={kind:?} bytes={} source=memory url=https://{}{}",
                bytes.len(),
                persona::LOCAL_HOST,
                path
            ));
            (bytes.to_vec(), 200)
        } else {
            // The virtual URL may carry a cache-busting query. Match only
            // the path component, never allow the query into a file lookup.
            let canonical = local_capes::canonical_asset_path(&path);
            let loaded = RUNTIME
                .get()
                .map(|runtime| persona::load(&runtime.root, canonical));
            match loaded {
                Some(Ok(Some(resource))) => {
                    logger(&format!(
                        "legacy Persona ZIP found: {} ({} bytes)",
                        resource.resource_uuid,
                        resource.bytes.len()
                    ));
                    (resource.bytes, 200)
                }
                Some(Err(e)) => {
                    logger(&format!("legacy Persona ZIP rejected: {e}"));
                    (Vec::new(), 404)
                }
                _ => {
                    logger("virtual local cape asset unavailable; no network request was sent");
                    (Vec::new(), 404)
                }
            }
        };
        {
            let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = state.requests.get_mut(&state_key(h)) {
                r.synthetic = true;
                r.status = status;
                r.callback_context = ctx;
                r.replay = Some(virtual_asset_replay(&method, data, status, image));
            }
        }
        let (body_bytes, declared_bytes) = {
            let state = tables().lock().unwrap_or_else(|e| e.into_inner());
            state
                .requests
                .get(&state_key(h))
                .and_then(|r| r.replay.as_ref())
                .map_or((0, 0), |rep| {
                    (
                        rep.bytes.len(),
                        rep.head_content_length.unwrap_or(rep.bytes.len()),
                    )
                })
        };
        logger(&format!(
            "LOCAL_ASSET_RESPONSE_READY method={method} status={status} bytes={body_bytes} content_length={declared_bytes} async={async_request} (game acceptance unverified)"
        ));
        if async_request {
            notify_game(h, ctx, STATUS_SEND_COMPLETE, ptr::null_mut(), 0);
        }
        return 1;
    }
    // A registered local cape is not a PlayFab product. Avoid issuing a
    // guaranteed 400 to the remote server: serve its known catalog metadata
    // as a synthetic WinHTTP response, just like a virtual Persona ZIP.
    // Unknown IDs and incomplete/streaming POST bodies use normal WinHTTP.
    let candidate = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get(&state_key(h)).and_then(|req| {
            let rule = RUNTIME
                .get()?
                .cfg
                .match_rule(&req.host, &req.path, &req.method)?;
            (rule.mode == Mode::CatalogItem).then(|| {
                (
                    req.id,
                    req.async_session,
                    req.request_body.clone(),
                    req.body_overflow,
                )
            })
        })
    };
    if let Some((request_id, async_request, body, overflow)) = candidate {
        let max = RUNTIME
            .get()
            .map_or(4 * 1024 * 1024, |r| r.cfg.max_capture_bytes);
        if let Some(bytes) =
            eligible_local_catalog(&body, total_len as usize, overflow, max, |id| {
                local_capes::catalog(id).map(ToOwned::to_owned)
            })
        {
            let byte_count = bytes.len();
            let stored = {
                let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(req) = state.requests.get_mut(&state_key(h)) {
                    req.synthetic = true;
                    req.catalog_checked = true;
                    req.status = 200;
                    req.callback_context = ctx;
                    req.replay = Some(Replay {
                        bytes,
                        position: 0,
                        head_content_length: None,
                        complete: true,
                        replaced: true,
                        catalog: true,
                        zip: false,
                        png: false,
                        strip_encoding: true,
                        status: 200,
                        delivery_logged: false,
                    });
                    true
                } else {
                    false
                }
            };
            if stored {
                let known_id = super::response::catalog_id(&body)
                    .unwrap_or_else(|_| "unrecognized".to_string());
                logger(&format!(
                    "request #{request_id}: LOCAL_CATALOG_SYNTHETIC item_uuid={known_id} status=200 bytes={byte_count} upstream_request=skipped async={async_request}"
                ));
                if async_request {
                    notify_game(h, ctx, STATUS_SEND_COMPLETE, ptr::null_mut(), 0);
                }
                return 1;
            }
        }
    }
    if let Some(r) = tables()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .requests
        .get_mut(&state_key(h))
    {
        r.callback_context = ctx;
    }
    let sent = original!(SEND, SendFn)(
        h,
        headers,
        header_len,
        optional,
        optional_len,
        total_len,
        ctx,
    );
    let error = if sent == 0 { GetLastError() } else { 0 };
    let status = if sent != 0 {
        "accepted"
    } else if error == ERROR_IO_PENDING {
        "queued_async"
    } else {
        "failed"
    };
    let request_id = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get(&state_key(h)).and_then(|r| {
            r.diagnostic_route
                .as_ref()
                .map(|route| (r.id, route.clone()))
        })
    };
    if let Some((id, route)) = request_id {
        debug_log(&format!(
            "request #{id}: SEND_RESULT route={route} status={status} win32_error={error}"
        ));
    }
    if sent == 0 {
        SetLastError(error);
    }
    sent
}
unsafe extern "system" fn on_write(h: H, data: *const c_void, len: u32, written: *mut u32) -> B {
    if !tables()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .requests
        .get(&state_key(h))
        .is_some_and(|req| req.intercept_enabled)
    {
        return original!(WRITE, WriteFn)(h, data, len, written);
    }
    // Synthetic Persona downloads are GET-only. No write is ever sent to DNS.
    if tables()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .requests
        .get(&state_key(h))
        .is_some_and(|r| r.synthetic)
    {
        return 0;
    }
    let result = original!(WRITE, WriteFn)(h, data, len, written);
    let error = if result == 0 { GetLastError() } else { 0 };
    if !data.is_null() && len > 0 {
        let async_request = tables()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requests
            .get(&state_key(h))
            .is_some_and(|r| r.async_session);
        // In an async WinHTTP session, ERROR_IO_PENDING indicates the queued
        // write uses the caller's live buffer. Copy it now, before it is freed.
        // The diagnostic describes it as queued, not as a confirmed delivery.
        let accepted = result != 0 || (async_request && error == ERROR_IO_PENDING);
        if accepted {
            let count = if async_request {
                len as usize
            } else if written.is_null() {
                0
            } else {
                (*written as usize).min(len as usize)
            };
            let mut message = None;
            {
                let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(req) = state.requests.get_mut(&state_key(h)) {
                    if count <= REQUEST_BODY_MAX.saturating_sub(req.request_body.len()) {
                        req.request_body
                            .extend_from_slice(std::slice::from_raw_parts(
                                data as *const u8,
                                count,
                            ));
                    } else {
                        req.body_overflow = true;
                    }
                    if let Some(route) = req.diagnostic_route.as_ref() {
                        message = Some(format!(
                            "request #{}: write {route}; submitted_bytes={count}; captured_body_bytes={}; queued={}; overflow={}",
                            req.id,
                            req.request_body.len(),
                            result == 0,
                            req.body_overflow
                        ));
                    }
                }
            }
            if let Some(message) = message {
                logger(&message);
            }
        }
    }
    if result == 0 {
        SetLastError(error);
    }
    result
}

unsafe fn query_number(h: H, code: u32) -> Option<u32> {
    let mut n: u32 = 0;
    let mut bytes: u32 = 4;
    let ok = original!(HEADERS, HeadersFn)(
        h,
        code | QUERY_NUMBER,
        ptr::null(),
        &mut n as *mut u32 as H,
        &mut bytes,
        ptr::null_mut(),
    );
    (ok != 0).then_some(n)
}
unsafe fn query_string(h: H, code: u32) -> Option<String> {
    let mut wide = [0u16; 128];
    let mut bytes = (wide.len() * 2) as u32;
    let ok = original!(HEADERS, HeadersFn)(
        h,
        code,
        ptr::null(),
        wide.as_mut_ptr() as H,
        &mut bytes,
        ptr::null_mut(),
    );
    if ok == 0 {
        return None;
    }
    let end = wide.iter().position(|x| *x == 0).unwrap_or(wide.len());
    Some(String::from_utf16_lossy(&wide[..end]))
}

// Async wardrobe pages are rewritten exactly once, before the game sees
// HEADERS. A native callback may arrive reentrantly, so it only signals the
// worker; neither JSON processing nor another WinHttpReadData runs inside it.
unsafe fn begin_async_capture(h: H, context: usize) -> bool {
    let Some(runtime) = RUNTIME.get() else { return false };
    let rule = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        let Some(req) = state.requests.get(&state_key(h)) else { return false };
        if !req.async_session || !req.bridge_installed {
            return false;
        }
        let Some(rule) = runtime.cfg.match_rule(&req.host, &req.path, &req.method) else {
            return false;
        };
        if !matches!(rule.mode, Mode::AppendArray | Mode::PersonaProfile | Mode::InventoryProbe) {
            return false;
        }
        rule
    };
    if query_number(h, QUERY_STATUS_CODE) != Some(200) {
        logger(&format!("{}: unexpected status; passthrough", rule.name));
        return false;
    }
    let encoding = match codec::Encoding::parse(
        &query_string(h, QUERY_CONTENT_ENCODING).unwrap_or_default(),
    ) {
        Ok(encoding) => encoding,
        Err(error) => {
            logger(&format!("{}: {error}; passthrough", rule.name));
            return false;
        }
    };
    let limit = runtime.cfg.max_capture_bytes.clamp(1024, 16 * 1024 * 1024);
    if query_number(h, QUERY_CONTENT_LENGTH).is_some_and(|size| size as usize > limit) {
        logger(&format!("{}: response larger than capture limit; passthrough", rule.name));
        return false;
    }
    let (sender, receiver) = mpsc::channel();
    {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        let Some(req) = state.requests.get_mut(&state_key(h)) else { return false };
        if req.capture.is_some() || req.replay.is_some() {
            return false;
        }
        req.callback_context = context;
        req.capture = Some(AsyncCapture {
            bytes: Vec::new(),
            buffer: Box::new([0; READ_CHUNK]),
            limit,
            encoding,
            events: sender,
            io_guard: Arc::new(Mutex::new(())),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
    }
    let handle = h as usize;
    if let Err(error) = std::thread::Builder::new()
        .name("bloader-cape-response".to_string())
        .spawn(move || unsafe { capture_worker(handle, receiver) })
    {
        if let Some(req) = tables().lock().unwrap_or_else(|e| e.into_inner())
            .requests.get_mut(&state_key(h))
        {
            req.capture = None;
        }
        warn_log(&format!("cape response worker unavailable ({error}); passthrough"));
        return false;
    }
    logger(&format!("route={}: async response capture scheduled", rule.name));
    true
}

unsafe fn capture_worker(handle: usize, receiver: mpsc::Receiver<AsyncReadEvent>) {
    let h = handle as H;
    // The initial wardrobe request may beat the background cape scanner.
    // Wait only on this worker; no filesystem scan runs in the game callback.
    if let Some(runtime) = RUNTIME.get() {
        let _ = local_capes::initialize(
            &runtime.cape_roots,
            runtime.texture_mode,
            &runtime.locale,
        );
    }
    loop {
        let read = {
            let state = tables().lock().unwrap_or_else(|e| e.into_inner());
            state.requests.get(&state_key(h))
                .and_then(|req| req.capture.as_ref())
                .map(|capture| (
                    capture.buffer.as_ptr() as H,
                    Arc::clone(&capture.io_guard),
                    Arc::clone(&capture.cancelled),
                ))
        };
        let Some((buffer, guard, cancelled)) = read else { return };
        // Serialize native reads against WinHttpCloseHandle so cancellation
        // cannot free a request handle while a new read is being issued.
        let result = {
            let _guard = guard.lock().unwrap_or_else(|e| e.into_inner());
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let ok = original!(READ, ReadFn)(h, buffer, READ_CHUNK as u32, ptr::null_mut());
            (ok, if ok == 0 { GetLastError() } else { 0 })
        };
        if result.0 == 0 && result.1 != ERROR_IO_PENDING {
            logger(&format!("async cape read failed win32_error={}; original prefix retained", result.1));
            finish_async(h, false);
            return;
        }
        match receiver.recv() {
            Ok(AsyncReadEvent::Chunk) => {}
            Ok(AsyncReadEvent::Eof) => {
                if !cancelled.load(Ordering::Acquire) {
                    finish_async(h, true);
                }
                return;
            }
            Ok(AsyncReadEvent::TooLarge) => {
                if !cancelled.load(Ordering::Acquire) {
                    logger("async cape response reached capture limit; original stream preserved");
                    finish_async(h, false);
                }
                return;
            }
            Ok(AsyncReadEvent::Failed { context, mut payload }) => {
                // A native WinHTTP callback can run inline during ReadData.
                // Forward the error only from this worker *after* ReadData
                // releases io_guard; the game may close the handle in its
                // error callback and must not deadlock against that guard.
                if let Some(req) = tables().lock().unwrap_or_else(|e| e.into_inner())
                    .requests.get_mut(&state_key(h))
                {
                    req.capture = None;
                }
                if !cancelled.load(Ordering::Acquire) {
                    notify_game(
                        h,
                        context,
                        STATUS_REQUEST_ERROR,
                        if payload.is_empty() { ptr::null_mut() } else { payload.as_mut_ptr() as H },
                        payload.len() as u32,
                    );
                }
                return;
            }
            Err(_) => return,
        }
    }
}
unsafe fn finish_async(h: H, completed: bool) {
    let Some(runtime) = RUNTIME.get() else { return };
    let taken = {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        let Some(req) = state.requests.get_mut(&state_key(h)) else {
            return;
        };
        let Some(cap) = req.capture.take() else {
            return;
        };
        // Keep the stable storage alive until WinHTTP has reported completion.
        // There must not be an outstanding read when this path is reached.
        (
            req.id,
            req.host.clone(),
            req.path.clone(),
            req.method.clone(),
            req.request_body.clone(),
            req.callback_context,
            cap.bytes,
            cap.encoding,
        )
    };
    let (id, host, path, method, body, context, original, encoding) = taken;
    let rule = runtime.cfg.match_rule(&host, &path, &method);
    debug_log(&format!(
        "request #{id}: RESPONSE_CAPTURED async=true complete={completed} bytes={} encoding={encoding:?} matched_rule={}",
        original.len(),
        rule.as_ref().map_or("none", |r| r.name.as_str())
    ));
    if completed && runtime.cfg.debug.response_shape {
        if let Ok(decoded) = codec::decode(
            &original,
            encoding,
            runtime.cfg.max_capture_bytes.clamp(1024, 16 * 1024 * 1024),
        ) {
            debug_json(id, "UPSTREAM_JSON", &decoded);
        } else {
            debug_log(&format!(
                "request #{id}: UPSTREAM_JSON unavailable (decode failed)"
            ));
        }
    }
    let mut replay = Replay {
        bytes: original,
        position: 0,
        head_content_length: None,
        complete: completed,
        replaced: false,
        catalog: false,
        zip: false,
        png: false,
        strip_encoding: false,
        status: 200,
        delivery_logged: false,
    };
    if completed {
        if let Some(rule) = rule {
            match codec::rewrite(
                &runtime.root,
                &rule,
                &body,
                &replay.bytes,
                encoding,
                runtime.cfg.max_capture_bytes.clamp(1024, 16 * 1024 * 1024),
            ) {
                Ok(Some(change)) => {
                    let original_len = replay.bytes.len();
                    let replacement_len = change.body.len();
                    let strip_encoding = change.strip_encoding;
                    replay.bytes = change.body;
                    replay.replaced = true;
                    replay.strip_encoding = strip_encoding;
                    logger(&format!(
                        "route={}: action=replace_json async=true original_bytes={original_len} replacement_bytes={replacement_len} encoding_removed={strip_encoding}",
                        rule.name
                    ));
                    debug_log(&format!(
                        "request #{id}: RESPONSE_REPLACED successfully bytes={replacement_len} route={}",
                        rule.name
                    ));
                    debug_json(id, "MODIFIED_JSON", &replay.bytes);
                }
                Ok(None) => {
                    logger(&format!(
                        "route={}: action=passthrough reason=no_changes",
                        rule.name
                    ));
                    debug_log(&format!(
                        "request #{id}: RESPONSE_UNCHANGED duplicate_or_empty"
                    ));
                }
                Err(e) => {
                    logger(&format!(
                        "route={}: action=passthrough reason=overlay_error ({e})",
                        rule.name
                    ));
                    debug_log(&format!(
                        "request #{id}: RESPONSE_REPLACE_FAILED; original bytes retained"
                    ));
                }
            }
        }
    }
    {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(req) = state.requests.get_mut(&state_key(h)) {
            req.replay = Some(replay);
            // The prefetch consumed the real response; from here the game is
            // served exclusively from the replay. Receiving must stay inert.
            req.async_prefetch_done = true;
        }
    }
    invoke_headers(h, context);
}
unsafe extern "system" fn status_bridge(h: H, context: usize, status: u32, info: H, len: u32) {
    if status == STATUS_HEADERS {
        let note = {
            let state = tables().lock().unwrap_or_else(|e| e.into_inner());
            state.requests.get(&state_key(h)).and_then(|r| {
                r.diagnostic_route
                    .as_ref()
                    .map(|route| (r.id, route.clone(), r.async_session))
            })
        };
        if let Some((id, route, asynchronous)) = note {
            let code = query_number(h, QUERY_STATUS_CODE);
            let content_length = query_number(h, QUERY_CONTENT_LENGTH);
            let encoding = query_string(h, QUERY_CONTENT_ENCODING)
                .unwrap_or_else(|| "identity/unknown".into());
            // Only log a fixed set of known Content-Encoding values.
            let encoding_name = match encoding.to_ascii_lowercase().as_str() {
                "gzip" => "gzip",
                "br" => "br",
                "deflate" => "deflate",
                "identity" => "identity",
                "" => "none",
                _ => "other/unknown",
            };
            logger(&format!(
                "request #{id}: headers route={route} async={asynchronous} status={code:?} content_length={content_length:?} encoding={encoding_name}"
            ));
            debug_log(&format!(
                "request #{id}: RESPONSE_HEADERS_CAPTURED route={route} http_status={code:?} raw_content_length={content_length:?} encoding={encoding_name}"
            ));
        }
        if catalog_fast_response(h) {
            invoke_headers(h, context);
            return;
        }
        // Withhold HEADERS until a single worker has captured and rewritten
        // the body. The game's own reads cannot overlap the native prefetch.
        if begin_async_capture(h, context) {
            return;
        }
    } else if status == STATUS_READ {
        let captured = {
            let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
            state.requests.get_mut(&state_key(h))
                .and_then(|req| req.capture.as_mut())
                .and_then(|capture| {
                    if info != capture.buffer.as_ptr() as H {
                        return None;
                    }
                    let event = if len == 0 {
                        AsyncReadEvent::Eof
                    } else if len as usize > READ_CHUNK {
                        AsyncReadEvent::TooLarge
                    } else {
                        // Preserve the entire last read on overflow. If a
                        // response cannot be rewritten, it is replayed
                        // byte-for-byte before native reads resume.
                        capture.bytes.extend_from_slice(
                            std::slice::from_raw_parts(info as *const u8, len as usize),
                        );
                        if capture.bytes.len() > capture.limit {
                            AsyncReadEvent::TooLarge
                        } else {
                            AsyncReadEvent::Chunk
                        }
                    };
                    Some((capture.events.clone(), event))
                })
        };
        if let Some((sender, event)) = captured {
            let _ = sender.send(event);
            return;
        }
    } else if status == STATUS_REQUEST_ERROR {
        let signal = {
            let state = tables().lock().unwrap_or_else(|e| e.into_inner());
            state.requests.get(&state_key(h))
                .and_then(|req| req.capture.as_ref())
                .map(|capture| capture.events.clone())
        };
        if let Some(sender) = signal {
            // Copy the WinHTTP_ASYNC_RESULT bytes before returning from the
            // native callback. The worker forwards this exact error payload
            // after its guarded WinHttpReadData call unwinds.
            let payload = if !info.is_null() && len > 0 && len <= 64 {
                std::slice::from_raw_parts(info as *const u8, len as usize).to_vec()
            } else {
                Vec::new()
            };
            let _ = sender.send(AsyncReadEvent::Failed { context, payload });
            return;
        }
    }
    // While an async prefetch owns the body, every WinHTTP status for this
    // handle belongs to our read loop, not to the game. Forwarding HEADERS
    // here let libHttpClient start its own read concurrently with the
    // prefetch — two readers on one response, which WinHTTP rejects and the
    // game then crashed on unreceived state (libHttpClient mov r14,[rdx]
    // with rdx=0). The game is notified once, from finish_async.
    let prefetch_owns = {
        tables()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requests
            .get(&state_key(h))
            .is_some_and(|r| r.capture.is_some() || r.async_prefetch_done)
    };
    if prefetch_owns && !matches!(status, STATUS_HANDLE_CLOSING) {
        return;
    }
    notify_game(h, context, status, info, len);
    if status == STATUS_HANDLE_CLOSING {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.remove(&state_key(h));
        state.connections.remove(&state_key(h));
        state.sessions.remove(&state_key(h));
        state.callbacks.remove(&state_key(h));
    }
}

unsafe extern "system" fn on_receive(h: H, reserved: H) -> B {
    if !tables()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .requests
        .get(&state_key(h))
        .is_some_and(|req| req.intercept_enabled)
    {
        return original!(RECEIVE, ReceiveFn)(h, reserved);
    }
    let synthetic = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state
            .requests
            .get(&state_key(h))
            .filter(|r| r.synthetic)
            .map(|r| (r.async_session, r.callback_context))
    };
    if let Some((async_req, context)) = synthetic {
        if async_req {
            invoke_headers(h, context);
        }
        return 1;
    }
    // While our async prefetch owns the response body, the game must not
    // issue another WinHttpReceiveResponse: WinHTTP rejects concurrent
    // readers and libHttpClient then dereferences state that was never
    // populated (observed as mov r14,[rdx] with rdx=0 in libHttpClient).
    // The captured reply is handed back through the replay path instead.
    let capturing = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state
            .requests
            .get(&state_key(h))
            .is_some_and(|r| r.capture.is_some() || r.async_prefetch_done)
    };
    if capturing {
        return 1;
    }
    let ok = original!(RECEIVE, ReceiveFn)(h, reserved);
    if ok == 0 {
        return ok;
    }
    let note = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get(&state_key(h)).and_then(|r| {
            r.diagnostic_route
                .as_ref()
                .map(|route| (r.id, route.clone(), r.async_session))
        })
    };
    if let Some((id, route, asynchronous)) = note {
        logger(&format!(
            "request #{id}: receive_response route={route} async={asynchronous} status={:?}",
            query_number(h, QUERY_STATUS_CODE)
        ));
    }
    if catalog_fast_response(h) {
        return ok;
    }
    let (rule, body) = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        let Some(req) = state.requests.get(&state_key(h)) else {
            return ok;
        };
        if req.async_session {
            return ok;
        }
        let Some(rt) = RUNTIME.get() else { return ok };
        let Some(rule) = rt.cfg.observe_rule(&req.host, &req.path, &req.method) else {
            return ok;
        };
        if !rule.enabled {
            logger(&format!(
                "{}: response observed; overlay disabled",
                rule.name
            ));
            return ok;
        }
        (rule, req.request_body.clone())
    };
    let Some(rt) = RUNTIME.get() else { return ok };
    if query_number(h, QUERY_STATUS_CODE) != Some(200) {
        logger(&format!(
            "{}: unexpected upstream status; passthrough",
            rule.name
        ));
        return ok;
    }
    let encoding = match codec::Encoding::parse(
        &query_string(h, QUERY_CONTENT_ENCODING).unwrap_or_default(),
    ) {
        Ok(v) => v,
        Err(e) => {
            logger(&format!("{}: {e}", rule.name));
            return ok;
        }
    };
    let limit = rt.cfg.max_capture_bytes.clamp(1024, 16 * 1024 * 1024);
    if query_number(h, QUERY_CONTENT_LENGTH).is_some_and(|n| n as usize > limit) {
        logger(&format!("{}: content length exceeds limit", rule.name));
        return ok;
    }
    let mut raw = Vec::new();
    let mut completed = false;
    loop {
        let remaining = (limit + 1).saturating_sub(raw.len());
        if remaining == 0 {
            break;
        }
        let mut temp = [0u8; READ_CHUNK];
        let capacity = temp.len().min(remaining) as u32;
        let mut bytes = 0u32;
        let success = original!(READ, ReadFn)(h, temp.as_mut_ptr() as H, capacity, &mut bytes);
        if success == 0 || bytes > capacity {
            break;
        }
        if bytes == 0 {
            completed = true;
            break;
        }
        raw.extend_from_slice(&temp[..bytes as usize]);
    }
    let request_id = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get(&state_key(h)).map(|r| r.id).unwrap_or(0)
    };
    debug_log(&format!(
        "request #{request_id}: RESPONSE_CAPTURED async=false complete={completed} bytes={} encoding={encoding:?}",
        raw.len()
    ));
    if completed && rt.cfg.debug.response_shape {
        if let Ok(decoded) = codec::decode(&raw, encoding, limit) {
            debug_json(request_id, "UPSTREAM_JSON", &decoded);
        } else {
            debug_log(&format!(
                "request #{request_id}: UPSTREAM_JSON unavailable (decode failed)"
            ));
        }
    }
    let mut replay = Replay {
        bytes: raw,
        position: 0,
        head_content_length: None,
        complete: completed,
        replaced: false,
        catalog: false,
        zip: false,
        png: false,
        strip_encoding: false,
        status: 200,
        delivery_logged: false,
    };
    if completed && replay.bytes.len() <= limit {
        match codec::rewrite(&rt.root, &rule, &body, &replay.bytes, encoding, limit) {
            Ok(Some(change)) => {
                let original_len = replay.bytes.len();
                let replacement_len = change.body.len();
                let strip_encoding = change.strip_encoding;
                replay.bytes = change.body;
                replay.replaced = true;
                replay.strip_encoding = strip_encoding;
                logger(&format!(
                    "route={}: action=replace_json async=false original_bytes={original_len} replacement_bytes={replacement_len} encoding_removed={strip_encoding}",
                    rule.name
                ));
                debug_log(&format!(
                    "request #{request_id}: RESPONSE_REPLACED successfully bytes={replacement_len} route={}",
                    rule.name
                ));
                debug_json(request_id, "MODIFIED_JSON", &replay.bytes);
            }
            Ok(None) => logger(&format!(
                "route={}: action=passthrough reason=no_changes",
                rule.name
            )),
            Err(e) => logger(&format!(
                "route={}: action=passthrough reason=overlay_error ({e})",
                rule.name
            )),
        }
    } else {
        logger(&format!(
            "{}: response not completely buffered; replay original data",
            rule.name
        ));
    }
    if let Some(req) = tables()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .requests
        .get_mut(&state_key(h))
    {
        req.replay = Some(replay);
    }
    ok
}
unsafe extern "system" fn on_read(h: H, buffer: H, cap: u32, out: *mut u32) -> B {
    if buffer.is_null() && cap > 0 {
        return original!(READ, ReadFn)(h, buffer, cap, out);
    }
    let response = {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get_mut(&state_key(h)).and_then(|req| {
            let async_req = req.async_session;
            let ctx = req.callback_context;
            req.replay.as_mut().and_then(|rep| {
                let remain = rep.bytes.len().saturating_sub(rep.position);
                if remain == 0 && !rep.complete {
                    return None;
                }
                let length = remain.min(cap as usize);
                if length > 0 && !buffer.is_null() {
                    ptr::copy_nonoverlapping(
                        rep.bytes.as_ptr().add(rep.position),
                        buffer as *mut u8,
                        length,
                    );
                }
                rep.position += length;
                let completion =
                    if rep.replaced && !rep.delivery_logged && rep.position == rep.bytes.len() {
                        rep.delivery_logged = true;
                        Some((
                            req.id,
                            req.diagnostic_route
                                .clone()
                                .unwrap_or_else(|| "persona_resource".into()),
                            rep.bytes.len(),
                        ))
                    } else {
                        None
                    };
                Some((length as u32, async_req, ctx, completion))
            })
        })
    };
    if let Some((count, async_req, ctx, completion)) = response {
        if !out.is_null() {
            *out = count;
        }
        if let Some((id, route, delivered)) = completion {
            logger(&format!(
                "request #{id}: route={route} action=client_read_complete delivered_bytes={delivered}"
            ));
            debug_log(&format!(
                "request #{id}: CLIENT_CONSUMED_OVERLAY completely delivered_bytes={delivered}"
            ));
        }
        if async_req {
            notify_game(h, ctx, STATUS_READ, buffer, count);
        }
        return 1;
    }
    original!(READ, ReadFn)(h, buffer, cap, out)
}
unsafe extern "system" fn on_available(h: H, out: *mut u32) -> B {
    let replay = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get(&state_key(h)).and_then(|r| {
            r.replay.as_ref().and_then(|rep| {
                let remain = rep.bytes.len().saturating_sub(rep.position);
                if remain == 0 && !rep.complete {
                    return None;
                }
                Some((
                    remain.min(u32::MAX as usize) as u32,
                    r.async_session,
                    r.callback_context,
                ))
            })
        })
    };
    if let Some((count, async_req, ctx)) = replay {
        if !out.is_null() {
            *out = count;
        }
        if async_req {
            let mut local = count;
            notify_game(h, ctx, STATUS_AVAILABLE, &mut local as *mut u32 as H, 4);
        }
        return 1;
    }
    original!(AVAILABLE, AvailableFn)(h, out)
}
unsafe fn synthetic_header(query: u32, buffer: H, length: *mut u32, text: &str) -> B {
    if length.is_null() {
        return 0;
    }
    if query & QUERY_NUMBER != 0 {
        let number = match text.parse::<u32>() {
            Ok(v) => v,
            Err(_) => return 0,
        };
        if buffer.is_null() || *length < 4 {
            *length = 4;
            SetLastError(ERROR_INSUFFICIENT_BUFFER);
            return 0;
        }
        ptr::write_unaligned(buffer as *mut u32, number);
        *length = 4;
        return 1;
    }
    let data: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let bytes = (data.len() * 2) as u32;
    if buffer.is_null() || *length < bytes {
        *length = bytes;
        SetLastError(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    ptr::copy_nonoverlapping(data.as_ptr(), buffer as *mut u16, data.len());
    *length = bytes;
    1
}
// Return headers for synthetic/rewritten WinHTTP replies without consulting
// the original handle. Synthetic requests were never sent to WinHTTP and a
// fallback query would report incorrect handle state, leaving the GUI loading.
fn synthetic_status_text(status: u32) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Unknown",
    }
}
fn synthetic_header_value(
    query: u32,
    custom: Option<&str>,
    size: usize,
    status: u32,
    png: bool,
    zip: bool,
) -> Option<String> {
    let key = query & 0xffff;
    let mime = if png {
        "image/png"
    } else if zip {
        "application/zip"
    } else {
        "application/json"
    };
    match key {
        QUERY_CONTENT_LENGTH => Some(size.to_string()),
        QUERY_STATUS_CODE => Some(status.to_string()),
        QUERY_STATUS_TEXT => Some(synthetic_status_text(status).into()),
        QUERY_VERSION => Some("HTTP/1.1".into()),
        QUERY_CONTENT_TYPE => Some(mime.into()),
        QUERY_RAW_HEADERS_CRLF => Some(format!(
            "HTTP/1.1 {status} {}\r\nContent-Type: {mime}\r\nContent-Length: {size}\r\nCache-Control: no-store\r\n\r\n",
            synthetic_status_text(status)
        )),
        QUERY_RAW_HEADERS => Some(format!(
            "HTTP/1.1 {status} {}\0Content-Type: {mime}\0Content-Length: {size}\0Cache-Control: no-store\0\0",
            synthetic_status_text(status)
        )),
        QUERY_CUSTOM => match custom.unwrap_or_default().to_ascii_lowercase().as_str() {
            "content-type" => Some(mime.into()),
            "content-length" => Some(size.to_string()),
            "cache-control" => Some("no-store".into()),
            _ => None,
        },
        _ => None,
    }
}
unsafe extern "system" fn on_headers(
    h: H,
    query: u32,
    name: *const u16,
    buffer: H,
    length: *mut u32,
    index: *mut u32,
) -> B {
    let replay = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get(&state_key(h)).and_then(|request| {
            request
                .replay
                .as_ref()
                .filter(|rep| rep.replaced)
                .map(|rep| {
                    (
                        rep.head_content_length.unwrap_or(rep.bytes.len()),
                        rep.status,
                        rep.png,
                        rep.zip,
                        rep.catalog,
                        rep.strip_encoding,
                        request.synthetic,
                    )
                })
        })
    };
    if let Some((size, status, png, zip, _catalog, strip_encoding, synthetic)) = replay {
        let key = query & 0xffff;
        let custom = if key == QUERY_CUSTOM && !name.is_null() {
            Some(read_wide(name))
        } else {
            None
        };
        let value = synthetic_header_value(query, custom.as_deref(), size, status, png, zip);
        if let Some(value) = value {
            return synthetic_header(query, buffer, length, &value);
        }
        // Synthetic handles have no original HTTP response. Do not consult
        // WinHttpQueryHeaders on them even when the caller asks for a header
        // we do not emit. Fail with the correct 'header not found' status.
        if synthetic
            || matches!(key, QUERY_CONTENT_ENCODING | QUERY_TRANSFER_ENCODING)
                && (strip_encoding || png || zip)
        {
            SetLastError(ERROR_WINHTTP_HEADER_NOT_FOUND);
            return 0;
        }
    }
    original!(HEADERS, HeadersFn)(h, query, name, buffer, length, index)
}

unsafe extern "system" fn on_close(h: H) -> B {
    let close_debug = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get(&state_key(h)).and_then(|r| {
            if !r.intercept_enabled {
                return None;
            }
            let name = r.diagnostic_route.clone().unwrap_or_else(|| {
                if r.host.eq_ignore_ascii_case(persona::LOCAL_HOST) {
                    "local_asset".into()
                } else {
                    "target_unknown".into()
                }
            });
            let (kind, bytes, read, synthetic) = if let Some(rep) = r.replay.as_ref() {
                ("replay", rep.bytes.len(), rep.position, r.synthetic)
            } else if let Some(cap) = r.capture.as_ref() {
                ("async_capture", cap.bytes.len(), 0, false)
            } else {
                ("passthrough", 0, 0, false)
            };
            Some((r.id, name, kind, bytes, read, synthetic))
        })
    };
    if let Some((id, route, kind, bytes, read, synthetic)) = close_debug {
        logger(&format!(
            "request #{id}: HANDLE_CLOSE route={route} mode={kind} synthetic={synthetic} bytes={bytes} consumed_bytes={read} fully_consumed={}",
            kind == "replay" && bytes == read
        ));
    }
    let keep_capture = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        // Unbridged game callbacks do not route HANDLE_CLOSING through us.
        // Never retain disabled/unrelated marketplace request handles.
        state
            .requests
            .get(&state_key(h))
            .is_some_and(|r| r.intercept_enabled && r.bridge_installed)
    };
    if !keep_capture {
        let mut state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.remove(&state_key(h));
        state.connections.remove(&state_key(h));
        state.sessions.remove(&state_key(h));
        state.callbacks.remove(&state_key(h));
    }
    let gate = {
        let state = tables().lock().unwrap_or_else(|e| e.into_inner());
        state.requests.get(&state_key(h))
            .and_then(|req| req.capture.as_ref())
            .map(|capture| {
                capture.cancelled.store(true, Ordering::Release);
                Arc::clone(&capture.io_guard)
            })
    };
    if let Some(gate) = gate {
        let _guard = gate.lock().unwrap_or_else(|e| e.into_inner());
        original!(CLOSE, CloseFn)(h)
    } else {
        original!(CLOSE, CloseFn)(h)
    }
}

#[cfg(test)]
mod isolation_tests {
    use super::*;
    #[test]
    fn synthetic_headers_support_names_and_standard_status() {
        assert_eq!(
            synthetic_header_value(QUERY_CUSTOM, Some("Content-Type"), 5, 200, false, false),
            Some("application/json".into())
        );
        assert_eq!(
            synthetic_header_value(QUERY_CUSTOM, Some("Content-Length"), 5, 200, false, false),
            Some("5".into())
        );
        assert_eq!(
            synthetic_header_value(QUERY_VERSION, None, 5, 200, false, false),
            Some("HTTP/1.1".into())
        );
        assert_eq!(
            synthetic_header_value(QUERY_STATUS_TEXT, None, 5, 200, false, false),
            Some("OK".into())
        );
        assert_eq!(
            synthetic_header_value(QUERY_CUSTOM, Some("Authorization"), 5, 200, false, false),
            None
        );
    }
    #[test]
    fn persona_zip_head_returns_get_metadata_with_no_body() {
        let zipped = b"virtual zip data".to_vec();
        let head = virtual_asset_replay("HEAD", zipped.clone(), 200, false);
        assert!(head.bytes.is_empty(), "HEAD must not return a ZIP body");
        assert_eq!(head.head_content_length, Some(zipped.len()));
        assert_eq!(head.status, 200);
        assert!(head.zip);
        assert!(virtual_asset_method_allowed("HEAD"));
        assert!(virtual_asset_method_allowed("GET"));
        assert!(virtual_asset_method_allowed("head"));
        assert!(!virtual_asset_method_allowed("POST"));
        assert_eq!(
            synthetic_header_value(
                QUERY_CONTENT_LENGTH,
                None,
                head.head_content_length.unwrap(),
                head.status,
                head.png,
                head.zip
            ),
            Some(zipped.len().to_string())
        );
        assert_eq!(
            synthetic_header_value(
                QUERY_CONTENT_TYPE,
                None,
                head.head_content_length.unwrap(),
                head.status,
                head.png,
                head.zip
            ),
            Some("application/zip".into())
        );
        let get = virtual_asset_replay("GET", zipped.clone(), 200, false);
        assert_eq!(get.bytes, zipped);
        assert_eq!(get.head_content_length, None);
        assert_eq!(get.bytes.len(), head.head_content_length.unwrap());
        let thumbnail = virtual_asset_replay("HEAD", vec![1, 2, 3], 200, true);
        assert!(thumbnail.bytes.is_empty());
        assert!(thumbnail.png);
        assert_eq!(thumbnail.head_content_length, Some(3));
        assert_eq!(
            synthetic_header_value(QUERY_CONTENT_TYPE, None, 3, 200, true, false),
            Some("image/png".into())
        );
    }
    #[test]
    fn missing_resources_and_unsupported_methods_keep_correct_status_headers() {
        let missing = virtual_asset_replay("HEAD", Vec::new(), 404, false);
        assert_eq!(missing.head_content_length, Some(0));
        assert_eq!(
            synthetic_header_value(QUERY_STATUS_TEXT, None, 0, 404, false, true),
            Some("Not Found".into())
        );
        assert_eq!(
            synthetic_header_value(QUERY_STATUS_TEXT, None, 0, 405, false, true),
            Some("Method Not Allowed".into())
        );
        assert_eq!(
            synthetic_header_value(QUERY_RAW_HEADERS_CRLF, None, 0, 405, false, true)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
            "HTTP/1.1 405 Method Not Allowed"
        );
    }
    #[test]
    fn only_complete_known_local_catalog_requests_are_synthesized() {
        let id = "11111111-2222-4333-8444-555555555555";
        let request = format!(r#"{{"ItemId":"{id}"}}"#);
        let data = br#"{"data":{"Item":{"Id":"local"}}}"#.to_vec();
        let good =
            eligible_local_catalog(request.as_bytes(), request.len(), false, 4096, |found| {
                if found == id {
                    Some(data.clone())
                } else {
                    None
                }
            });
        assert_eq!(good, Some(data.clone()));
        assert!(
            eligible_local_catalog(
                request.as_bytes(),
                request.len() + 1,
                false,
                4096,
                |_| Some(data.clone())
            )
            .is_none()
        );
        assert!(
            eligible_local_catalog(request.as_bytes(), request.len(), true, 4096, |_| Some(
                data.clone()
            ))
            .is_none()
        );
        assert!(
            eligible_local_catalog(request.as_bytes(), request.len(), false, 2, |_| Some(
                data.clone()
            ))
            .is_none()
        );
        assert!(
            eligible_local_catalog(request.as_bytes(), request.len(), false, 4096, |_| None)
                .is_none()
        );
    }
    #[test]
    fn disabled_marketplace_routes_are_never_intercepted() {
        // BLoader 移植：should_intercept 额外受 config.json 总开关控制。
        MASTER_ENABLED.store(true, Ordering::SeqCst);
        let cfg: Config = serde_json::from_str(
            r#"{
            "rules":[
                {"name":"search","enabled":false,"host":"20ca2.playfabapi.com",
                 "path":"/Catalog/Search","method":"POST","mode":"append_array"},
                {"name":"capes","enabled":true,"host":"store.mktpl.minecraft-services.net",
                 "path":"/api/v2.0/layout/pages/DressingRoom_Capes",
                 "method":"POST","mode":"append_array"}
            ]}"#,
        )
        .unwrap();
        assert!(!should_intercept(
            &cfg,
            "20ca2.playfabapi.com",
            "/Catalog/Search",
            "POST"
        ));
        assert!(!should_intercept(
            &cfg,
            "20ca2.playfabapi.com",
            "/SomethingElse",
            "POST"
        ));
        assert!(should_intercept(
            &cfg,
            "store.mktpl.minecraft-services.net",
            "/api/v2.0/layout/pages/DressingRoom_Capes",
            "POST"
        ));
        assert!(should_intercept(
            &cfg,
            "cosmetics.invalid",
            "/personabinary/00000000-0000-4000-8000-000000000001.zip",
            "GET"
        ));
        let off: Config = serde_json::from_str(
            r#"{"enabled":false,"rules":[
            {"name":"capes","enabled":true,"host":"store.mktpl.minecraft-services.net",
             "path":"/api/v2.0/layout/pages/DressingRoom_Capes",
             "method":"POST","mode":"append_array"}]}"#,
        )
        .unwrap();
        assert!(!should_intercept(
            &off,
            "store.mktpl.minecraft-services.net",
            "/api/v2.0/layout/pages/DressingRoom_Capes",
            "POST"
        ));
        assert!(!should_intercept(
            &off,
            "cosmetics.invalid",
            "/personabinary/test.zip",
            "GET"
        ));
        MASTER_ENABLED.store(false, Ordering::SeqCst);
        assert!(
            !should_intercept(
                &cfg,
                "store.mktpl.minecraft-services.net",
                "/api/v2.0/layout/pages/DressingRoom_Capes",
                "POST"
            ),
            "config.json enable_cosmetics=false must stop every interception"
        );
    }
}

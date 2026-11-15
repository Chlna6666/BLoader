//! Asynchronous, bounded community cape distribution from the BMCBL CDN.
//! Network requests never run in a game WinHTTP callback or the loader's OEP path.
//! Only HTTPS, the fixed CDN host, and content-addressed /pkgs/ URLs are accepted.
use super::local_capes::{AssetType, Registry};
use parking_lot::{Mutex, RwLock};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const CDN_HOST: &str = "be-cape-share.roundstudio.top";
const USER_AGENT: &str = "BMCBL/1.0.0";
const INDEX_LIMIT: usize = 512 * 1024;
const PACK_LIMIT: usize = 64 * 1024 * 1024;
const TOTAL_PACK_LIMIT: usize = 128 * 1024 * 1024;
const MAX_PACKS: usize = 128;
const MAX_CAPES: usize = 512;
const DOWNLOAD_WORKERS: usize = 2;

struct Runtime {
    cache: PathBuf,
    locale: String,
}
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static REGISTRY: OnceLock<RwLock<Arc<Registry>>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

fn state() -> &'static RwLock<Arc<Registry>> {
    REGISTRY.get_or_init(|| RwLock::new(Arc::new(Registry::default())))
}

pub fn start(cache: PathBuf, locale: String, enabled: bool) {
    let _ = RUNTIME.set(Runtime { cache, locale });
    // Config::apply_update runs before install(). It may already have marked
    // the switch enabled, so a transition-only setter cannot start this worker.
    ENABLED.store(enabled, Ordering::Release);
    if enabled {
        refresh_in_background();
    }
}

pub fn set_enabled(enabled: bool) {
    let previous = ENABLED.swap(enabled, Ordering::AcqRel);
    if enabled && !previous && RUNTIME.get().is_some() {
        refresh_in_background();
    }
}

pub fn snapshot() -> Option<Arc<Registry>> {
    ENABLED.load(Ordering::Acquire).then(|| Arc::clone(&state().read()))
}

pub fn groups() -> Vec<(String, Vec<Value>)> {
    snapshot().map_or_else(Vec::new, |registry| registry.groups())
}

pub fn catalog(id: &str) -> Option<Vec<u8>> {
    snapshot()?.catalog_bytes(id).map(ToOwned::to_owned)
}

pub fn asset(path: &str) -> Option<(Vec<u8>, AssetType)> {
    let registry = snapshot()?;
    let (bytes, kind) = registry.asset_bytes(path)?;
    Some((bytes.to_vec(), kind))
}

fn publish(cache: &Path, locale: &str) {
    let registry = Registry::scan(&[cache.to_path_buf()], locale);
    let packs = registry.accepted;
    let rejected = registry.rejected;
    let items = registry.item_count();
    *state().write() = Arc::new(registry);
    crate::platform::logging::scoped_info_message(
        "cosmetics",
        &format!("community capes cache loaded packs={packs} rejected={rejected} items={items}"),
    );
}

fn refresh_in_background() {
    if WORKER_ACTIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return;
    }
    if std::thread::Builder::new()
        .name("bloader-community-capes".into())
        .spawn(|| {
            if let Some(runtime) = RUNTIME.get() {
                // Warm start from disk; HTTPS is never on the UI or game thread.
                if runtime.cache.join("capes").is_dir() {
                    publish(&runtime.cache, &runtime.locale);
                }
                if ENABLED.load(Ordering::Acquire) {
                    match synchronize(&runtime.cache) {
                        Ok(changed) => {
                            if changed && ENABLED.load(Ordering::Acquire) {
                                publish(&runtime.cache, &runtime.locale);
                            }
                        }
                        Err(error) => crate::platform::logging::scoped_warn_message(
                            "cosmetics",
                            &format!("community capes sync unavailable; cached capes retained: {error}"),
                        ),
                    }
                }
            }
            WORKER_ACTIVE.store(false, Ordering::Release);
        })
        .is_err()
    {
        WORKER_ACTIVE.store(false, Ordering::Release);
    }
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RemotePack {
    id: String,
    uuid: String,
    hash: String,
    name: String,
    cape_count: usize,
    url: String,
}
#[derive(Deserialize)]
struct RemoteIndex {
    capes: Vec<RemotePack>,
}
#[derive(Deserialize)]
struct VersionPointer {
    index: String,
}

fn verified_index(bytes: &[u8]) -> Result<Vec<RemotePack>, String> {
    let index: RemoteIndex = serde_json::from_slice(bytes)
        .map_err(|_| "invalid CDN cape index JSON")?;
    if index.capes.len() > MAX_PACKS {
        return Err("community index exceeds package limit".into());
    }
    let mut ids = HashSet::new();
    let mut uuids = HashSet::new();
    let mut total = 0usize;
    for entry in &index.capes {
        let stable = !entry.id.is_empty()
            && entry.id.len() <= 64
            && entry.id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
        let uuid = entry.uuid.len() == 36 && entry.uuid.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() }
        });
        let hash = entry.hash.len() == 64 && entry.hash.bytes().all(|b| b.is_ascii_hexdigit());
        if !stable || !uuid || !hash || entry.name.trim().is_empty() || entry.name.len() > 90
            || entry.name.chars().any(char::is_control)
            || entry.cape_count == 0 || entry.cape_count > 64
            || !ids.insert(entry.id.clone()) || !uuids.insert(entry.uuid.to_ascii_lowercase())
        {
            return Err("invalid or duplicate community package metadata".into());
        }
        // Never permit a CDN index to select another host, a traversal path,
        // a query string, or a different download path.
        let expected = format!("/pkgs/{}.{}.mccapes", entry.id, &entry.hash[..8]);
        if entry.url != expected {
            return Err("unsafe community package URL".into());
        }
        total += entry.cape_count;
        if total > MAX_CAPES {
            return Err("community index exceeds cape limit".into());
        }
    }
    Ok(index.capes)
}

fn version_path(path: &str) -> bool {
    path.strip_prefix("/v").and_then(|rest| rest.strip_suffix("/index.json"))
        .is_some_and(|version| !version.is_empty()
            && version.len() <= 10
            && version.bytes().all(|b| b.is_ascii_digit()))
}

fn load_remote_index() -> Result<Vec<RemotePack>, String> {
    let path = match https_get("/index.json", 4096) {
        Ok(bytes) => serde_json::from_slice::<VersionPointer>(&bytes).ok()
            .map(|pointer| pointer.index)
            .filter(|path| version_path(path))
            .unwrap_or_else(|| "/v1/index.json".into()),
        Err(_) => "/v1/index.json".into(),
    };
    verified_index(&https_get(&path, INDEX_LIMIT)?)
}

fn cache_file(root: &Path, entry: &RemotePack) -> PathBuf {
    root.join("capes").join(format!("{}.{}.mccapes", entry.id, &entry.hash[..8]))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn verify_cached(path: &Path, hash: &str) -> bool {
    fs::metadata(path).is_ok_and(|info| info.is_file() && info.len() <= PACK_LIMIT as u64)
        && fs::read(path).is_ok_and(|bytes| sha256_hex(&bytes).eq_ignore_ascii_case(hash))
}

fn synchronize(cache: &Path) -> Result<bool, String> {
    let entries = load_remote_index()?;
    if !ENABLED.load(Ordering::Acquire) {
        return Ok(false);
    }
    let folder = cache.join("capes");
    fs::create_dir_all(&folder).map_err(|e| format!("cache create failed: {e}"))?;
    let mut pending = VecDeque::new();
    let mut referenced = HashSet::new();
    let mut bytes_on_disk = 0usize;
    for entry in &entries {
        let path = cache_file(cache, entry);
        let filename = path.file_name().and_then(|s| s.to_str())
            .ok_or("invalid cache filename")?.to_owned();
        referenced.insert(filename);
        if verify_cached(&path, &entry.hash) {
            bytes_on_disk = bytes_on_disk.saturating_add(fs::metadata(&path).map_or(0, |m| m.len() as usize));
        } else {
            pending.push_back(entry.clone());
        }
    }
    if bytes_on_disk > TOTAL_PACK_LIMIT {
        return Err("community cached assets exceed size limit".into());
    }
    let changed = !pending.is_empty();
    let queue = Mutex::new(pending);
    let failures = AtomicUsize::new(0);
    let downloaded = AtomicUsize::new(0);
    // Enforce the aggregate cache budget before writing each downloaded ZIP,
    // not only after all downloads have filled the disk.
    let total_bytes = AtomicUsize::new(bytes_on_disk);
    std::thread::scope(|scope| {
        for _ in 0..DOWNLOAD_WORKERS {
            let queue = &queue;
            let failures = &failures;
            let downloaded = &downloaded;
            let total_bytes = &total_bytes;
            let folder = &folder;
            scope.spawn(move || loop {
                if !ENABLED.load(Ordering::Acquire) { break; }
                let Some(entry) = queue.lock().pop_front() else { break };
                let result = (|| -> Result<(), String> {
                    let bytes = https_get(&entry.url, PACK_LIMIT)?;
                    if !sha256_hex(&bytes).eq_ignore_ascii_case(&entry.hash) {
                        return Err("download SHA-256 mismatch".into());
                    }
                    if total_bytes.fetch_update(Ordering::AcqRel, Ordering::Acquire,
                        |previous| previous.checked_add(bytes.len())
                            .filter(|sum| *sum <= TOTAL_PACK_LIMIT)).is_err()
                    {
                        return Err("community cache aggregate size limit exceeded".into());
                    }
                    let destination = folder.join(format!("{}.{}.mccapes", entry.id, &entry.hash[..8]));
                    let staging = destination.with_extension("downloading");
                    fs::write(&staging, bytes).map_err(|e| e.to_string())?;
                    if destination.exists() {
                        fs::remove_file(&destination).map_err(|e| e.to_string())?;
                    }
                    fs::rename(&staging, destination).map_err(|e| e.to_string())?;
                    Ok(())
                })();
                if result.is_ok() {
                    downloaded.fetch_add(1, Ordering::Relaxed);
                } else {
                    failures.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    let failed = failures.load(Ordering::Acquire);
    // Preserve the last successfully verified package set when a request fails.
    if failed != 0 || !ENABLED.load(Ordering::Acquire) {
        return Err(format!("{failed} community packages failed to download"));
    }
    // Commit a new index only after the complete content-addressed set is present.
    let mut total_bytes = 0u64;
    for entry in &entries {
        let path = cache_file(cache, entry);
        let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
        total_bytes = total_bytes.saturating_add(size);
        if total_bytes > TOTAL_PACK_LIMIT as u64 {
            return Err("community packages exceed cache size limit".into());
        }
    }
    let mut removed = 0usize;
    if let Ok(files) = fs::read_dir(&folder) {
        for file in files.flatten() {
            let name = file.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".mccapes") && !referenced.contains(name.as_ref()) {
                if fs::remove_file(file.path()).is_ok() { removed += 1; }
            }
        }
    }
    crate::platform::logging::scoped_info_message("cosmetics",
        &format!("community capes sync packs={} downloaded={} removed={removed}", entries.len(), downloaded.load(Ordering::Acquire)));
    Ok(changed || removed != 0)
}

// WinHTTP is used directly to avoid a second TLS stack and keeps the fixed
// User-Agent compatible with the CDN. Hooked native game requests are passed
// through because this host is not a Cosmetics interception target.
type H = *mut c_void;
#[link(name = "winhttp")]
unsafe extern "system" {
    fn WinHttpOpen(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> H;
    fn WinHttpConnect(session: H, server: *const u16, port: u16, reserved: u32) -> H;
    fn WinHttpOpenRequest(connection: H, method: *const u16, path: *const u16,
        version: *const u16, referrer: *const u16, accept: *const *const u16, flags: u32) -> H;
    fn WinHttpSetTimeouts(handle: H, resolve: i32, connect: i32, send: i32, receive: i32) -> i32;
    fn WinHttpSetOption(handle: H, option: u32, buffer: *const c_void, size: u32) -> i32;
    fn WinHttpSendRequest(handle: H, headers: *const u16, header_length: u32,
        optional: H, optional_length: u32, total_length: u32, context: usize) -> i32;
    fn WinHttpReceiveResponse(handle: H, reserved: H) -> i32;
    fn WinHttpQueryHeaders(handle: H, info_level: u32, header: *const u16,
        buffer: H, size: *mut u32, index: *mut u32) -> i32;
    fn WinHttpReadData(handle: H, buffer: H, size: u32, read: *mut u32) -> i32;
    fn WinHttpCloseHandle(handle: H) -> i32;
}
struct Handle(H);
impl Handle {
    fn new(handle: H) -> Result<Self, String> {
        if handle.is_null() { Err("WinHTTP handle unavailable".into()) } else { Ok(Self(handle)) }
    }
}
impl Drop for Handle {
    fn drop(&mut self) { unsafe { WinHttpCloseHandle(self.0); } }
}
fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

fn https_get(path: &str, limit: usize) -> Result<Vec<u8>, String> {
    if path.len() > 192 || !path.starts_with('/') || path.contains("..")
        || path.contains('?') || path.contains('#') || path.contains('\\')
    {
        return Err("unsafe community CDN path".into());
    }
    // All handles are synchronous, used only by the background fetch worker.
    unsafe {
        let session = Handle::new(WinHttpOpen(wide(USER_AGENT).as_ptr(), 0,
            ptr::null(), ptr::null(), 0))?;
        if WinHttpSetTimeouts(session.0, 2500, 4000, 4000, 5000) == 0 {
            return Err("WinHTTP timeout configuration failed".into());
        }
        let connection = Handle::new(WinHttpConnect(session.0, wide(CDN_HOST).as_ptr(), 443, 0))?;
        let request = Handle::new(WinHttpOpenRequest(connection.0, wide("GET").as_ptr(),
            wide(path).as_ptr(), ptr::null(), ptr::null(), ptr::null(), 0x0080_0000))?;
        // WINHTTP_OPTION_DISABLE_FEATURE / WINHTTP_DISABLE_REDIRECTS:
        // do not allow redirects to evade the pinned host and path restrictions.
        let disable_redirects: u32 = 2;
        if WinHttpSetOption(request.0, 63, &disable_redirects as *const u32 as *const c_void, 4) == 0 {
            return Err("WinHTTP redirect policy failed".into());
        }
        if WinHttpSendRequest(request.0, ptr::null(), 0, ptr::null_mut(), 0, 0, 0) == 0
            || WinHttpReceiveResponse(request.0, ptr::null_mut()) == 0
        {
            return Err("community CDN request timed out or failed".into());
        }
        let mut status = 0u32;
        let mut size = 4u32;
        if WinHttpQueryHeaders(request.0, 19 | 0x2000_0000, ptr::null(),
            &mut status as *mut u32 as H, &mut size, ptr::null_mut()) == 0 || status != 200
        {
            return Err(format!("community CDN status {status}"));
        }
        let started = Instant::now();
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            if started.elapsed() > Duration::from_secs(12) {
                return Err("community CDN response deadline exceeded".into());
            }
            let mut count = 0u32;
            if WinHttpReadData(request.0, chunk.as_mut_ptr() as H, chunk.len() as u32, &mut count) == 0 {
                return Err("community CDN response read failed".into());
            }
            if count == 0 { break; }
            if count as usize > chunk.len() || bytes.len() > limit.saturating_sub(count as usize) {
                return Err("community CDN response size exceeded".into());
            }
            bytes.extend_from_slice(&chunk[..count as usize]);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn community_index_rejects_untrusted_paths_and_duplicate_keys() {
        let base = r#"{"capes":[{"id":"sample","uuid":"11111111-2222-4333-8444-555555555555","hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"Sample","capeCount":1,"url":"/pkgs/sample.aaaaaaaa.mccapes"}]}"#;
        assert_eq!(verified_index(base.as_bytes()).unwrap().len(), 1);
        assert!(verified_index(base.replace("/pkgs/sample.aaaaaaaa.mccapes","https://other.test/a").as_bytes()).is_err());
        assert!(verified_index(base.replace("\"id\":\"sample\"","\"id\":\"../bad\"").as_bytes()).is_err());
    }
    #[test]
    fn community_pointer_rejects_arbitrary_paths() {
        assert!(version_path("/v1/index.json"));
        assert!(version_path("/v123/index.json"));
        assert!(!version_path("https://evil.test/index.json"));
        assert!(!version_path("/v../../index.json"));
    }
}

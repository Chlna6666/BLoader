//! Verified native DLL loading and optional BLoaderModEntryV1 registration.
//! Native modules execute with full Minecraft-process privileges, not WASM isolation.
use std::collections::HashSet;
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
};
use windows::core::{PCSTR, PCWSTR};

use super::protocol::{self, ModuleSpec};
use super::Session;
use crate::platform::utils;

const FILE_SHARE_READ_ONLY: u32 = 0x0000_0001;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const MAX_BODY: usize = 32 * 1024;
const MAX_INFLIGHT_PER_MOD: usize = 32;
const QUEUE_SIZE: usize = 64;

#[repr(C)]
pub struct BLoaderHostV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub host_context: *mut c_void,
    pub reply: Option<unsafe extern "C" fn(*mut c_void, u64, *const u8, u32) -> i32>,
    pub emit: Option<unsafe extern "C" fn(*mut c_void, *const u8, u32, *const u8, u32) -> i32>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BLoaderModV1 {
    pub struct_size: u32,
    pub mod_context: *mut c_void,
    pub on_request: Option<unsafe extern "C" fn(*mut c_void, u64, *const u8, u32, *const u8, u32) -> i32>,
    pub on_close: Option<unsafe extern "C" fn(*mut c_void)>,
}

type EntryFn = unsafe extern "C" fn(*const BLoaderHostV1, *mut BLoaderModV1) -> i32;

struct HostContext {
    session: Weak<Session>,
    module: String,
    generation: String,
    pending: Arc<Mutex<HashSet<u64>>>,
}

/// No arbitrary FreeLibrary: loaded Mods and ABI callback pointers are process-lifetime resources.
pub enum Loaded {
    NoRpc,
    Rpc(Arc<NativeModule>),
}

pub struct NativeModule {
    pub generation: String,
    pub pending: Arc<Mutex<HashSet<u64>>>,
    queue: mpsc::SyncSender<Work>,
}

struct Work {
    request: u64,
    method: String,
    body: Vec<u8>,
}

impl NativeModule {
    pub fn enqueue(&self, request: u64, method: String, body: Vec<u8>) -> Result<(), &'static str> {
        let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
        if pending.len() >= MAX_INFLIGHT_PER_MOD || !pending.insert(request) {
            return Err("BUSY");
        }
        match self.queue.try_send(Work { request, method, body }) {
            Ok(()) => Ok(()),
            Err(_) => {
                pending.remove(&request);
                Err("BUSY")
            }
        }
    }

    pub fn cancel(&self, request: u64) -> bool {
        self.pending.lock().unwrap_or_else(|error| error.into_inner()).remove(&request)
    }
}

pub fn load(spec: &ModuleSpec, plugins_root: &Path, session: &Arc<Session>) -> Result<Loaded, &'static str> {
    // Keep a read-only handle open until LoadLibraryExW finishes. No duplicated DLL or staging tree.
    let (path, _file_lock) = verify_artifact(spec, plugins_root)?;
    let leaf = path.file_name().ok_or("FILE_UNSAFE")?;
    let leaf_wide = wide(leaf);
    // Refuse ambiguous base-name collisions, including DLLs loaded by another Mod.
    if let Ok(existing) = unsafe { GetModuleHandleW(PCWSTR(leaf_wide.as_ptr())) } {
        let existing_path = utils::get_module_path(existing.0 as usize);
        if !existing_path.as_os_str().is_empty() {
            return Err("DEPENDENCY_UNSAFE");
        }
    }
    let requested = wide(path.as_os_str());
    let handle = unsafe {
        LoadLibraryExW(PCWSTR(requested.as_ptr()), None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)
    }.map_err(|_| "LOAD_FAILED")?;
    let resolved = utils::get_module_path(handle.0 as usize);
    if resolved.as_os_str().is_empty() ||
        !paths_equal(&resolved, &path)
    {
        return Err("FILE_UNSAFE");
    }
    let Some(export) = (unsafe { GetProcAddress(handle, PCSTR(b"BLoaderModEntryV1\0".as_ptr())) }) else {
        return Ok(Loaded::NoRpc);
    };
    // SAFETY: the export is explicitly named and the entry ABI is specified by v1.
    let entry: EntryFn = unsafe { std::mem::transmute(export) };
    let pending = Arc::new(Mutex::new(HashSet::new()));
    let context = Box::new(HostContext {
        session: Arc::downgrade(session),
        module: spec.identifier(),
        generation: spec.plugin_generation.clone(),
        pending: pending.clone(),
    });
    // Callbacks can outlive the module entry call; keep the context and host vtable
    // stable for the remaining game process lifetime.
    let context = Box::into_raw(context);
    let host = Box::leak(Box::new(BLoaderHostV1 {
        struct_size: std::mem::size_of::<BLoaderHostV1>() as u32,
        abi_version: 1,
        host_context: context.cast(),
        reply: Some(native_reply),
        emit: Some(native_emit),
    }));
    let mut module = BLoaderModV1 {
        struct_size: std::mem::size_of::<BLoaderModV1>() as u32,
        mod_context: std::ptr::null_mut(),
        on_request: None,
        on_close: None,
    };
    let status = unsafe { entry(host as *const _, &mut module) };
    if status != 0 || module.struct_size < std::mem::size_of::<BLoaderModV1>() as u32
        || module.on_request.is_none() {
        return Err("ABI_UNSUPPORTED");
    }
    let Some(callback) = module.on_request else { return Err("ABI_UNSUPPORTED") };
    let context_addr = module.mod_context as usize;
    let (sender, receiver) = mpsc::sync_channel::<Work>(QUEUE_SIZE);
    let weak = Arc::downgrade(session);
    let id = spec.identifier();
    let generation = spec.plugin_generation.clone();
    let pending_for_worker = pending.clone();
    thread::Builder::new()
        .name("bloader-virtual-mod-rpc".to_string())
        .spawn(move || {
            for work in receiver {
                // Calls into untrusted code never run on the transport reader or
                // under a global session lock. A blocked module only blocks itself.
                if !pending_for_worker.lock()
                    .unwrap_or_else(|error| error.into_inner()).contains(&work.request) {
                    continue;
                }
                let result = unsafe {
                    callback(
                        context_addr as *mut c_void,
                        work.request,
                        work.method.as_ptr(),
                        work.method.len() as u32,
                        work.body.as_ptr(),
                        work.body.len() as u32,
                    )
                };
                if result != 0 {
                    if pending_for_worker.lock()
                        .unwrap_or_else(|error| error.into_inner()).remove(&work.request) {
                        if let Some(session) = weak.upgrade() {
                            session.send_response(&id, &generation, work.request, None, Some("MODULE_UNAVAILABLE"));
                            session.finish_inflight();
                        }
                    }
                }
            }
        }).map_err(|_| "INTERNAL_ERROR")?;
    Ok(Loaded::Rpc(Arc::new(NativeModule {
        generation: spec.plugin_generation.clone(),
        pending,
        queue: sender,
    })))
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    let a = fs::canonicalize(a).ok();
    let b = fs::canonicalize(b).ok();
    a.zip(b).is_some_and(|(left, right)|
        left.to_string_lossy().eq_ignore_ascii_case(&right.to_string_lossy()))
}

/// Load the DLL from the installed plugin root:
/// <BMCBL>/plugins/<plugin_id>/game-mods/<relative DLL>.
/// `plugins_root` is supplied by the authenticated launcher, never by a Mod.
/// Open without FILE_SHARE_WRITE or FILE_SHARE_DELETE, keep that exact file handle
/// until LoadLibraryExW returns. No additional install/staging/copy occurs here.
fn verify_artifact(
    spec: &ModuleSpec,
    plugins_root: &Path,
) -> Result<(PathBuf, File), &'static str> {
    if !spec.valid() || !protocol::valid_mod_relative_path(&spec.relative_path) {
        return Err("INVALID_PLAN");
    }
    if !plugins_root.is_absolute() || plugins_root.file_name()
        .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("plugins"))
    {
        return Err("FILE_UNSAFE");
    }

    // All managed module paths must stay under the exact directory assigned to
    // their plugin identity. The authenticated BMCBL host provides this root;
    // a plugin cannot substitute an absolute DLL path in the launch plan.
    let plugin_root = plugins_root.join(&spec.plugin_id);
    let module_path = plugin_root.join(&spec.relative_path);
    if !module_path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("dll")) {
        return Err("FILE_UNSAFE");
    }

    // Reject NTFS junctions and reparse points throughout the plugin subtree.
    // The Wine drive mapping may itself use a symlink, so do not scan parents
    // *above* plugins_root; canonicalize the complete root before comparison.
    reject_reparse_point(plugins_root)?;
    reject_reparse_point(&plugin_root)?;
    let mut checked = plugin_root.clone();
    for part in spec.relative_path.split('/') {
        checked.push(part);
        reject_reparse_point(&checked)?;
    }

    let root = fs::canonicalize(plugins_root).map_err(|_| "FILE_UNSAFE")?;
    let plugin = fs::canonicalize(&plugin_root).map_err(|_| "FILE_UNSAFE")?;
    let path = fs::canonicalize(&module_path).map_err(|_| "FILE_UNSAFE")?;
    if !same_or_child(&plugin, &root.join(&spec.plugin_id)) ||
        !same_or_child(&path, &plugin.join("game-mods")) ||
        path == plugin.join("game-mods")
    {
        return Err("FILE_UNSAFE");
    }

    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ_ONLY)
        .open(&path)
        .map_err(|_| "FILE_UNSAFE")?;
    if file.metadata().map_err(|_| "FILE_UNSAFE")?.len() != spec.size_bytes {
        return Err("HASH_MISMATCH");
    }
    let mut bytes = Vec::with_capacity(spec.size_bytes as usize);
    file.by_ref().take(protocol::MAX_MODULE_BYTES + 1)
        .read_to_end(&mut bytes).map_err(|_| "FILE_UNSAFE")?;
    if bytes.len() as u64 != spec.size_bytes || !pe_x64_dll(&bytes) {
        return Err("FILE_UNSAFE");
    }
    let digest = Sha256::digest(&bytes);
    let observed = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    if observed != spec.sha256 {
        return Err("HASH_MISMATCH");
    }
    Ok((path, file))
}

fn reject_reparse_point(path: &Path) -> Result<(), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "FILE_UNSAFE")?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("FILE_UNSAFE");
    }
    Ok(())
}

fn same_or_child(candidate: &Path, root: &Path) -> bool {
    // Windows paths are normally case-insensitive. Compare components, not
    // string prefixes (C:\plugins\id must not match C:\plugins\id-other).
    let lhs = candidate.components().collect::<Vec<_>>();
    let rhs = root.components().collect::<Vec<_>>();
    lhs.len() >= rhs.len() && lhs.iter().zip(rhs.iter()).all(|(left, right)| {
        left.as_os_str().to_string_lossy().to_lowercase()
            == right.as_os_str().to_string_lossy().to_lowercase()
    })
}

fn pe_x64_dll(bytes: &[u8]) -> bool {
    if bytes.get(..2) != Some(b"MZ".as_slice()) || bytes.len() < 64 { return false; }
    let Some(offset) = bytes.get(60..64).and_then(|slice| slice.try_into().ok())
        .map(u32::from_le_bytes).map(|offset| offset as usize) else { return false };
    let Some(header) = bytes.get(offset..offset.saturating_add(24)) else { return false };
    header.starts_with(b"PE\0\0\x64\x86") && (header[23] & 0x20) != 0
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

unsafe extern "C" fn native_reply(
    context: *mut c_void, request: u64, bytes: *const u8, len: u32,
) -> i32 {
    if context.is_null() || len as usize > MAX_BODY || (len != 0 && bytes.is_null()) {
        return -1;
    }
    let context = unsafe { &*context.cast::<HostContext>() };
    if !context.pending.lock().unwrap_or_else(|error| error.into_inner()).remove(&request) {
        return -2;
    }
    let Some(session) = context.session.upgrade() else { return -3 };
    // SAFETY: the module promises its borrowed input is valid until this call returns.
    let body = if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(bytes, len as usize) } };
    session.send_response(&context.module, &context.generation, request, Some(body), None);
    session.finish_inflight();
    0
}

unsafe extern "C" fn native_emit(
    context: *mut c_void, name: *const u8, name_len: u32, bytes: *const u8, len: u32,
) -> i32 {
    if context.is_null() || name.is_null() || name_len == 0 || name_len > 128
        || len as usize > MAX_BODY || (len > 0 && bytes.is_null())
    { return -1; }
    let context = unsafe { &*context.cast::<HostContext>() };
    let name = unsafe { std::slice::from_raw_parts(name, name_len as usize) };
    let Ok(name) = std::str::from_utf8(name) else { return -1 };
    if !protocol::valid_method(name) { return -1; }
    let Some(session) = context.session.upgrade() else { return -3 };
    let body = if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(bytes, len as usize) } };
    session.send_event(&context.module, &context.generation, name, &STANDARD.encode(body));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn component_boundary_blocks_sibling_plugins() {
        let root = Path::new(r"C:/BMCBL/plugins/plugin-a");
        assert!(same_or_child(Path::new(r"C:/BMCBL/plugins/plugin-a/game-mods/example.dll"), root));
        assert!(!same_or_child(Path::new(r"C:/BMCBL/plugins/plugin-a-evil/example.dll"), root));
        assert!(!same_or_child(Path::new(r"C:/BMCBL/plugins/plugin-b/example.dll"), root));
    }

    #[test]
    fn rejects_non_pe_payloads_and_incomplete_headers() {
        assert!(!pe_x64_dll(b"MZ"));
        assert!(!pe_x64_dll(&[0u8; 128]));
    }
}

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LoadLibraryA, LoadLibraryW,
};
use windows::core::{PCSTR, PCWSTR};

use crate::bl;
use crate::core::runtime_ready::{self, ReadyLevel};
use crate::runtime::foundation::{crash_report, logging, mod_diagnostics, native_stdio};

#[derive(Serialize, Deserialize)]
struct ModManifest {
    #[serde(default)]
    id: Option<String>,
    name: String,
    entry: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    mod_type: String,
    #[serde(default)]
    api_version: Option<u32>,
    #[serde(default)]
    inject_delay_ms: Option<u64>,
    /// Deprecated compatibility field. BLoader 0.2.19+ no longer counts
    /// graphics frames or installs a Present hook for delayed Mod loading.
    #[serde(default)]
    inject_min_frames: Option<usize>,
    /// Runtime readiness required before a hot Mod may load.
    /// Supported values: process, window, stable-window.
    #[serde(default)]
    inject_ready: Option<String>,
    #[serde(default)]
    requires_symbol_pack: bool,
    #[serde(default)]
    required_symbols: Vec<String>,
    /// 该原生模块加载失败时是否必须向开发者弹出错误提示。
    #[serde(default)]
    required: bool,
    /// LoadLibrary 成功后必须存在的导出。
    #[serde(default)]
    verify_exports: Vec<String>,
    /// LoadLibrary 成功后必须已经存在于进程中的依赖模块。
    #[serde(default)]
    verify_modules: Vec<String>,
    /// 验证成功后是否显示显式成功提示。
    #[serde(default)]
    notify_success: bool,
    /// 用于自动识别 puts/printf/std::cout 输出来源的额外前缀。
    #[serde(default)]
    log_aliases: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestBundle {
    manifest: ModManifest,
}

#[derive(Clone)]
struct PreloadMod {
    id: String,
    name: String,
    version: Option<String>,
    kind: String,
    dll_path: PathBuf,
    log_aliases: Vec<String>,
    required: bool,
    verify_exports: Vec<String>,
    verify_modules: Vec<String>,
    notify_success: bool,
}

#[derive(Clone)]
struct HotMod {
    id: String,
    name: String,
    version: Option<String>,
    kind: String,
    dll_path: PathBuf,
    log_aliases: Vec<String>,
    inject_delay_ms: u64,
    ready_level: ReadyLevel,
    required: bool,
    verify_exports: Vec<String>,
    verify_modules: Vec<String>,
    notify_success: bool,
}

#[derive(Clone)]
struct BlMod {
    id: String,
    name: String,
    dll_path: PathBuf,
    api_version: u32,
    version: Option<String>,
    author: Option<String>,
    description: Option<String>,
    log_aliases: Vec<String>,
    requires_symbol_pack: bool,
    required_symbols: Vec<String>,
}

#[derive(Default)]
struct DiscoveredMods {
    preload: Vec<PreloadMod>,
    hot: Vec<HotMod>,
    bl: Vec<BlMod>,
}

const MOD_TYPE_PRELOAD_NATIVE: &str = "preload-native";
const MOD_TYPE_HOT_NATIVE: &str = "hot-native";
const MOD_TYPE_HOT_INJECT: &str = "hot-inject";
const MOD_TYPE_BL: &str = "BL";

const HOT_INJECT_DEFAULT_DELAY_MS: u64 = 15_000;
const HOT_INJECT_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

static LOADED_DLL_KEYS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static NATIVE_LOAD_REPORTS: OnceLock<Mutex<Vec<NativeLoadReport>>> = OnceLock::new();
static NATIVE_PRELOAD_SUMMARY: OnceLock<NativePreloadSummary> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct NativePreloadSummary {
    pub discovered: usize,
    pub attempted: usize,
    pub verified: usize,
    pub failed: usize,
    pub required_failed: usize,
    pub success_notifications: usize,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum NativeLoadState {
    Verified,
    Failed,
    AlreadyVerified,
}

#[derive(Clone, Debug, Serialize)]
struct NativeLoadReport {
    name: String,
    phase: String,
    required: bool,
    notify_success: bool,
    expected_path: String,
    resolved_path: Option<String>,
    loader_api: Option<String>,
    module_handle: Option<String>,
    state: NativeLoadState,
    stage: String,
    detail: String,
    verified_exports: Vec<String>,
    verified_modules: Vec<String>,
    elapsed_ms: u128,
}

/// 在 BLoader 自身的 `DllMain(DLL_PROCESS_ATTACH)` 返回之前，同步加载原生预加载包。
///
/// 该路径刻意只处理打包目录中的 `native` / `preload-native`，不处理 BL Mod、
/// hot-native、hot-inject 或散落 DLL，避免在 Loader Lock 内执行自动打包、延迟线程
/// 和宿主 API 初始化。系统 Runtime DLL 由宿主和 Windows 自己管理，BLoader 不加载。
///
/// 注意：Windows 官方不建议在 DllMain 中调用 LoadLibrary。这里是为兼容
/// PreLoadCpp 的既有预加载语义而提供的受限路径。
pub unsafe fn load_native_preloads_in_dllmain(game_dir: &Path) -> NativePreloadSummary {
    let mods_dir = game_dir.join("mods");
    let mut summary = NativePreloadSummary::default();

    let Ok(entries) = fs::read_dir(&mods_dir) else {
        logging::write_bootstrap_marker(&format!(
            "native-load discovery_failed phase=dllmain path={} reason=mods_dir_unavailable",
            mods_dir.display()
        ));
        let _ = NATIVE_PRELOAD_SUMMARY.set(summary);
        return summary;
    };

    let mut native_preloads = Vec::new();
    for entry in entries.flatten() {
        let package_dir = entry.path();
        if !package_dir.is_dir() {
            continue;
        }

        let manifest_path = package_dir.join("manifest.json");
        if !manifest_path.exists() {
            continue;
        }

        let text = match fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(error) => {
                record_native_report(NativeLoadReport {
                    name: package_display_name(&package_dir),
                    phase: "dllmain_preload".to_string(),
                    required: false,
                    notify_success: false,
                    expected_path: manifest_path.display().to_string(),
                    resolved_path: None,
                    loader_api: None,
                    module_handle: None,
                    state: NativeLoadState::Failed,
                    stage: "manifest_read".to_string(),
                    detail: error.to_string(),
                    verified_exports: Vec::new(),
                    verified_modules: Vec::new(),
                    elapsed_ms: 0,
                });
                summary.failed += 1;
                continue;
            }
        };

        let manifest = match parse_manifest_json(&text) {
            Ok(manifest) => manifest,
            Err(error) => {
                record_native_report(NativeLoadReport {
                    name: package_display_name(&package_dir),
                    phase: "dllmain_preload".to_string(),
                    required: false,
                    notify_success: false,
                    expected_path: manifest_path.display().to_string(),
                    resolved_path: None,
                    loader_api: None,
                    module_handle: None,
                    state: NativeLoadState::Failed,
                    stage: "manifest_parse".to_string(),
                    detail: error.to_string(),
                    verified_exports: Vec::new(),
                    verified_modules: Vec::new(),
                    elapsed_ms: 0,
                });
                summary.failed += 1;
                continue;
            }
        };

        if manifest.mod_type != "native" && manifest.mod_type != MOD_TYPE_PRELOAD_NATIVE {
            continue;
        }

        let dll_path = package_dir.join(&manifest.entry);
        if is_reserved_system_runtime(&dll_path) {
            logging::write_bootstrap_marker(&format!(
                "native-load skipped phase=dllmain reason=system_runtime_owned_by_host path={}",
                dll_path.display()
            ));
            continue;
        }

        summary.discovered += 1;
        let id = manifest.id.unwrap_or_else(|| manifest.name.clone());
        let preload = PreloadMod {
            id,
            name: manifest.name,
            version: manifest.version,
            kind: manifest.mod_type,
            dll_path,
            log_aliases: manifest.log_aliases,
            required: manifest.required,
            verify_exports: manifest.verify_exports,
            verify_modules: manifest.verify_modules,
            notify_success: manifest.notify_success,
        };
        let _ = diagnostics_identity(&preload);
        native_preloads.push(preload);
    }

    native_preloads.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.dll_path.cmp(&b.dll_path))
    });

    for preload in native_preloads {
        summary.attempted += 1;
        let report = unsafe { load_and_verify_native(&preload, "dllmain_preload", true) };
        match report.state {
            NativeLoadState::Verified | NativeLoadState::AlreadyVerified => {
                summary.verified += 1;
                if report.notify_success {
                    summary.success_notifications += 1;
                }
            }
            NativeLoadState::Failed => {
                summary.failed += 1;
                if report.required {
                    summary.required_failed += 1;
                }
            }
        }
        apply_native_diagnostics(&preload, &report);
        record_native_report(report);
    }

    let _ = NATIVE_PRELOAD_SUMMARY.set(summary);
    logging::write_bootstrap_marker(&format!(
        "native-load summary phase=dllmain discovered={} attempted={} verified={} failed={} required_failed={}",
        summary.discovered,
        summary.attempted,
        summary.verified,
        summary.failed,
        summary.required_failed,
    ));
    summary
}

fn package_display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "<unknown-package>".to_string())
}

fn is_reserved_system_runtime(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("xgameruntime.dll"))
}

fn diagnostics_identity(preload: &PreloadMod) -> mod_diagnostics::ModIdentity {
    mod_diagnostics::register_discovered(
        preload.id.clone(),
        preload.name.clone(),
        preload.version.clone(),
        preload.kind.clone(),
        &preload.dll_path,
        preload.log_aliases.clone(),
    )
}

fn apply_native_diagnostics(preload: &PreloadMod, report: &NativeLoadReport) {
    let identity = diagnostics_identity(preload);
    match report.state {
        NativeLoadState::Verified | NativeLoadState::AlreadyVerified => {
            let module = report
                .module_handle
                .as_deref()
                .and_then(|value| value.strip_prefix("0x"))
                .and_then(|value| usize::from_str_radix(value, 16).ok())
                .unwrap_or(0);
            if module != 0 {
                mod_diagnostics::mark_loaded(&identity, module, &report.phase);
            } else {
                mod_diagnostics::record_lifecycle(
                    &identity,
                    "load_verified",
                    &format!("phase={} handle=unavailable", report.phase),
                );
            }
        }
        NativeLoadState::Failed => {
            mod_diagnostics::mark_failed(
                &identity,
                &report.phase,
                &format!("stage={} detail={}", report.stage, report.detail),
            );
        }
    }
}

unsafe fn load_and_verify_native(
    preload: &PreloadMod,
    phase: &str,
    prefer_load_library_a: bool,
) -> NativeLoadReport {
    let started = Instant::now();
    let expected_path = canonical_or_original(&preload.dll_path);
    let expected_text = expected_path.display().to_string();

    if !is_dll_path(&expected_path) {
        return NativeLoadReport {
            name: preload.name.clone(),
            phase: phase.to_string(),
            required: preload.required,
            notify_success: preload.notify_success,
            expected_path: expected_text,
            resolved_path: None,
            loader_api: None,
            module_handle: None,
            state: NativeLoadState::Failed,
            stage: "entry_validation".to_string(),
            detail: "DLL entry does not exist, is not a file, or does not end with .dll".to_string(),
            verified_exports: Vec::new(),
            verified_modules: Vec::new(),
            elapsed_ms: started.elapsed().as_millis(),
        };
    }

    let key = dll_key(&expected_path);
    if LOADED_DLL_KEYS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&key)
    {
        return NativeLoadReport {
            name: preload.name.clone(),
            phase: phase.to_string(),
            required: preload.required,
            notify_success: preload.notify_success,
            expected_path: expected_text.clone(),
            resolved_path: Some(expected_text),
            loader_api: None,
            module_handle: None,
            state: NativeLoadState::AlreadyVerified,
            stage: "deduplicated".to_string(),
            detail: "the exact DLL path was already loaded and verified by BLoader".to_string(),
            verified_exports: preload.verify_exports.clone(),
            verified_modules: preload.verify_modules.clone(),
            elapsed_ms: started.elapsed().as_millis(),
        };
    }

    logging::write_bootstrap_marker(&format!(
        "native-load begin phase={} name={} required={} path={}",
        phase,
        preload.name,
        preload.required,
        expected_path.display(),
    ));

    let preexisting = module_by_base_name(&expected_path).map(|module| {
        let path = canonical_or_original(&crate::utils::get_module_path(module.0 as usize));
        (module, path)
    });

    if let Some((module, loaded_path)) = &preexisting {
        if !paths_equivalent(&expected_path, loaded_path) {
            return NativeLoadReport {
                name: preload.name.clone(),
                phase: phase.to_string(),
                required: preload.required,
                notify_success: preload.notify_success,
                expected_path: expected_text,
                resolved_path: Some(loaded_path.display().to_string()),
                loader_api: Some("GetModuleHandleW".to_string()),
                module_handle: Some(format!("0x{:X}", module.0 as usize)),
                state: NativeLoadState::Failed,
                stage: "preexisting_module_collision".to_string(),
                detail: "a different DLL with the same base name was already loaded before BLoader; the requested proxy cannot become the active runtime".to_string(),
                verified_exports: Vec::new(),
                verified_modules: Vec::new(),
                elapsed_ms: started.elapsed().as_millis(),
            };
        }
    }

    let (module, loader_api, already_loaded) = if let Some((module, _)) = preexisting {
        (module, "GetModuleHandleW", true)
    } else {
        let identity = diagnostics_identity(preload);
        mod_diagnostics::mark_loading(&identity, phase);
        let load_result = {
            let _scope = mod_diagnostics::enter_scope(&identity, format!("LoadLibrary:{phase}"));
            mod_diagnostics::record_lifecycle(
                &identity,
                "native_entry_call",
                &format!(
                    "phase={phase} api_preference={}",
                    if prefer_load_library_a {
                        "LoadLibraryA"
                    } else {
                        "LoadLibraryW"
                    }
                ),
            );
            unsafe {
                native_stdio::capture_library_load(&identity, phase, || {
                    load_native_library(&expected_path, prefer_load_library_a)
                })
            }
        };
        // A third-party DLL can replace the top-level exception filter from its
        // DllMain. Restore BLoader attribution before any later Mod executes.
        crash_report::rearm_unhandled_filter(&format!("after-native-load:{}", preload.id));
        match load_result {
            Ok((module, loader_api)) => (module, loader_api, false),
            Err(detail) => {
                return NativeLoadReport {
                    name: preload.name.clone(),
                    phase: phase.to_string(),
                    required: preload.required,
                    notify_success: preload.notify_success,
                    expected_path: expected_text,
                    resolved_path: None,
                    loader_api: Some(if prefer_load_library_a {
                        "LoadLibraryA".to_string()
                    } else {
                        "LoadLibraryW".to_string()
                    }),
                    module_handle: None,
                    state: NativeLoadState::Failed,
                    stage: "load_library".to_string(),
                    detail,
                    verified_exports: Vec::new(),
                    verified_modules: Vec::new(),
                    elapsed_ms: started.elapsed().as_millis(),
                };
            }
        }
    };

    let resolved_path = crate::utils::get_module_path(module.0 as usize);
    if resolved_path.as_os_str().is_empty() {
        return NativeLoadReport {
            name: preload.name.clone(),
            phase: phase.to_string(),
            required: preload.required,
            notify_success: preload.notify_success,
            expected_path: expected_text,
            resolved_path: None,
            loader_api: Some(loader_api.to_string()),
            module_handle: Some(format!("0x{:X}", module.0 as usize)),
            state: NativeLoadState::Failed,
            stage: "module_path_query".to_string(),
            detail: "LoadLibrary returned a module handle but GetModuleFileNameW could not resolve it"
                .to_string(),
            verified_exports: Vec::new(),
            verified_modules: Vec::new(),
            elapsed_ms: started.elapsed().as_millis(),
        };
    }

    let resolved_path = canonical_or_original(&resolved_path);
    if !paths_equivalent(&expected_path, &resolved_path) {
        return NativeLoadReport {
            name: preload.name.clone(),
            phase: phase.to_string(),
            required: preload.required,
            notify_success: preload.notify_success,
            expected_path: expected_text,
            resolved_path: Some(resolved_path.display().to_string()),
            loader_api: Some(loader_api.to_string()),
            module_handle: Some(format!("0x{:X}", module.0 as usize)),
            state: NativeLoadState::Failed,
            stage: "module_path_verification".to_string(),
            detail: "LoadLibrary returned a different module path; probable same-name module collision"
                .to_string(),
            verified_exports: Vec::new(),
            verified_modules: Vec::new(),
            elapsed_ms: started.elapsed().as_millis(),
        };
    }

    let mut verified_exports = Vec::new();
    for export in &preload.verify_exports {
        let Ok(export_name) = CString::new(export.as_bytes()) else {
            return failed_verification_report(
                preload,
                phase,
                &expected_path,
                &resolved_path,
                module,
                loader_api,
                "export_verification",
                format!("export name contains an embedded NUL: {export}"),
                verified_exports,
                Vec::new(),
                started,
            );
        };
        if unsafe { GetProcAddress(module, PCSTR(export_name.as_ptr().cast())) }.is_none() {
            return failed_verification_report(
                preload,
                phase,
                &expected_path,
                &resolved_path,
                module,
                loader_api,
                "export_verification",
                format!("required export is missing: {export}"),
                verified_exports,
                Vec::new(),
                started,
            );
        }
        verified_exports.push(export.clone());
    }

    let mut verified_modules = Vec::new();
    for module_name in &preload.verify_modules {
        let wide = wide_null(std::ffi::OsStr::new(module_name.as_str()));
        let dependency = unsafe { GetModuleHandleW(PCWSTR(wide.as_ptr())) };
        let Ok(dependency) = dependency else {
            return failed_verification_report(
                preload,
                phase,
                &expected_path,
                &resolved_path,
                module,
                loader_api,
                "dependency_verification",
                format!("required module is not loaded: {module_name}"),
                verified_exports,
                verified_modules,
                started,
            );
        };
        let dependency_path = crate::utils::get_module_path(dependency.0 as usize);
        verified_modules.push(format!("{}={}", module_name, dependency_path.display()));
    }

    LOADED_DLL_KEYS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(key);

    NativeLoadReport {
        name: preload.name.clone(),
        phase: phase.to_string(),
        required: preload.required,
        notify_success: preload.notify_success,
        expected_path: expected_path.display().to_string(),
        resolved_path: Some(resolved_path.display().to_string()),
        loader_api: Some(loader_api.to_string()),
        module_handle: Some(format!("0x{:X}", module.0 as usize)),
        state: if already_loaded {
            NativeLoadState::AlreadyVerified
        } else {
            NativeLoadState::Verified
        },
        stage: if already_loaded {
            "already_loaded_verified".to_string()
        } else {
            "verified".to_string()
        },
        detail: "DllMain returned successfully; module path, exports, and dependency modules were verified"
            .to_string(),
        verified_exports,
        verified_modules,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

unsafe fn load_native_library(
    path: &Path,
    prefer_load_library_a: bool,
) -> Result<(HMODULE, &'static str), String> {
    let path_text = path.to_string_lossy();
    if prefer_load_library_a && path_text.is_ascii() {
        let path_ansi = CString::new(path_text.as_bytes())
            .map_err(|_| "path contains an embedded NUL".to_string())?;
        return unsafe { LoadLibraryA(PCSTR(path_ansi.as_ptr().cast())) }
            .map(|module| (module, "LoadLibraryA"))
            .map_err(|error| {
                format!(
                    "LoadLibraryA failed code=0x{:08X} message={}",
                    error.code().0 as u32,
                    error.message()
                )
            });
    }

    if prefer_load_library_a && !path_text.is_ascii() {
        logging::write_bootstrap_marker(&format!(
            "native-load api_fallback requested=LoadLibraryA actual=LoadLibraryW reason=non_ascii_path path={}",
            path.display()
        ));
    }
    let wide = wide_null(path.as_os_str());
    unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }
        .map(|module| (module, "LoadLibraryW"))
        .map_err(|error| {
            format!(
                "LoadLibraryW failed code=0x{:08X} message={}",
                error.code().0 as u32,
                error.message()
            )
        })
}

fn failed_verification_report(
    preload: &PreloadMod,
    phase: &str,
    expected_path: &Path,
    resolved_path: &Path,
    module: HMODULE,
    loader_api: &str,
    stage: &str,
    detail: String,
    verified_exports: Vec<String>,
    verified_modules: Vec<String>,
    started: Instant,
) -> NativeLoadReport {
    NativeLoadReport {
        name: preload.name.clone(),
        phase: phase.to_string(),
        required: preload.required,
        notify_success: preload.notify_success,
        expected_path: expected_path.display().to_string(),
        resolved_path: Some(resolved_path.display().to_string()),
        loader_api: Some(loader_api.to_string()),
        module_handle: Some(format!("0x{:X}", module.0 as usize)),
        state: NativeLoadState::Failed,
        stage: stage.to_string(),
        detail,
        verified_exports,
        verified_modules,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

fn module_by_base_name(path: &Path) -> Option<HMODULE> {
    let file_name = path.file_name()?;
    let wide = wide_null(file_name);
    unsafe { GetModuleHandleW(PCWSTR(wide.as_ptr())).ok() }
}

pub fn verified_native_module_path(module_name: &str) -> Option<PathBuf> {
    let wide = wide_null(std::ffi::OsStr::new(module_name));
    let module = unsafe { GetModuleHandleW(PCWSTR(wide.as_ptr())).ok()? };
    let path = crate::utils::get_module_path(module.0 as usize);
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(canonical_or_original(&path))
}

fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    dll_key(left) == dll_key(right)
}

fn record_native_report(report: NativeLoadReport) {
    let state = match report.state {
        NativeLoadState::Verified => "verified",
        NativeLoadState::Failed => "failed",
        NativeLoadState::AlreadyVerified => "already_verified",
    };
    logging::write_bootstrap_marker(&format!(
        "native-load result phase={} state={} stage={} name={} required={} handle={} expected={} resolved={} detail={}",
        report.phase,
        state,
        report.stage,
        report.name,
        report.required,
        report.module_handle.as_deref().unwrap_or("none"),
        report.expected_path,
        report.resolved_path.as_deref().unwrap_or("none"),
        report.detail,
    ));
    NATIVE_LOAD_REPORTS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(report);
}

pub fn publish_native_preload_reports() -> NativePreloadSummary {
    let summary = NATIVE_PRELOAD_SUMMARY.get().copied().unwrap_or_default();
    let reports = NATIVE_LOAD_REPORTS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();

    for report in &reports {
        let message = format!(
            "state={:?} stage={} name={} required={} api={} handle={} expected={} resolved={} exports=[{}] modules=[{}] elapsed={}ms detail={}",
            report.state,
            report.stage,
            report.name,
            report.required,
            report.loader_api.as_deref().unwrap_or("none"),
            report.module_handle.as_deref().unwrap_or("none"),
            report.expected_path,
            report.resolved_path.as_deref().unwrap_or("none"),
            report.verified_exports.join(","),
            report.verified_modules.join(","),
            report.elapsed_ms,
            report.detail,
        );
        match report.state {
            NativeLoadState::Verified | NativeLoadState::AlreadyVerified => {
                logging::scoped_info_message("native-loader", &format!("LOAD_SUCCESS | {message}"));
            }
            NativeLoadState::Failed => {
                logging::scoped_error_message("native-loader", &format!("LOAD_FAILURE | {message}"));
            }
        }
    }

    let status_path = write_native_status_file(summary, &reports);

    logging::scoped_info_message(
        "native-loader",
        &format!(
            "SUMMARY | discovered={} attempted={} verified={} failed={} required_failed={} status_file={}",
            summary.discovered,
            summary.attempted,
            summary.verified,
            summary.failed,
            summary.required_failed,
            status_path.display(),
        ),
    );
    summary
}

fn write_native_status_file(_summary: NativePreloadSummary, _reports: &[NativeLoadReport]) -> PathBuf {
    PathBuf::from("<memory-only>")
}

pub fn required_native_failure_message() -> Option<String> {
    let reports = NATIVE_LOAD_REPORTS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let failures: Vec<_> = reports
        .iter()
        .filter(|report| report.required && matches!(report.state, NativeLoadState::Failed))
        .collect();
    if failures.is_empty() {
        return None;
    }

    let mut lines = vec![
        "One or more required native modules failed to load or verify.".to_string(),
        "The game may continue, but the requested runtime/mod functionality is not active.".to_string(),
        String::new(),
    ];
    for report in failures.iter().take(8) {
        lines.push(format!(
            "- {}: {} ({})",
            report.name, report.detail, report.stage
        ));
    }
    lines.push(String::new());
    lines.push("Details are available through the live console/debug diagnostic stream.".to_string());
    Some(lines.join("\n"))
}

pub fn native_success_notification_message() -> Option<String> {
    let reports = NATIVE_LOAD_REPORTS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let successes: Vec<_> = reports
        .iter()
        .filter(|report| {
            report.notify_success
                && matches!(
                    report.state,
                    NativeLoadState::Verified | NativeLoadState::AlreadyVerified
                )
        })
        .collect();
    if successes.is_empty() {
        return None;
    }

    let mut lines = vec!["Native module loading verified successfully:".to_string(), String::new()];
    for report in successes.iter().take(8) {
        lines.push(format!(
            "- {}: {} | {}",
            report.name,
            report.resolved_path.as_deref().unwrap_or(&report.expected_path),
            report.module_handle.as_deref().unwrap_or("handle unavailable")
        ));
    }
    lines.push(String::new());
    lines.push("Detailed status is memory-only in this diagnostic build.".to_string());
    Some(lines.join("\n"))
}

/// 主加载入口：扫描并加载所有 Mods
/// 返回值: bool (true 表示 PreLoader 已加载并接管，false 表示普通加载)
pub unsafe fn load_mods(game_dir: &Path) -> bool {
    let mods_dir = ensure_mods_dir(game_dir);
    let mut discovered = discover_mods(&mods_dir);

    if discovered.preload.is_empty() && discovered.hot.is_empty() && discovered.bl.is_empty() {
        logging::info_message("No mods found to load.");
        return false;
    }

    let preloader_activated = load_preload_mods(&mut discovered.preload);
    load_bl_mods(&mut discovered.bl);
    spawn_hot_mod_loader(discovered.hot);

    preloader_activated
}

fn ensure_mods_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("mods")
}

fn discover_mods(mods_dir: &Path) -> DiscoveredMods {
    let mut discovered = DiscoveredMods::default();
    let entries: Vec<_> = fs::read_dir(mods_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .collect();

    for entry in entries {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            discover_packaged_mod(&entry_path, &mut discovered);
            continue;
        }

        if entry_path.is_file() {
            discover_loose_dll(mods_dir, &entry_path, &mut discovered);
        }
    }

    discovered
}

fn discover_packaged_mod(entry_path: &Path, discovered: &mut DiscoveredMods) {
    let manifest_path = entry_path.join("manifest.json");
    if !manifest_path.exists() {
        return;
    }

    let Ok(text) = fs::read_to_string(&manifest_path) else {
        logging::warn_message(&format!(
            "Failed to open manifest: {}",
            manifest_path.display()
        ));
        return;
    };

    let Ok(manifest) = parse_manifest_json(&text) else {
        logging::warn_message(&format!(
            "Failed to parse manifest: {}",
            manifest_path.display()
        ));
        return;
    };

    let dll_path = entry_path.join(&manifest.entry);
    if !is_dll_path(&dll_path) {
        return;
    }
    if is_reserved_system_runtime(&dll_path) {
        logging::scoped_info_message(
            "native-loader",
            &format!(
                "SKIP_RESERVED_RUNTIME | BLoader does not load system runtime DLLs | path={}",
                dll_path.display()
            ),
        );
        return;
    }

    let id = manifest.id.clone().unwrap_or_else(|| manifest.name.clone());
    let kind = manifest.mod_type.clone();
    match manifest.mod_type.as_str() {
        MOD_TYPE_BL => discovered.bl.push(BlMod {
            id,
            name: manifest.name,
            dll_path,
            api_version: manifest.api_version.unwrap_or(1),
            version: manifest.version,
            author: manifest.author,
            description: manifest.description,
            log_aliases: manifest.log_aliases,
            requires_symbol_pack: manifest.requires_symbol_pack,
            required_symbols: manifest.required_symbols,
        }),
        MOD_TYPE_HOT_NATIVE | MOD_TYPE_HOT_INJECT => {
            let ready_level = resolve_hot_ready_level(&manifest);
            if let Some(frames) = manifest.inject_min_frames {
                logging::scoped_warn_message(
                    "runtime-ready",
                    &format!(
                        "mod={} deprecated inject_min_frames={} ignored; readiness={} graphics_hooks=disabled",
                        manifest.name,
                        frames,
                        ready_level.as_str()
                    ),
                );
            }
            discovered.hot.push(HotMod {
                id,
                name: manifest.name,
                version: manifest.version,
                kind,
                dll_path,
                log_aliases: manifest.log_aliases,
                inject_delay_ms: manifest
                    .inject_delay_ms
                    .unwrap_or(HOT_INJECT_DEFAULT_DELAY_MS),
                ready_level,
                required: manifest.required,
                verify_exports: manifest.verify_exports,
                verify_modules: manifest.verify_modules,
                notify_success: manifest.notify_success,
            });
        }
        _ => discovered.preload.push(PreloadMod {
            id,
            name: manifest.name,
            version: manifest.version,
            kind,
            dll_path,
            log_aliases: manifest.log_aliases,
            required: manifest.required,
            verify_exports: manifest.verify_exports,
            verify_modules: manifest.verify_modules,
            notify_success: manifest.notify_success,
        }),
    }
}

fn resolve_hot_ready_level(manifest: &ModManifest) -> ReadyLevel {
    let default = if manifest.mod_type == MOD_TYPE_HOT_NATIVE {
        ReadyLevel::Window
    } else {
        ReadyLevel::StableWindow
    };

    let Some(configured) = manifest.inject_ready.as_deref() else {
        return default;
    };

    if let Some(level) = ReadyLevel::from_manifest(configured) {
        return level;
    }

    logging::scoped_warn_message(
        "runtime-ready",
        &format!(
            "mod={} invalid inject_ready={} fallback={} supported=process|window|stable-window",
            manifest.name,
            configured,
            default.as_str()
        ),
    );
    default
}

fn parse_manifest_json(text: &str) -> Result<ModManifest, serde_json::Error> {
    match serde_json::from_str(text) {
        Ok(manifest) => Ok(manifest),
        Err(_) => serde_json::from_str::<ManifestBundle>(text).map(|bundle| bundle.manifest),
    }
}

fn discover_loose_dll(mods_dir: &Path, entry_path: &Path, discovered: &mut DiscoveredMods) {
    if !is_dll_path(entry_path) {
        return;
    }
    if is_reserved_system_runtime(entry_path) {
        logging::scoped_info_message(
            "native-loader",
            &format!(
                "SKIP_RESERVED_RUNTIME | loose system runtime DLL is not packaged or loaded | path={}",
                entry_path.display()
            ),
        );
        return;
    }

    let Some(packaged) = package_loose_dll(mods_dir, entry_path) else {
        return;
    };

    discovered.preload.push(packaged);
}

fn package_loose_dll(_mods_dir: &Path, entry_path: &Path) -> Option<PreloadMod> {
    let file_stem = entry_path.file_stem()?.to_string_lossy().to_string();
    Some(PreloadMod {
        id: file_stem.clone(),
        name: file_stem,
        version: None,
        kind: MOD_TYPE_PRELOAD_NATIVE.to_string(),
        dll_path: entry_path.to_path_buf(),
        log_aliases: Vec::new(),
        required: false,
        verify_exports: Vec::new(),
        verify_modules: Vec::new(),
        notify_success: false,
    })
}

fn load_preload_mods(preload_mods: &mut Vec<PreloadMod>) -> bool {
    preload_mods.sort_by(|a, b| {
        let a_is_preloader = a.name == "PreLoader";
        let b_is_preloader = b.name == "PreLoader";

        if a_is_preloader && !b_is_preloader {
            std::cmp::Ordering::Less
        } else if !a_is_preloader && b_is_preloader {
            std::cmp::Ordering::Greater
        } else {
            a.name.cmp(&b.name)
        }
    });

    let mut preloader_activated = false;

    for preload in preload_mods {
        let is_preloader = preload.name == "PreLoader";

        logging::info_message(&format!(
            "Loading Mod: {} <{}>",
            preload.name,
            preload
                .dll_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));

        let loaded = unsafe { load_library(preload, "preload_native", is_preloader) };

        if is_preloader && loaded {
            preloader_activated = true;
            break;
        }
    }

    preloader_activated
}

fn load_bl_mods(bl_mods: &mut Vec<BlMod>) {
    bl_mods.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

    for mod_info in bl_mods {
        if mod_info.api_version != 1 {
            logging::warn_message(&format!(
                "Skipping BL mod {} ({}): unsupported api_version {}",
                mod_info.name, mod_info.id, mod_info.api_version
            ));
            continue;
        }
        if let Some(reason) =
            symbol_requirement_message(mod_info.requires_symbol_pack, &mod_info.required_symbols)
        {
            logging::warn_message(&format!(
                "Skipping BL mod {} ({}): symbol requirements not met: {}",
                mod_info.name, mod_info.id, reason
            ));
            continue;
        }

        logging::info_message(&format!(
            "Loading BL Mod: {} ({}) <{}>",
            mod_info.name,
            mod_info.id,
            mod_info
                .dll_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));
        unsafe {
            bl::loader::load_bl_mod(
                &mod_info.id,
                &mod_info.name,
                &mod_info.dll_path,
                mod_info.api_version,
                mod_info.version.as_deref(),
                mod_info.author.as_deref(),
                mod_info.description.as_deref(),
                &mod_info.log_aliases,
            );
        }
    }
}

fn symbol_requirement_message(
    requires_symbol_pack: bool,
    required_symbols: &[String],
) -> Option<String> {
    if requires_symbol_pack || !required_symbols.is_empty() {
        Some("Minecraft symbol subsystem is disabled in this lightweight build".to_string())
    } else {
        None
    }
}

fn spawn_hot_mod_loader(mut hot_mods: Vec<HotMod>) {
    if hot_mods.is_empty() {
        return;
    }

    let _ = thread::Builder::new()
        .name("bloader-hot-mod-loader".to_string())
        .spawn(move || {
            logging::scoped_info_message(
                "runtime-ready",
                &format!(
                    "hot Mod queue={} mode=oep+window-stability graphics_hooks=disabled",
                    hot_mods.len()
                ),
            );

            hot_mods.sort_by(|a, b| {
                a.inject_delay_ms
                    .cmp(&b.inject_delay_ms)
                    .then_with(|| a.ready_level.cmp(&b.ready_level))
                    .then_with(|| a.name.cmp(&b.name))
            });

            for hot_mod in hot_mods {
                let readiness_reached =
                    runtime_ready::wait_for(hot_mod.ready_level, HOT_INJECT_WAIT_TIMEOUT);
                runtime_ready::wait_until_oep_delay(hot_mod.inject_delay_ms);

                if !readiness_reached {
                    logging::scoped_warn_message(
                        "runtime-ready",
                        &format!(
                            "mod={} readiness={} timed out; continuing for compatibility after configured delay",
                            hot_mod.name,
                            hot_mod.ready_level.as_str()
                        ),
                    );
                }

                logging::info_message(&format!(
                    "Hot Loading Mod: {} <{}> readiness={} readiness_reached={} delay_from_oep={}ms graphics_hooks=disabled",
                    hot_mod.name,
                    hot_mod
                        .dll_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    hot_mod.ready_level.as_str(),
                    readiness_reached,
                    hot_mod.inject_delay_ms,
                ));
                let native_mod = PreloadMod {
                    id: hot_mod.id.clone(),
                    name: hot_mod.name.clone(),
                    version: hot_mod.version.clone(),
                    kind: hot_mod.kind.clone(),
                    dll_path: hot_mod.dll_path.clone(),
                    log_aliases: hot_mod.log_aliases.clone(),
                    required: hot_mod.required,
                    verify_exports: hot_mod.verify_exports.clone(),
                    verify_modules: hot_mod.verify_modules.clone(),
                    notify_success: hot_mod.notify_success,
                };
                unsafe { load_library(&native_mod, "hot_inject", false) };
            }
        });
}

fn is_dll_path(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .map(|ext| ext.to_string_lossy().eq_ignore_ascii_case("dll"))
        == Some(true)
}

fn dll_key(path: &Path) -> String {
    match path.canonicalize() {
        Ok(path) => path.to_string_lossy().to_lowercase(),
        Err(_) => path.to_string_lossy().to_lowercase(),
    }
}

unsafe fn load_library(preload: &PreloadMod, phase: &str, silent_success: bool) -> bool {
    let report = unsafe { load_and_verify_native(preload, phase, false) };
    let success = matches!(
        report.state,
        NativeLoadState::Verified | NativeLoadState::AlreadyVerified
    );

    if success {
        if !silent_success {
            logging::scoped_info_message(
                "native-loader",
                &format!(
                    "LOAD_SUCCESS | phase={} name={} handle={} resolved={} stage={}",
                    report.phase,
                    report.name,
                    report.module_handle.as_deref().unwrap_or("none"),
                    report.resolved_path.as_deref().unwrap_or("none"),
                    report.stage,
                ),
            );
        }
    } else {
        logging::scoped_error_message(
            "native-loader",
            &format!(
                "LOAD_FAILURE | phase={} name={} expected={} resolved={} stage={} detail={}",
                report.phase,
                report.name,
                report.expected_path,
                report.resolved_path.as_deref().unwrap_or("none"),
                report.stage,
                report.detail,
            ),
        );
    }

    let visible_title;
    let visible_message;
    if !success && report.required {
        visible_title = Some("BLoader Required Native Module Failed");
        visible_message = Some(format!(
            "Required native module '{}' failed during {}.\n\nStage: {}\nReason: {}\nExpected: {}\nResolved: {}\n\nSee logs\\latest.log and logs\\native-load-status.json.",
            report.name,
            report.phase,
            report.stage,
            report.detail,
            report.expected_path,
            report.resolved_path.as_deref().unwrap_or("not loaded"),
        ));
    } else if success && report.notify_success {
        visible_title = Some("BLoader Native Module Loaded");
        visible_message = Some(format!(
            "Native module '{}' was loaded and verified successfully.\n\nPhase: {}\nPath: {}\nHandle: {}\n\nDetailed status is memory-only in this diagnostic build.",
            report.name,
            report.phase,
            report.resolved_path.as_deref().unwrap_or(&report.expected_path),
            report.module_handle.as_deref().unwrap_or("unavailable"),
        ));
    } else {
        visible_title = None;
        visible_message = None;
    }

    apply_native_diagnostics(preload, &report);
    record_native_report(report);
    let reports = NATIVE_LOAD_REPORTS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let _ = write_native_status_file(
        NATIVE_PRELOAD_SUMMARY.get().copied().unwrap_or_default(),
        &reports,
    );

    if let (Some(title), Some(message)) = (visible_title, visible_message) {
        let title = title.to_string();
        let _ = thread::Builder::new()
            .name("bloader-native-load-notice".to_string())
            .spawn(move || {
                if success {
                    crate::runtime::foundation::error_dialog::show_native_load_success(
                        &title,
                        &message,
                    );
                } else {
                    crate::runtime::foundation::error_dialog::show_native_load_error(
                        &title,
                        &message,
                    );
                }
            });
    }

    success
}

#[cfg(test)]
mod tests {
    use super::{parse_manifest_json, resolve_hot_ready_level, symbol_requirement_message};
    use crate::core::runtime_ready::ReadyLevel;

    #[test]
    fn symbol_pack_requirement_blocks_a_mod_without_a_loaded_pack() {
        let reason = symbol_requirement_message(true, &[])
            .expect("a required pack must block the mod when no pack is loaded");

        assert_eq!(
            reason,
            "Minecraft symbol subsystem is disabled in this lightweight build"
        );
    }

    #[test]
    fn accepts_the_blgen_manifest_bundle_format() {
        let manifest = parse_manifest_json(
            r#"{"manifest":{"name":"Probe","entry":"probe.dll","type":"BL","requires_symbol_pack":true,"required_symbols":["client.instance"]}}"#,
        )
        .expect("blgen bundle must parse");

        assert!(manifest.requires_symbol_pack);
        assert_eq!(manifest.required_symbols, ["client.instance"]);
    }

    #[test]
    fn parses_native_load_verification_contract() {
        let manifest = parse_manifest_json(
            r#"{"name":"NativeProbe","entry":"native_probe.dll","type":"native","required":true,"notify_success":true,"verify_exports":["ProbeInitialize"],"verify_modules":["kernel32.dll"]}"#,
        )
        .expect("native verification fields must parse");

        assert!(manifest.required);
        assert!(manifest.notify_success);
        assert_eq!(manifest.verify_exports, ["ProbeInitialize"]);
        assert_eq!(manifest.verify_modules, ["kernel32.dll"]);
    }

    #[test]
    fn hot_inject_defaults_to_stable_window_readiness() {
        let manifest = parse_manifest_json(
            r#"{"name":"HotProbe","entry":"probe.dll","type":"hot-inject"}"#,
        )
        .expect("hot manifest must parse");
        assert_eq!(resolve_hot_ready_level(&manifest), ReadyLevel::StableWindow);
    }

    #[test]
    fn hot_native_accepts_explicit_process_readiness() {
        let manifest = parse_manifest_json(
            r#"{"name":"HotProbe","entry":"probe.dll","type":"hot-native","inject_ready":"process"}"#,
        )
        .expect("hot manifest must parse");
        assert_eq!(resolve_hot_ready_level(&manifest), ReadyLevel::Process);
    }
}

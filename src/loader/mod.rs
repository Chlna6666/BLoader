mod registry;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self};
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LoadLibraryA, LoadLibraryW,
};
use windows::core::{PCSTR, PCWSTR};

use crate::bootstrap::runtime_ready::{self, ReadyLevel};
use crate::platform::{
    file_io_policy, logging, mod_diagnostics, native_stdio,
};

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
    #[serde(default)]
    inject_min_frames: Option<usize>,
    #[serde(default)]
    inject_ready: Option<String>,
    #[serde(default)]
    requires_symbol_pack: bool,
    #[serde(default)]
    required_symbols: Vec<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    verify_exports: Vec<String>,
    #[serde(default)]
    verify_modules: Vec<String>,
    #[serde(default)]
    notify_success: bool,
    #[serde(default)]
    log_aliases: Vec<String>,
    /// BLoader's own dependency IDs; do not reinterpret LeviLamina's
    /// versioned 'dependencies' field (it has a different schema).
    #[serde(default)]
    bloader_dependencies: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestBundle {
    manifest: ModManifest,
}

#[derive(Clone)]
struct PreloadMod {
    id: String,
    name: String,
    dependencies: Vec<String>,
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
    dependencies: Vec<String>,
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

#[derive(Default)]
struct DiscoveredMods {
    hot: Vec<HotMod>,
}

const MOD_TYPE_MOD_PRELOAD: &str = "mod-preload";
const MOD_TYPE_MOD_NATIVE: &str = "mod-native";
const MOD_TYPE_HOT_NATIVE: &str = "hot-native";
const MOD_TYPE_HOT_INJECT: &str = "hot-inject";

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

pub unsafe fn load_mod_preloads_before_oep(game_dir: &Path) -> NativePreloadSummary {
    let mods_dir = bloader_mods_dir(game_dir);
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
                    phase: "mod_preload".to_string(),
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
                    phase: "mod_preload".to_string(),
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

        if manifest.mod_type != MOD_TYPE_MOD_PRELOAD {
            continue;
        }

        let Some(dll_path) = checked_mod_entry(&package_dir, &manifest.entry) else {
            logging::write_bootstrap_marker(&format!(
                "mod-preload ignored invalid or escaping entry package={} entry={}",
                package_dir.display(), manifest.entry
            ));
            continue;
        };
        if is_reserved_runtime_dll(&dll_path) {
            logging::write_bootstrap_marker(&format!(
                "native-load skipped phase=dllmain reason=system_runtime_owned_by_host path={}",
                dll_path.display()
            ));
            continue;
        }

        // 内置披风启用时抑制独立 Cosmetics.dll，避免 WinHTTP 钩子叠加。
        let package_name = package_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if crate::cosmetics::should_suppress_native_mod(package_name)
            || crate::cosmetics::should_suppress_native_mod(&manifest.name)
        {
            logging::write_bootstrap_marker(&format!(
                "native-load skipped phase=dllmain reason=cosmetics_builtin_suppressed name={}",
                manifest.name
            ));
            continue;
        }

        summary.discovered += 1;
        let id = manifest.id.unwrap_or_else(|| manifest.name.clone());
        let preload = PreloadMod {
            id,
            name: manifest.name,
            dependencies: manifest.bloader_dependencies,
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

    let mut ready = ready_mod_ids();
    let plan = plan_preload_mods(&native_preloads, &ready);
    for (index, reason) in plan.rejected {
        let preload = &native_preloads[index];
        record_dependency_failure(preload, "mod_preload", reason);
        summary.failed += 1;
        if preload.required {
            summary.required_failed += 1;
        }
    }
    for index in plan.order {
        let preload = &native_preloads[index];
        if let Some(dependency) = dependency_not_ready(&preload.dependencies, &ready) {
            record_dependency_failure(
                preload,
                "mod_preload",
                format!("required dependency {dependency} did not load successfully"),
            );
            summary.failed += 1;
            if preload.required {
                summary.required_failed += 1;
            }
            continue;
        }
        summary.attempted += 1;
        let report = unsafe { load_and_verify_native(preload, "mod_preload", true) };
        match report.state {
            NativeLoadState::Verified | NativeLoadState::AlreadyVerified => {
                ready.insert(preload.id.to_ascii_lowercase());
                ready.insert(preload.name.to_ascii_lowercase());
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
        apply_native_diagnostics(preload, &report);
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

/// Never confuse a BLoader-owned Mod with an external runtime/bootstrap or
/// LeviLamina-owned addon, even when the manifest type is forged.
fn is_reserved_runtime_dll(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            ["xgameruntime.dll", "preloader.dll", "levilamina.dll", "bedrockscenecapture.dll"]
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved))
        })
}

/// Manifest entries cannot escape a single BLoader Mod package, including by
/// ../ traversal, absolute paths, Win32 device prefixes, or directory links.
fn checked_mod_entry(package_dir: &Path, entry: &str) -> Option<PathBuf> {
    let relative = Path::new(entry);
    if entry.is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let root = package_dir.parent()?.canonicalize().ok()?;
    let dir = package_dir.canonicalize().ok()?;
    let dll = package_dir.join(relative).canonicalize().ok()?;
    if !dir.starts_with(&root) || !dll.starts_with(&dir) || !is_dll_path(&dll) {
        return None;
    }
    Some(dll)
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

fn ready_mod_ids() -> HashSet<String> {
    crate::platform::mod_diagnostics::all_mods()
        .into_iter()
        .filter(|identity| identity.state == "loaded")
        .flat_map(|identity| [
            identity.id.to_ascii_lowercase(),
            identity.name.to_ascii_lowercase(),
        ])
        .collect()
}

fn dependency_not_ready<'a>(
    dependencies: &'a [String],
    ready: &HashSet<String>,
) -> Option<&'a str> {
    dependencies
        .iter()
        .find(|dep| !ready.contains(&dep.trim().to_ascii_lowercase()))
        .map(String::as_str)
}

fn plan_preload_mods(mods: &[PreloadMod], ready: &HashSet<String>) -> registry::LoadPlan {
    let nodes = mods.iter()
        .map(|item| registry::ModNode {
            id: item.id.clone(),
            dependencies: item.dependencies.clone(),
        })
        .collect::<Vec<_>>();
    registry::plan_mods(&nodes, ready)
}

fn record_dependency_failure(preload: &PreloadMod, phase: &str, reason: String) {
    let report = NativeLoadReport {
        name: preload.name.clone(),
        phase: phase.to_string(),
        required: preload.required,
        notify_success: false,
        expected_path: preload.dll_path.display().to_string(),
        resolved_path: None,
        loader_api: None,
        module_handle: None,
        state: NativeLoadState::Failed,
        stage: "dependency_graph".to_string(),
        detail: reason,
        verified_exports: Vec::new(),
        verified_modules: Vec::new(),
        elapsed_ms: 0,
    };
    logging::scoped_error_message(
        "native-loader",
        &format!(
            "MOD_DEPENDENCY_FAILURE | phase={} name={} reason={}",
            phase, report.name, report.detail,
        ),
    );
    apply_native_diagnostics(preload, &report);
    record_native_report(report);
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
        let path = canonical_or_original(&crate::platform::utils::get_module_path(module.0 as usize));
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
        // Preserve the newly loaded Runtime's own exception filter.
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

    let resolved_path = crate::platform::utils::get_module_path(module.0 as usize);
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
        let dependency_path = crate::platform::utils::get_module_path(dependency.0 as usize);
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
    let path = crate::platform::utils::get_module_path(module.0 as usize);
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
                logging::scoped_info_message("native-loader", "LOAD_SUCCESS");
                logging::scoped_info_message("native-loader", &message);
            }
            NativeLoadState::Failed => {
                logging::scoped_error_message("native-loader", "LOAD_FAILURE");
                logging::scoped_error_message("native-loader", &message);
            }
        }
    }

    logging::scoped_info_message(
        "native-loader",
        &format!(
            "SUMMARY | discovered={} attempted={} verified={} failed={} required_failed={}",
            summary.discovered,
            summary.attempted,
            summary.verified,
            summary.failed,
            summary.required_failed,
        ),
    );
    summary
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
    if file_io_policy::writes_allowed() {
        lines.push(String::new());
        lines.push("See logs\\latest.log for details.".to_string());
    }
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
    Some(lines.join("\n"))
}

/// Schedule ordinary BLoader native mods after the Minecraft OEP release.
/// The separate mod-preload phase has already run on the gated startup thread.
pub unsafe fn load_mods(game_dir: &Path) {
    let mods_dir = ensure_mods_dir(game_dir);
    let discovered = discover_mods(&mods_dir);
    if discovered.hot.is_empty() {
        logging::info_message("No BLoader startup or delayed mods found.");
        return;
    }
    spawn_hot_mod_loader(discovered.hot);
}

fn bloader_mods_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("BLoader").join("mods")
}

fn ensure_mods_dir(game_dir: &Path) -> PathBuf {
    let mods_dir = bloader_mods_dir(game_dir);
    if !mods_dir.exists() {
        if file_io_policy::writes_allowed() {
            let _ = fs::create_dir_all(&mods_dir);
            logging::warn_message("Mods directory created, no mods loaded.");
        } else {
            logging::info_message(
                "Legacy UWP read-only mode: mods directory is absent and will not be created.",
            );
        }
    }
    mods_dir
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

    let mod_type = manifest.mod_type.as_str();
    if !matches!(
        mod_type,
        MOD_TYPE_MOD_PRELOAD | MOD_TYPE_MOD_NATIVE | MOD_TYPE_HOT_NATIVE | MOD_TYPE_HOT_INJECT
    ) {
        // PreLoader and LeviLamina are the sole owners of preload-native and
        // native respectively; do not retry their DLLs even after failure.
        logging::scoped_warn_message(
            "native-loader",
            &format!(
                "SKIP_FOREIGN_MOD_TYPE | name={} type={} | BLoader types=mod-preload|mod-native|hot-native|hot-inject; LeviLamina native and PreLoader preload-native are external",
                manifest.name, mod_type
            ),
        );
        return;
    }

    let Some(dll_path) = checked_mod_entry(entry_path, &manifest.entry) else {
        logging::scoped_error_message(
            "native-loader",
            &format!(
                "SKIP_INVALID_MOD_ENTRY | name={} entry={} | absolute paths, traversal, and symlink escapes are forbidden",
                manifest.name, manifest.entry,
            ),
        );
        return;
    };
    if is_reserved_runtime_dll(&dll_path) {
        logging::scoped_info_message(
            "native-loader",
            &format!(
                "SKIP_RESERVED_RUNTIME | BLoader does not load system runtime DLLs | path={}",
                dll_path.display()
            ),
        );
        return;
    }

    // 内置披风启用时抑制独立 Cosmetics.dll（preload/hot 两条路径都覆盖）。
    let package_name = entry_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if crate::cosmetics::should_suppress_native_mod(package_name)
        || crate::cosmetics::should_suppress_native_mod(&manifest.name)
    {
        logging::scoped_info_message(
            "native-loader",
            &format!(
                "SKIP_COSMETICS_BUILTIN | built-in cosmetics owns the WinHTTP overlay | name={}",
                manifest.name
            ),
        );
        return;
    }

    let id = manifest.id.clone().unwrap_or_else(|| manifest.name.clone());
    let kind = manifest.mod_type.clone();

    // The mod-preload manifest was handled during prepare_premain_preloads
    // (before Minecraft OEP) and must never be loaded a second time here.
    if manifest.mod_type == MOD_TYPE_MOD_PRELOAD {
        return;
    }

    let (ready_level, delay_ms) = if manifest.mod_type == MOD_TYPE_MOD_NATIVE {
        (ReadyLevel::Process, 0)
    } else {
        (resolve_hot_ready_level(&manifest),
         manifest.inject_delay_ms.unwrap_or(HOT_INJECT_DEFAULT_DELAY_MS))
    };
    if let Some(frames) = manifest.inject_min_frames {
        logging::scoped_warn_message(
            "runtime-ready",
            &format!(
                "mod={} ignored inject_min_frames={} readiness={} graphics_hooks=disabled",
                manifest.name,
                frames,
                ready_level.as_str(),
            ),
        );
    }
    discovered.hot.push(HotMod {
        id,
        name: manifest.name,
        dependencies: manifest.bloader_dependencies,
        version: manifest.version,
        kind,
        dll_path,
        log_aliases: manifest.log_aliases,
        inject_delay_ms: delay_ms,
        ready_level,
        required: manifest.required,
        verify_exports: manifest.verify_exports,
        verify_modules: manifest.verify_modules,
        notify_success: manifest.notify_success,
    });
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
    if is_reserved_runtime_dll(entry_path) {
        logging::scoped_info_message(
            "native-loader",
            &format!(
                "SKIP_RESERVED_RUNTIME | loose system runtime DLL is not packaged or loaded | path={}",
                entry_path.display()
            ),
        );
        return;
    }
    // Unmanifested DLLs are packaged as mod-native, never as early mod-preload.
    // Do not load them inside the game bootstrap before OEP.
    if let Some(packaged) = package_loose_dll(mods_dir, entry_path) {
        discovered.hot.push(HotMod {
            id: packaged.id,
            name: packaged.name,
            dependencies: packaged.dependencies,
            version: packaged.version,
            kind: MOD_TYPE_MOD_NATIVE.to_string(),
            dll_path: packaged.dll_path,
            log_aliases: packaged.log_aliases,
            inject_delay_ms: 0,
            ready_level: ReadyLevel::Process,
            required: packaged.required,
            verify_exports: packaged.verify_exports,
            verify_modules: packaged.verify_modules,
            notify_success: packaged.notify_success,
        });
    }
}

fn package_loose_dll(mods_dir: &Path, entry_path: &Path) -> Option<PreloadMod> {
    if !file_io_policy::writes_allowed() {
        logging::scoped_warn_message(
            "native-loader",
            &format!(
                "SKIP_AUTO_PACKAGE | legacy UWP read-only mode leaves loose DLL untouched | path={}",
                entry_path.display()
            ),
        );
        return None;
    }

    let file_stem = entry_path.file_stem()?.to_string_lossy().to_string();
    let file_name = entry_path.file_name()?.to_string_lossy().to_string();
    let target_dir = mods_dir.join(&file_stem);
    let target_dll = target_dir.join(&file_name);
    let target_manifest = target_dir.join("manifest.json");

    logging::info_message(&format!("Auto-packaging found: {}", file_name));

    if !target_dir.exists() && fs::create_dir(&target_dir).is_err() {
        logging::error_message(&format!("Failed to create package dir for {}", file_stem));
        return None;
    }

    if !target_dll.exists() {
        if let Err(error) = fs::rename(entry_path, &target_dll) {
            logging::error_message(&format!("Failed to move DLL {}: {}", file_name, error));
            return None;
        }
    } else {
        logging::warn_message(&format!(
            "Target DLL {} already exists, skipping move.",
            target_dll.display()
        ));
    }

    if !target_manifest.exists() {
        let manifest = ModManifest {
            id: Some(file_stem.clone()),
            name: file_stem.clone(),
            entry: file_name.clone(),
            version: None,
            author: None,
            description: None,
            mod_type: MOD_TYPE_MOD_NATIVE.to_string(),
            api_version: None,
            inject_delay_ms: None,
            inject_min_frames: None,
            inject_ready: None,
            requires_symbol_pack: false,
            required_symbols: Vec::new(),
            required: false,
            verify_exports: Vec::new(),
            verify_modules: Vec::new(),
            notify_success: false,
            log_aliases: Vec::new(),
            bloader_dependencies: Vec::new(),
        };

        match serde_json::to_string_pretty(&manifest) {
            Ok(json) => {
                if let Err(error) = fs::write(&target_manifest, json) {
                    logging::error_message(&format!(
                        "Failed to generate manifest for {}: {}",
                        file_stem, error
                    ));
                }
            }
            Err(error) => logging::error_message(&format!(
                "Failed to serialize manifest for {}: {}",
                file_stem, error
            )),
        }
    }

    Some(PreloadMod {
        id: file_stem.clone(),
        name: file_stem,
        dependencies: Vec::new(),
        version: None,
        kind: MOD_TYPE_MOD_NATIVE.to_string(),
        dll_path: target_dll,
        log_aliases: Vec::new(),
        required: false,
        verify_exports: Vec::new(),
        verify_modules: Vec::new(),
        notify_success: false,
    })
}

fn hot_as_native(hot: &HotMod) -> PreloadMod {
    PreloadMod {
        id: hot.id.clone(),
        name: hot.name.clone(),
        dependencies: hot.dependencies.clone(),
        version: hot.version.clone(),
        kind: hot.kind.clone(),
        dll_path: hot.dll_path.clone(),
        log_aliases: hot.log_aliases.clone(),
        required: hot.required,
        verify_exports: hot.verify_exports.clone(),
        verify_modules: hot.verify_modules.clone(),
        notify_success: hot.notify_success,
    }
}

fn spawn_hot_mod_loader(mut hot_mods: Vec<HotMod>) {
    if hot_mods.is_empty() {
        return;
    }
    let _ = thread::Builder::new()
        .name("bloader-hot-mod-loader".to_string())
        .spawn(move || {
            hot_mods.sort_by(|a, b| {
                a.inject_delay_ms
                    .cmp(&b.inject_delay_ms)
                    .then_with(|| a.ready_level.cmp(&b.ready_level))
                    .then_with(|| a.name.cmp(&b.name))
            });
            let mut ready = ready_mod_ids();
            let nodes = hot_mods.iter().map(|item| registry::ModNode {
                id: item.id.clone(),
                dependencies: item.dependencies.clone(),
            }).collect::<Vec<_>>();
            let plan = registry::plan_mods(&nodes, &ready);
            logging::scoped_info_message(
                "runtime-ready",
                &format!("hot Mod queue={} dependency_ordered=true", hot_mods.len()),
            );
            for (index, reason) in plan.rejected {
                let mod_phase = if hot_mods[index].kind == MOD_TYPE_MOD_NATIVE {
                    "mod_native"
                } else {
                    "hot_inject"
                };
                record_dependency_failure(&hot_as_native(&hot_mods[index]), mod_phase, reason);
            }
            for index in plan.order {
                let hot_mod = &hot_mods[index];
                let native_mod = hot_as_native(hot_mod);
                let mod_phase = if hot_mod.kind == MOD_TYPE_MOD_NATIVE {
                    "mod_native"
                } else {
                    "hot_inject"
                };
                if let Some(dependency) = dependency_not_ready(&hot_mod.dependencies, &ready) {
                    record_dependency_failure(
                        &native_mod,
                        mod_phase,
                        format!("required dependency {dependency} did not load successfully"),
                    );
                    continue;
                }

                let readiness_reached =
                    runtime_ready::wait_for(hot_mod.ready_level, HOT_INJECT_WAIT_TIMEOUT);
                if !readiness_reached {
                    record_dependency_failure(
                        &native_mod,
                        mod_phase,
                        format!(
                            "runtime readiness {} timed out; refusing unsafe load",
                            hot_mod.ready_level.as_str()
                        ),
                    );
                    continue;
                }
                runtime_ready::wait_until_oep_delay(hot_mod.inject_delay_ms);
                logging::info_message(&format!(
                    "Loading {} Mod: {} <{}> readiness={} delay_from_oep={}ms",
                    if hot_mod.kind == MOD_TYPE_MOD_NATIVE { "Native" } else { "Delayed" },
                    hot_mod.name,
                    hot_mod.dll_path.file_name().unwrap_or_default().to_string_lossy(),
                    hot_mod.ready_level.as_str(),
                    hot_mod.inject_delay_ms,
                ));
                if unsafe { load_library(&native_mod, mod_phase, false) } {
                    ready.insert(hot_mod.id.to_ascii_lowercase());
                    ready.insert(hot_mod.name.to_ascii_lowercase());
                }
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
            "Required native module '{}' failed during {}.\n\nStage: {}\nReason: {}\nExpected: {}\nResolved: {}{}",
            report.name,
            report.phase,
            report.stage,
            report.detail,
            report.expected_path,
            report.resolved_path.as_deref().unwrap_or("not loaded"),
            if file_io_policy::writes_allowed() {
                "\n\nSee logs\\latest.log."
            } else {
                ""
            },
        ));
    } else if success && report.notify_success {
        visible_title = Some("BLoader Native Module Loaded");
        visible_message = Some(format!(
            "Native module '{}' was loaded and verified successfully.\n\nPhase: {}\nPath: {}\nHandle: {}",
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

    if let (Some(title), Some(message)) = (visible_title, visible_message) {
        let title = title.to_string();
        let _ = thread::Builder::new()
            .name("bloader-native-load-notice".to_string())
            .spawn(move || {
                if success {
                    crate::platform::error_dialog::show_native_load_success(
                        &title,
                        &message,
                    );
                } else {
                    crate::platform::error_dialog::show_native_load_error(
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
    use super::{
        bloader_mods_dir, checked_mod_entry, is_reserved_runtime_dll, parse_manifest_json,
        resolve_hot_ready_level, MOD_TYPE_MOD_NATIVE, MOD_TYPE_MOD_PRELOAD,
    };
    use crate::bootstrap::runtime_ready::ReadyLevel;

    #[test]
    fn bloader_mods_are_outside_levilamina_mod_root() {
        let root = std::path::Path::new("C:/Minecraft");
        assert_eq!(bloader_mods_dir(root), root.join("BLoader").join("mods"));
        assert_ne!(bloader_mods_dir(root), root.join("mods"));
    }

    #[test]
    fn rejects_foreign_runtime_modules() {
        assert!(is_reserved_runtime_dll(std::path::Path::new("LeviLamina.dll")));
        assert!(is_reserved_runtime_dll(std::path::Path::new("PreLoader.dll")));
        assert!(is_reserved_runtime_dll(std::path::Path::new("BedrockSceneCapture.dll")));
        assert!(!is_reserved_runtime_dll(std::path::Path::new("Example.dll")));
    }

    #[test]
    fn rejects_mod_entry_path_traversal() {
        let root = std::env::temp_dir().join(format!(
            "bloader-entry-test-{}", std::process::id()
        ));
        let package = root.join("Example");
        std::fs::create_dir_all(&package).unwrap();
        let allowed = package.join("example.dll");
        std::fs::write(&allowed, b"fake").unwrap();
        assert_eq!(checked_mod_entry(&package, "example.dll").as_deref(), Some(allowed.canonicalize().unwrap().as_path()));
        assert!(checked_mod_entry(&package, "../outside.dll").is_none());
        assert!(checked_mod_entry(&package, "C:\\Windows\\System32\\kernel32.dll").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn distinct_bloader_types_do_not_overlap_external_loaders() {
        assert_eq!(MOD_TYPE_MOD_NATIVE, "mod-native");
        assert_eq!(MOD_TYPE_MOD_PRELOAD, "mod-preload");
        assert_ne!(MOD_TYPE_MOD_NATIVE, "native");
        assert_ne!(MOD_TYPE_MOD_PRELOAD, "preload-native");
    }

    #[test]
    fn parses_native_load_verification_contract() {
        let manifest = parse_manifest_json(
            r#"{"name":"NativeProbe","entry":"native_probe.dll","type":"mod-native","required":true,"notify_success":true,"verify_exports":["ProbeInitialize"],"verify_modules":["kernel32.dll"]}"#,
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

use serde::{Deserialize, Serialize};
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::core::PCWSTR;

use crate::runtime::foundation::{
    crash_report, file_io_policy, logging, mod_diagnostics, native_stdio,
};

const PRELOAD_TYPE: &str = "preload";
const PRELOADER_DLL: &str = "PreLoader.dll";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreloaderProxyState {
    NotFound,
    Loaded,
    Failed,
}

#[derive(Clone, Copy, Debug)]
pub struct PreloaderProxySummary {
    pub discovered: usize,
    pub state: PreloaderProxyState,
}

impl PreloaderProxySummary {
    pub fn active(self) -> bool {
        self.state == PreloaderProxyState::Loaded
    }

    pub fn failed(self) -> bool {
        self.state == PreloaderProxyState::Failed
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    name: String,
    version: Option<String>,
    path: PathBuf,
    log_aliases: Vec<String>,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    id: Option<String>,
    name: String,
    entry: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(rename = "type", default)]
    mod_type: String,
    #[serde(default)]
    log_aliases: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestBundle {
    manifest: Manifest,
}

#[derive(Serialize)]
struct ProxyStatus<'a> {
    state: &'a str,
    discovered: usize,
    candidate: Option<String>,
    resolved: Option<String>,
    detail: &'a str,
    elapsed_ms: u128,
}

pub unsafe fn try_load(game_dir: &Path) -> PreloaderProxySummary {
    let started = Instant::now();
    let mut candidates = discover_candidates(&game_dir.join("mods"));
    candidates.sort_by(|a, b| a.path.cmp(&b.path));

    if candidates.is_empty() {
        publish_status(ProxyStatus {
            state: "not_found",
            discovered: 0,
            candidate: None,
            resolved: None,
            detail: "no type=preload package with entry PreLoader.dll was found",
            elapsed_ms: started.elapsed().as_millis(),
        });
        logging::write_bootstrap_marker("preloader.early.not_found");
        return PreloaderProxySummary {
            discovered: 0,
            state: PreloaderProxyState::NotFound,
        };
    }

    if candidates.len() > 1 {
        logging::write_bootstrap_marker(&format!(
            "preloader.early.multiple count={} selected={}",
            candidates.len(),
            candidates[0].path.display()
        ));
        logging::scoped_warn_message(
            "preloader",
            &format!(
                "Multiple priority PreLoader packages found; using first deterministic candidate | count={} | selected={}",
                candidates.len(),
                candidates[0].path.display()
            ),
        );
    }

    let discovered = candidates.len();
    let candidate = candidates.remove(0);
    let expected = canonical_or_original(&candidate.path);
    let identity = mod_diagnostics::register_discovered(
        candidate.id.clone(),
        candidate.name.clone(),
        candidate.version.clone(),
        PRELOAD_TYPE,
        &expected,
        candidate.log_aliases.clone(),
    );

    logging::write_bootstrap_marker(&format!(
        "preloader.priority.detected name={} path={} type={} phase=bootstrap-thread-first",
        candidate.name,
        expected.display(),
        PRELOAD_TYPE
    ));
    logging::scoped_info_message(
        "preloader",
        &format!(
            "Priority preload detected | {} | {}",
            candidate.name,
            expected.display()
        ),
    );

    if let Some(module) = module_by_name(PRELOADER_DLL) {
        let resolved = canonical_or_original(&crate::utils::get_module_path(module.0 as usize));
        if paths_equivalent(&expected, &resolved) {
            mod_diagnostics::mark_loaded(&identity, module.0 as usize, "preloader_preexisting");
            let detail = "PreLoader was already loaded from the requested package; proxy ownership accepted";
            logging::write_bootstrap_marker(&format!(
                "preloader.priority.preexisting module=0x{:X} path={}",
                module.0 as usize,
                resolved.display()
            ));
            logging::scoped_info_message(
                "preloader",
                &format!(
                    "Priority preload already active | module=0x{:X} | path={}",
                    module.0 as usize,
                    resolved.display()
                ),
            );
            publish_status(ProxyStatus {
                state: "loaded",
                discovered,
                candidate: Some(expected.display().to_string()),
                resolved: Some(resolved.display().to_string()),
                detail,
                elapsed_ms: started.elapsed().as_millis(),
            });
            return PreloaderProxySummary {
                discovered,
                state: PreloaderProxyState::Loaded,
            };
        }

        let detail = format!(
            "PreLoader.dll name collision: expected={} loaded={}",
            expected.display(),
            resolved.display()
        );
        mod_diagnostics::mark_failed(&identity, "preloader_collision", &detail);
        logging::scoped_error_message("preloader", &detail);
        logging::write_bootstrap_marker(&format!("preloader.priority.collision {detail}"));
        publish_status(ProxyStatus {
            state: "failed",
            discovered,
            candidate: Some(expected.display().to_string()),
            resolved: Some(resolved.display().to_string()),
            detail: &detail,
            elapsed_ms: started.elapsed().as_millis(),
        });
        return PreloaderProxySummary {
            discovered,
            state: PreloaderProxyState::Failed,
        };
    }

    mod_diagnostics::mark_loading(&identity, "preloader_priority_early");
    logging::write_bootstrap_marker(&format!(
        "preloader.priority.load.begin path={} capture=native-stdio",
        expected.display()
    ));

    // This executes after the loader lock has been released, so the same native
    // stdout/stderr capture used for regular Mods is safe here. PreLoader can
    // synchronously load LeviLamina and additional native Mods; their early
    // printf/puts/std::cout/Rust stdout is captured before any console exists and
    // later replayed through BLoader's console backlog.
    let load_result = {
        let _scope = mod_diagnostics::enter_scope(&identity, "LoadLibrary:preloader_priority_early");
        let wide = wide_null(expected.as_os_str());
        unsafe {
            native_stdio::capture_library_load(&identity, "preloader_priority_early", || {
                LoadLibraryW(PCWSTR(wide.as_ptr()))
            })
        }
    };
    crash_report::rearm_unhandled_filter("after-preloader-priority-early-load");

    match load_result {
        Ok(module) => {
            let resolved = canonical_or_original(&crate::utils::get_module_path(module.0 as usize));
            if !paths_equivalent(&expected, &resolved) {
                let detail = format!(
                    "LoadLibrary returned a different PreLoader module path: expected={} resolved={}",
                    expected.display(),
                    resolved.display()
                );
                mod_diagnostics::mark_failed(&identity, "preloader_path_verification", &detail);
                logging::scoped_error_message("preloader", &detail);
                logging::write_bootstrap_marker(&format!(
                    "preloader.priority.path_mismatch {detail}"
                ));
                publish_status(ProxyStatus {
                    state: "failed",
                    discovered,
                    candidate: Some(expected.display().to_string()),
                    resolved: Some(resolved.display().to_string()),
                    detail: &detail,
                    elapsed_ms: started.elapsed().as_millis(),
                });
                return PreloaderProxySummary {
                    discovered,
                    state: PreloaderProxyState::Failed,
                };
            }

            mod_diagnostics::mark_loaded(&identity, module.0 as usize, "preloader_priority_early");
            let detail = "PreLoader loaded successfully during earliest bootstrap-thread phase; remaining preload modules are delegated to PreLoader";
            logging::scoped_info_message(
                "preloader",
                &format!(
                    "Priority preload active | module=0x{:X} | elapsed={}ms | BLoader direct preload pass disabled",
                    module.0 as usize,
                    started.elapsed().as_millis()
                ),
            );
            logging::write_bootstrap_marker(&format!(
                "preloader.priority.active module=0x{:X} path={} elapsed_ms={} delegate_remaining=true phase=bootstrap-thread-first",
                module.0 as usize,
                resolved.display(),
                started.elapsed().as_millis()
            ));
            publish_status(ProxyStatus {
                state: "loaded",
                discovered,
                candidate: Some(expected.display().to_string()),
                resolved: Some(resolved.display().to_string()),
                detail,
                elapsed_ms: started.elapsed().as_millis(),
            });
            PreloaderProxySummary {
                discovered,
                state: PreloaderProxyState::Loaded,
            }
        }
        Err(error) => {
            let detail = format!(
                "LoadLibraryW failed code=0x{:08X} message={}",
                error.code().0 as u32,
                error.message()
            );
            mod_diagnostics::mark_failed(&identity, "preloader_load", &detail);
            logging::scoped_error_message(
                "preloader",
                &format!(
                    "Priority preload failed | path={} | {} | falling back to BLoader preload loader",
                    expected.display(),
                    detail
                ),
            );
            logging::write_bootstrap_marker(&format!(
                "preloader.priority.failed path={} detail={} phase=bootstrap-thread-first",
                expected.display(),
                detail
            ));
            publish_status(ProxyStatus {
                state: "failed",
                discovered,
                candidate: Some(expected.display().to_string()),
                resolved: None,
                detail: &detail,
                elapsed_ms: started.elapsed().as_millis(),
            });
            PreloaderProxySummary {
                discovered,
                state: PreloaderProxyState::Failed,
            }
        }
    }
}

fn discover_candidates(mods_dir: &Path) -> Vec<Candidate> {
    let Ok(entries) = fs::read_dir(mods_dir) else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let package_dir = entry.path();
        if !package_dir.is_dir() {
            continue;
        }

        let manifest_path = package_dir.join("manifest.json");
        let Ok(text) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = parse_manifest(&text) else {
            continue;
        };
        if !manifest.mod_type.eq_ignore_ascii_case(PRELOAD_TYPE) {
            continue;
        }

        let path = package_dir.join(&manifest.entry);
        let is_preloader = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(PRELOADER_DLL));
        if !is_preloader || !path.is_file() {
            continue;
        }

        let name = manifest.name;
        candidates.push(Candidate {
            id: manifest.id.unwrap_or_else(|| name.clone()),
            name,
            version: manifest.version,
            path,
            log_aliases: manifest.log_aliases,
        });
    }
    candidates
}

fn parse_manifest(text: &str) -> Result<Manifest, serde_json::Error> {
    match serde_json::from_str(text) {
        Ok(manifest) => Ok(manifest),
        Err(_) => serde_json::from_str::<ManifestBundle>(text).map(|bundle| bundle.manifest),
    }
}

fn module_by_name(name: &str) -> Option<HMODULE> {
    let wide = wide_null(std::ffi::OsStr::new(name));
    unsafe { GetModuleHandleW(PCWSTR(wide.as_ptr())).ok() }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn normalize_path(path: &Path) -> String {
    canonical_or_original(path)
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn publish_status(status: ProxyStatus<'_>) {
    if !file_io_policy::writes_allowed() {
        return;
    }

    let path = PathBuf::from("logs").join("preloader-status.json");
    let _ = fs::create_dir_all("logs");
    if let Ok(data) = serde_json::to_vec_pretty(&status) {
        let _ = fs::write(path, data);
    }
}

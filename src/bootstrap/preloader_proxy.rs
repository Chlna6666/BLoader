use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::core::PCWSTR;

use crate::platform::{logging, mod_diagnostics, native_stdio};

const PRELOAD_TYPE: &str = "preloader-bootstrap";
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

pub fn has_priority_preloader(game_dir: &Path) -> bool {
    !discover_candidates(game_dir).is_empty()
}

pub unsafe fn try_load(game_dir: &Path) -> PreloaderProxySummary {
    if crate::platform::console::early_console_requested() {
        crate::platform::console::init_console_window_early();
        crate::platform::console::activate_console_logging();
        // The bridge initialized before third-party preloads. Publish its
        // bounded early records now, before native DLL output can overtake it.
        crate::bootstrap::xuser_bridge::publish_pending_logs();
    }

    // Capture native WriteConsoleA/W and console WriteFile output before
    // PreLoader and LeviLamina start their own logger threads. The console
    // hook observes writes without rebinding any third-party CRT FILE*.
    // The latest.log relay remains an independent fallback for file-only logs.
    if logging::console_is_ready() {
        crate::platform::console_capture::install();
    }

    let started = Instant::now();
    let mut candidates = discover_candidates(game_dir);
    candidates.sort_by(|a, b| a.path.cmp(&b.path));

    if candidates.is_empty() {
        let dir = game_dir.join("mods").join("PreLoader");
        if dir.join(PRELOADER_DLL).is_file() {
            logging::scoped_warn_message(
                "preloader",
                "PreLoader.dll exists but BLoader metadata is missing/invalid: expected mods/PreLoader/bloader.json type=preloader-bootstrap. DLL untouched.",
            );
        }
        logging::write_bootstrap_marker("preloader.early.not_found");
        return PreloaderProxySummary {
            discovered: 0,
            state: PreloaderProxyState::NotFound,
        };
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
        let resolved = canonical_or_original(&crate::platform::utils::get_module_path(module.0 as usize));
        if paths_equivalent(&expected, &resolved) {
            mod_diagnostics::mark_loaded(&identity, module.0 as usize, "preloader_preexisting");
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
        return PreloaderProxySummary {
            discovered,
            state: PreloaderProxyState::Failed,
        };
    }

    // PreLoader owns its CRT streams and crash handlers. Do not swap stdout/
    // stderr before LoadLibraryW: the global rebinding caused startup failures
    // and offers no reliable interception of WriteConsoleW in any case.
    mod_diagnostics::mark_loading(&identity, "preloader_priority_early");
    logging::write_bootstrap_marker(&format!(
        "preloader.priority.load.begin path={} capture=noninvasive-log-relay",
        expected.display()
    ));

    let load_result = {
        let _scope = mod_diagnostics::enter_scope(&identity, "LoadLibrary:preloader_priority_early");
        unsafe {
            native_stdio::capture_library_load(&identity, "preloader_priority_early", || {
                let wide = wide_null(expected.as_os_str());
                LoadLibraryW(PCWSTR(wide.as_ptr()))
            })
        }
    };
    // Preserve the runtime-owned unhandled exception filter installed by
    // PreLoader/LeviLamina/CrashLogger rather than overriding it here.

    match load_result {
        Ok(module) => {
            let resolved = canonical_or_original(&crate::platform::utils::get_module_path(module.0 as usize));
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
                return PreloaderProxySummary {
                    discovered,
                    state: PreloaderProxyState::Failed,
                };
            }

            mod_diagnostics::mark_loaded(&identity, module.0 as usize, "preloader_priority_early");
            logging::scoped_info_message(
                "preloader",
                &format!(
                    "Priority preload active | module=0x{:X} | elapsed={}ms | BLoader direct preload pass disabled",
                    module.0 as usize,
                    started.elapsed().as_millis()
                ),
            );
            // Loaded != captured: the line above is BLoader's own lifecycle
            // event, not a message emitted by PreLoader. Report the actual
            // observer state and the presence of LeviLamina's module without
            // claiming to own its Mod registry or CRT streams.
            logging::scoped_info_message(
                "preloader",
                &format!(
                    "External loader observation | LeviLamina.dll={} | native_console_capture={} | file_log_relay={} | external_mods=delegated",
                    if module_by_name("LeviLamina.dll").is_some() {
                        "present"
                    } else {
                        "not_detected_at_preload_return"
                    },
                    if crate::platform::console_capture::is_active() {
                        "active"
                    } else {
                        "inactive"
                    },
                    if logging::console_is_ready() {
                        "logs/latest.log"
                    } else {
                        "waiting_for_console"
                    },
                ),
            );
            logging::write_bootstrap_marker(&format!(
                "preloader.priority.active module=0x{:X} path={} elapsed_ms={} delegate_remaining=true phase=bootstrap-thread-first",
                module.0 as usize,
                resolved.display(),
                started.elapsed().as_millis()
            ));
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
                    "Priority preload failed | path={} | {} | native runtime chain will NOT be retried by BLoader",
                    expected.display(),
                    detail
                ),
            );
            logging::write_bootstrap_marker(&format!(
                "preloader.priority.failed path={} detail={} phase=bootstrap-thread-first",
                expected.display(),
                detail
            ));
            PreloaderProxySummary {
                discovered,
                state: PreloaderProxyState::Failed,
            }
        }
    }
}

/// BLoader directly owns the bootstrap at mods/PreLoader/PreLoader.dll.
/// Its BLoader-only metadata type is *preloader-bootstrap*. The descriptor
/// lives in bloader.json, invisible to PreLoader/LeviLamina's manifest.json
/// scanners. Never move or delete PreLoader.dll during startup.
fn discover_candidates(game_dir: &Path) -> Vec<Candidate> {
    let dir = game_dir.join("mods").join("PreLoader");
    let path = dir.join(PRELOADER_DLL);
    if !path.is_file() {
        return Vec::new();
    }
    // Only BLoader-owned metadata can authorize loading PreLoader.dll.
    // LeviLamina's manifest.json must never be accepted as a bootstrap
    // fallback, even if it happens to contain an old BLoader type string.
    let manifest = fs::read_to_string(dir.join("bloader.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let is_bootstrap = manifest.as_ref().is_some_and(|manifest| {
        manifest.get("type").and_then(serde_json::Value::as_str) == Some(PRELOAD_TYPE)
            && manifest.get("entry").and_then(serde_json::Value::as_str) == Some(PRELOADER_DLL)
            && manifest.get("name").and_then(serde_json::Value::as_str) == Some("PreLoader")
    });
    if !is_bootstrap {
        return Vec::new();
    }
    let version = manifest
        .as_ref()
        .and_then(|manifest| manifest.get("version"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    vec![Candidate {
        id: "PreLoader".to_string(),
        name: "PreLoader".to_string(),
        version,
        path,
        log_aliases: Vec::new(),
    }]
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

// src/lib.rs
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::panic::{self, PanicHookInfo};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

use crate::platform::logging;

mod bootstrap;
mod config;
mod cosmetics;
mod game;
mod loader;
mod platform;

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(
    hinstance: HINSTANCE,
    call_reason: u32,
    reserved: *const c_void,
) -> i32 {
    match call_reason {
        DLL_PROCESS_ATTACH => {
            crate::platform::utils::set_loader_module_handle(hinstance.0 as usize);
            crate::platform::utils::set_exe_cwd();
            crate::platform::logging::note_process_start();
            crate::platform::crash_report::install_early();

            let static_process_attach = !reserved.is_null();
            let gate_armed = crate::bootstrap::pre_main_gate::install_for_process_start(static_process_attach);
            crate::platform::logging::write_bootstrap_marker(&format!(
                "dllmain.premain-gate static_attach={} armed={} mode=inline-single-thread",
                static_process_attach, gate_armed
            ));

            let runtime_kind = crate::platform::runtime_environment::prime_detection();
            crate::platform::logging::write_bootstrap_marker(&format!(
                "runtime.environment.early kind={} wine={}",
                runtime_kind.as_str(), runtime_kind.is_wine(),
            ));
            crate::platform::logging::write_bootstrap_marker(&format!(
                "bloader.build version={} profile={} xuser_bridge=embedded protocol=1",
                crate::platform::build_info::VERSION,
                crate::platform::build_info::PROFILE,
            ));
            crate::platform::logging::write_bootstrap_marker(
                "dllmain.xuser_bridge.deferred target=premain-after-loader-lock",
            );

            if gate_armed {
                crate::platform::logging::write_bootstrap_marker(
                    "dllmain.bootstrap.deferred target=minecraft-startup-thread-oep",
                );
            } else if static_process_attach {
                crate::platform::logging::write_bootstrap_marker(
                    "dllmain.bootstrap.fatal reason=static-attach-premain-gate-unavailable",
                );
                return 0;
            } else {
                crate::platform::logging::write_bootstrap_marker(
                    "dllmain.bootstrap.fallback target=bloader-late-attach reason=no-static-oep-gate",
                );
                match thread::Builder::new()
                    .name("bloader-late-attach".to_string())
                    .spawn(run_bootstrap_late_attach)
                {
                    Ok(_) => logging::write_bootstrap_marker("bootstrap.late_attach.spawn.ok"),
                    Err(error) => logging::write_bootstrap_marker(&format!(
                        "bootstrap.late_attach.spawn.failed error={error}"
                    )),
                }
            }
        }
        DLL_PROCESS_DETACH => {
            logging::write_bootstrap_marker("bootstrap.process_detach");
        }
        _ => {}
    }
    1
}

// Legacy ABI compatibility exports. Older BL Mods resolve these symbols at load
// time; keep them even though the panel renderer and BL host were removed.
#[unsafe(export_name = "bl_i18n_current_locale")]
pub unsafe extern "system" fn bl_i18n_current_locale(out_buf: *mut u8, out_len: usize) -> usize {
    let value = crate::platform::i18n::current_locale();
    let bytes = value.as_bytes();
    if !out_buf.is_null() && out_len > 0 {
        let copy_len = bytes.len().min(out_len - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, copy_len);
            *out_buf.add(copy_len) = 0;
        }
    }
    bytes.len()
}

#[unsafe(export_name = "bl_register_d3d12_render_callback")]
pub unsafe extern "system" fn bl_register_d3d12_render_callback(_callback: *const c_void) {
    // Renderer service is not compiled in the lightweight build.
}

pub(crate) fn run_bootstrap_on_startup_thread() -> bool {
    crate::platform::logging::write_bootstrap_marker(
        "bootstrap.inline.start execution=minecraft-startup-thread",
    );
    run_bootstrap_sequence("oep-gated-startup-thread")
}

fn run_bootstrap_late_attach() {
    crate::platform::logging::write_bootstrap_marker(
        "bootstrap.thread.start execution=late-attach-worker",
    );
    crate::bootstrap::runtime_ready::mark_oep_released("late-attach");
    let _ = run_bootstrap_sequence("late-attach-worker");
}

fn run_bootstrap_sequence(execution_mode: &'static str) -> bool {
    // Load the launch policy before any third-party DLL, including PreLoader.
    // The same immutable startup snapshot governs both OEP preloads and normal mods.
    let config = config::Config::load();
    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.config.early_loaded disable_mod_loading={} cosmetics={}",
        config.disable_mod_loading, config.enable_cosmetics
    ));
    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.xuser_bridge.premain.begin execution={execution_mode} loader_lock=released"
    ));
    crate::bootstrap::xuser_bridge::initialize_before_mods();
    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.xuser_bridge.premain.done execution={execution_mode} loader_lock=released"
    ));

    // A disabled mod chain must not hide the early diagnostic console.
    if config.enable_debug_console {
        unsafe {
            crate::platform::console::init_console_window_early();
            crate::platform::console::activate_console_logging();
        }
        crate::bootstrap::xuser_bridge::publish_pending_logs();
    }

    crate::platform::crash_report::install();

    // LeviLamina owns its own async logger and exception/crash hooks.
    // Avoid process-wide OutputDebugString interception when delegating to
    // PreLoader; standalone BLoader loading retains the compatibility hook.
    let preloader_present = crate::bootstrap::preloader_proxy::has_priority_preloader(
        &crate::platform::utils::get_exe_directory(),
    );
    if !preloader_present {
        unsafe { crate::game::game_log::install(); }
    } else {
        crate::platform::logging::write_bootstrap_marker(
            "bootstrap.game_log.delegated owner=PreLoader transport=file-relay",
        );
    }

    let premain_result = panic::catch_unwind(|| unsafe { prepare_premain_preloads(execution_mode, &config) });
    let (preloader_summary, loaded_summary) = match premain_result {
        Ok(summary) => summary,
        Err(panic_payload) => {
            let details = panic_payload_to_string(panic_payload.as_ref());
            crate::platform::logging::write_bootstrap_marker(&format!(
                "bootstrap.premain.panic execution={execution_mode} {details}"
            ));
            report_bootstrap_panic(&details, execution_mode);
            return false;
        }
    };

    if let Err(panic_payload) = panic::catch_unwind(|| unsafe {
        bootstrap(preloader_summary, loaded_summary, execution_mode, config)
    }) {
        let details = panic_payload_to_string(panic_payload.as_ref());
        report_bootstrap_panic(&details, execution_mode);
        return false;
    }

    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.sequence.complete execution={execution_mode}"
    ));
    true
}

unsafe fn prepare_premain_preloads(
    execution_mode: &str,
    config: &config::Config,
) -> (
    crate::bootstrap::preloader_proxy::PreloaderProxySummary,
    crate::loader::NativePreloadSummary,
) {
    if config.disable_mod_loading {
        crate::platform::logging::write_bootstrap_marker(
            "bootstrap.mods.skipped phase=pre-main reason=disable_mod_loading",
        );
        return (
            crate::bootstrap::preloader_proxy::PreloaderProxySummary {
                discovered: 0,
                state: crate::bootstrap::preloader_proxy::PreloaderProxyState::NotFound,
            },
            crate::loader::NativePreloadSummary::default(),
        );
    }
    let game_dir = crate::platform::utils::get_exe_directory();
    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.preloader.premain.begin phase={execution_mode}"
    ));

    let preloader_summary = crate::bootstrap::preloader_proxy::try_load(&game_dir);
    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.preloader.premain.done discovered={} state={:?} execution={execution_mode}",
        preloader_summary.discovered, preloader_summary.state,
    ));

    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.native_preload.premain.begin execution={execution_mode}"
    ));
    let loaded_summary = if preloader_summary.active() {
        crate::platform::logging::write_bootstrap_marker(
            "bootstrap.native_preload.premain.delegated owner=PreLoader",
        );
        crate::loader::NativePreloadSummary::default()
    } else {
        if preloader_summary.failed() {
            crate::platform::logging::write_bootstrap_marker(
                "bootstrap.native_preload.premain.fallback reason=preloader-failed",
            );
        }
        crate::loader::load_native_preloads_in_dllmain(&game_dir)
    };
    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.native_preload.premain.done discovered={} attempted={} verified={} failed={} required_failed={} execution={execution_mode}",
        loaded_summary.discovered,
        loaded_summary.attempted,
        loaded_summary.verified,
        loaded_summary.failed,
        loaded_summary.required_failed,
    ));

    (preloader_summary, loaded_summary)
}

unsafe fn bootstrap(
    preloader_summary: crate::bootstrap::preloader_proxy::PreloaderProxySummary,
    loaded_summary: crate::loader::NativePreloadSummary,
    execution_mode: &'static str,
    config: config::Config,
) {
    let start_time = Instant::now();
    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.begin execution={execution_mode}"
    ));

    config::ensure_config_watcher();
    config::Config::apply_update(&config);
    crate::platform::logging::write_bootstrap_marker("bootstrap.config.loaded");
    crate::platform::logging::init(&config.log_level);
    crate::platform::logging::write_bootstrap_marker("bootstrap.logging.ready");
    crate::platform::crash_report::spawn_external_logger(crate::platform::utils::loader_module_handle());
    crate::platform::i18n::init(&config);
    crate::platform::logging::write_bootstrap_marker("bootstrap.i18n.ready");

    logging::info_message(
        "Minecraft symbol subsystem: disabled (not compiled in lightweight build).",
    );

    if execution_mode == "oep-gated-startup-thread" {
        schedule_post_oep_runtime_io(config.enable_debug_console, preloader_summary.active());
    } else {
        if config.enable_debug_console {
            unsafe { crate::platform::console::init_console(); }
            crate::platform::logging::write_bootstrap_marker("bootstrap.console.ready");
        }
        if !preloader_summary.active() {
            crate::platform::native_stdio::install_process_capture();
        }
    }

    crate::platform::native_stdio::flush_pending();
    if !preloader_summary.active() {
        unsafe { crate::game::game_log::install(); }
    }
    crate::bootstrap::xuser_bridge::publish_pending_logs();
    setup_panic_hook();
    crate::platform::runtime_environment::log_summary();

    const PKG_NAME: &str = crate::platform::build_info::NAME;
    const VERSION: &str = crate::platform::build_info::VERSION;
    let application_path = crate::platform::utils::get_exe_path();
    let application_name = application_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unknown>");
    let application_version =
        crate::platform::utils::current_application_version().unwrap_or_else(|| "unknown".to_string());

    let current_locale = crate::platform::i18n::current_locale();
    logging::startup_banner(
        PKG_NAME, VERSION, application_name, &application_version, &current_locale,
    );
    logging::info_message(&format!(
        "Host application: {} v{}",
        application_name, application_version
    ));
    logging::info_message(&format!("Path: {}", application_path.display()));
    logging::info_message(
        "Runtime profile: lightweight (panel=off, ArcUI=not-compiled, symbols=not-compiled, i18n=embedded)",
    );
    logging::scoped_info_message(
        "bootstrap",
        &format!("Immediate startup execution mode: {execution_mode}"),
    );

    match preloader_summary.state {
        crate::bootstrap::preloader_proxy::PreloaderProxyState::Loaded => logging::scoped_info_message(
            "preloader",
            "PreLoader and its delegated preload chain completed synchronously before Minecraft OEP execution.",
        ),
        crate::bootstrap::preloader_proxy::PreloaderProxyState::Failed => logging::scoped_warn_message(
            "preloader",
            "Priority PreLoader failed; BLoader completed the legacy native preload fallback before Minecraft OEP execution.",
        ),
        crate::bootstrap::preloader_proxy::PreloaderProxyState::NotFound => logging::scoped_debug_message(
            "preloader",
            "No type=preload PreLoader.dll package was found; legacy native preloads were handled before Minecraft OEP execution.",
        ),
    }

    logging::scoped_info_message(
        "premain-gate",
        &format!(
            "Critical preload phase completed before Minecraft main | execution={} native_discovered={} attempted={} verified={} failed={} required_failed={}",
            execution_mode,
            loaded_summary.discovered,
            loaded_summary.attempted,
            loaded_summary.verified,
            loaded_summary.failed,
            loaded_summary.required_failed,
        ),
    );

    let game_dir = crate::platform::utils::get_exe_directory();
    crate::game::launch_info::install(&game_dir);
    let native_preload_summary = crate::loader::publish_native_preload_reports();
    let native_load_failure_message = crate::loader::required_native_failure_message();
    let native_load_success_message = crate::loader::native_success_notification_message();
    if native_load_failure_message.is_some() {
        crate::platform::logging::scoped_error_message(
            "native-loader",
            &format!(
                "required native preload verification failed | count={}",
                native_preload_summary.required_failed
            ),
        );
    }

    if let Some(message) = native_load_failure_message {
        let _ = thread::Builder::new()
            .name("bloader-native-load-error".to_string())
            .spawn(move || {
                crate::platform::error_dialog::show_native_load_error(
                    "BLoader Native Module Load Failed", &message,
                );
            });
    } else if let Some(message) = native_load_success_message {
        let _ = thread::Builder::new()
            .name("bloader-native-load-success".to_string())
            .spawn(move || {
                crate::platform::error_dialog::show_native_load_success(
                    "BLoader Native Module Loaded", &message,
                );
            });
    }

    logging::scoped_info_message(
        "runtime-ready",
        "Delayed Mod readiness uses OEP/process/window stability only; no D3D/DXGI device, swapchain, Present, or dummy HWND hook is installed.",
    );

    logging::info_message(&format!(
        "{} v{} initialized. {}",
        PKG_NAME, VERSION, crate::platform::i18n::tr("bootstrap.start")
    ));
    logging::info_message(&format!(
        "Locale={}",
        crate::platform::i18n::current_locale()
    ));

    if crate::platform::file_io_policy::writes_allowed() {
        crate::game::file_redirection::install(&config, &game_dir);
    } else {
        logging::scoped_info_message(
            "file-redirection",
            "Legacy UWP read-only mode: file redirection hooks are disabled to prevent target directory/file creation.",
        );
    }
    crate::game::network_hook::install(&config);
    // 内置披风（Cosmetics 移植）：OEP 前安装 WinHTTP 钩子，
    // 覆盖启动期 marketplace 请求；内部按 enable_cosmetics 门控。
    crate::cosmetics::install(&config, &game_dir);

    crate::platform::logging::write_bootstrap_marker(
        "bootstrap.queue_hook.skipped_for_hudhook",
    );

    if config.disable_mod_loading {
        logging::info_message("Mod loading disabled by config.");
    } else {
        crate::platform::logging::write_bootstrap_marker(&format!(
            "bootstrap.mods.load.begin execution={execution_mode} serial=true"
        ));
        crate::loader::load_mods(&game_dir, preloader_summary.active());
        crate::platform::logging::write_bootstrap_marker(&format!(
            "bootstrap.mods.load.done execution={execution_mode} serial=true"
        ));
    }

    let elapsed = start_time.elapsed();
    logging::info_message(&format!(
        "{} ({:.3}s).",
        crate::platform::i18n::tr("bootstrap.complete"),
        elapsed.as_secs_f64()
    ));

    if config.enable_debug_console {
        crate::platform::console::start_input_listener();
    }
}

const POST_OEP_RUNTIME_IO_DELAY_MS: u64 = 1_500;

fn schedule_post_oep_runtime_io(enable_debug_console: bool, preloader_active: bool) {
    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.runtime_io.deferred console=after-oep-immediate stdio=oep+{}ms console_enabled={}",
        POST_OEP_RUNTIME_IO_DELAY_MS, enable_debug_console,
    ));

    match thread::Builder::new()
        .name("bloader-post-oep-runtime-io".to_string())
        .spawn(move || {
            if !crate::bootstrap::runtime_ready::wait_for(
                crate::bootstrap::runtime_ready::ReadyLevel::Process,
                Duration::from_secs(10),
            ) {
                crate::platform::logging::write_bootstrap_marker(
                    "post-oep.runtime_io.skipped reason=oep-release-timeout",
                );
                return;
            }

            if enable_debug_console {
                unsafe { crate::platform::console::init_console(); }
                crate::platform::logging::write_bootstrap_marker(
                    "post-oep.console.ready delay_ms=0 stdio_pending=true",
                );
            }

            crate::bootstrap::runtime_ready::wait_until_oep_delay(POST_OEP_RUNTIME_IO_DELAY_MS);

            // LeviLamina/PreLoader owns its own CRT stdout and stderr. The
            // process-wide _dup2/_setvbuf/_flushall binding can race with
            // asynchronous native logger threads and corrupt their streams.
            // Use the external latest.log relay instead in delegated mode.
            if !preloader_active {
                unsafe { crate::platform::native_stdio::install_process_capture(); }
                crate::platform::native_stdio::flush_pending();
            }
            // Leave third-party CrashLogger/LeviLamina exception ownership in
            // place. Force-rearming the BLoader top-level filter hijacks it.
            crate::platform::logging::write_bootstrap_marker(&format!(
                "post-oep.runtime_io.ready delay_ms={} console={} stdio={}",
                POST_OEP_RUNTIME_IO_DELAY_MS, enable_debug_console,
                if preloader_active { "delegated-to-PreLoader" } else { "process-crt" },
            ));
        }) {
        Ok(_) => crate::platform::logging::write_bootstrap_marker(
            "bootstrap.runtime_io.defer.spawn.ok",
        ),
        Err(error) => crate::platform::logging::write_bootstrap_marker(&format!(
            "bootstrap.runtime_io.defer.spawn.failed error={error}"
        )),
    }
}

fn report_bootstrap_panic(details: &str, execution_mode: &str) {
    let message = format!("BLoader failed during startup.\n\n{details}");
    crate::platform::logging::emergency_error_message(
        "loader",
        &format!("Bootstrap panic execution={execution_mode}: {details}"),
    );
    crate::platform::logging::write_bootstrap_marker(&format!(
        "bootstrap.panic execution={execution_mode} {details}"
    ));
    crate::platform::error_dialog::show_fatal_error("BLoader Startup Error", &message);
}

fn setup_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let error_msg = panic_info_to_string(info);
        crate::platform::crash_report::capture_rust_panic(&error_msg, true);
        logging::emergency_error_message("loader", &format!("FATAL PANIC: {}", error_msg));
    }));
}

fn panic_info_to_string(info: &PanicHookInfo<'_>) -> String {
    let msg = panic_payload_to_string(info.payload());
    let location = info
        .location()
        .map(|location| format!("{}:{}", location.file(), location.line()))
        .unwrap_or_else(|| "<unknown location>".to_string());
    format!("{msg} at {location}")
}

fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "Unknown panic".to_string()
}

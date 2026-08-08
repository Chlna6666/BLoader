// src/lib.rs
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::panic::{self, PanicHookInfo};
use std::thread;
use std::time::Instant;

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

use crate::runtime::foundation::logging;

mod bl;
mod bl_dummy_hwnd;
mod config;
mod core;
mod runtime;
mod utils;

pub use crate::bl::host::{bl_i18n_current_locale, bl_i18n_tr, bl_register_mod_lang};
pub use crate::core::d3d12_queue::bl_register_d3d12_render_callback;

#[unsafe(no_mangle)]
pub extern "system" fn bl_camera_zoom_set_enabled(_enabled: bool) -> bool {
    false
}
#[unsafe(no_mangle)]
pub extern "system" fn bl_camera_zoom_set_percent(_percent: u32) -> bool {
    false
}
#[unsafe(no_mangle)]
pub extern "system" fn bl_camera_zoom_get_enabled() -> bool {
    false
}
#[unsafe(no_mangle)]
pub extern "system" fn bl_camera_zoom_get_percent() -> u32 {
    0
}
#[unsafe(no_mangle)]
pub extern "system" fn bl_gamma_set_enabled(_enabled: bool) -> bool {
    false
}
#[unsafe(no_mangle)]
pub extern "system" fn bl_gamma_set_value(_value: f32) -> bool {
    false
}
#[unsafe(no_mangle)]
pub extern "system" fn bl_gamma_get_enabled() -> bool {
    false
}
#[unsafe(no_mangle)]
pub extern "system" fn bl_gamma_get_value() -> f32 {
    1.0
}
#[unsafe(no_mangle)]
pub extern "system" fn bl_render3d_ready() -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "system" fn bl_render3d_line(
    _level_render: usize,
    _screen_context: usize,
    _x0: f32,
    _y0: f32,
    _z0: f32,
    _x1: f32,
    _y1: f32,
    _z1: f32,
    _r: f32,
    _g: f32,
    _b: f32,
    _a: f32,
) -> bool {
    false
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(
    hinstance: HINSTANCE,
    call_reason: u32,
    reserved: *const c_void,
) -> i32 {
    match call_reason {
        DLL_PROCESS_ATTACH => {
            utils::set_exe_cwd();
            utils::set_loader_module_handle(hinstance.0 as usize);
            runtime::foundation::crash_report::install_early();

            // BMCBL adds BLoader.dll to Minecraft's PE import table. Windows
            // documents lpvReserved != NULL for this static process-start load.
            // Arm the OEP gate while DllMain still runs, but do not LoadLibrary
            // any third-party code until the bootstrap thread starts after the
            // loader lock is released.
            let static_process_attach = !reserved.is_null();
            let gate_armed = core::pre_main_gate::install_for_process_start(static_process_attach);
            runtime::foundation::logging::write_bootstrap_marker(&format!(
                "dllmain.premain-gate static_attach={} armed={}",
                static_process_attach, gate_armed
            ));

            let runtime_kind = runtime::foundation::runtime_environment::prime_detection();
            runtime::foundation::logging::write_bootstrap_marker(&format!(
                "runtime.environment.early kind={} wine={}",
                runtime_kind.as_str(),
                runtime_kind.is_wine(),
            ));
            runtime::foundation::logging::write_bootstrap_marker(&format!(
                "bloader.build version={} profile={} xuser_bridge=embedded protocol=1",
                runtime::foundation::build_info::VERSION,
                runtime::foundation::build_info::PROFILE,
            ));

            runtime::foundation::logging::write_bootstrap_marker(
                "dllmain.xuser_bridge.begin mode=pipe_gated",
            );
            // BMCBL creates the PID-scoped pipe before resuming the process.
            // No pipe means this performs one immediate failed open and returns
            // without loading xgameruntime or installing MinHook. A valid pipe
            // is consumed synchronously so QueryApiImpl is ready before the
            // executable can enter its normal GDK initialization path.
            core::xuser_bridge::initialize_before_mods();
            runtime::foundation::logging::write_bootstrap_marker(
                "dllmain.xuser_bridge.done",
            );

            runtime::foundation::logging::write_bootstrap_marker(
                "dllmain.preloader.deferred target=bloader-bootstrap-premain",
            );

            match thread::Builder::new()
                .name("bloader-bootstrap".to_string())
                .spawn(run_bootstrap_with_error_dialog)
            {
                Ok(_) => logging::write_bootstrap_marker("bootstrap.thread.spawn.ok"),
                Err(error) => {
                    logging::write_bootstrap_marker(&format!(
                        "bootstrap.thread.spawn.failed error={error}"
                    ));
                    // Nothing will perform pre-main initialization now. Release
                    // the gate so a thread-creation failure does not permanently
                    // deadlock Minecraft at its entry point.
                    core::pre_main_gate::release("bootstrap-thread-spawn-failed");
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

fn run_bootstrap_with_error_dialog() {
    runtime::foundation::logging::write_bootstrap_marker("bootstrap.thread.start");
    runtime::foundation::crash_report::install();

    // All loader work that must happen before Minecraft executes its original
    // entry point belongs in this first phase. The startup thread is parked at
    // the OEP INT3 gate while this thread is free of the Windows loader lock.
    let premain_result = panic::catch_unwind(|| unsafe { prepare_premain_preloads() });
    let (preloader_summary, loaded_summary) = match premain_result {
        Ok(summary) => summary,
        Err(panic_payload) => {
            let details = panic_payload_to_string(panic_payload.as_ref());
            runtime::foundation::logging::write_bootstrap_marker(&format!(
                "bootstrap.premain.panic {details}"
            ));
            core::pre_main_gate::release("premain-rust-panic");
            report_bootstrap_panic(&details);
            return;
        }
    };

    // Critical preload initialization is complete. Only now may Minecraft's
    // primary thread execute its original first instruction.
    core::pre_main_gate::release("critical-preloads-ready");

    if let Err(panic_payload) =
        panic::catch_unwind(|| unsafe { bootstrap(preloader_summary, loaded_summary) })
    {
        let details = panic_payload_to_string(panic_payload.as_ref());
        report_bootstrap_panic(&details);
    }
}

unsafe fn prepare_premain_preloads() -> (
    core::preloader_proxy::PreloaderProxySummary,
    core::loader::NativePreloadSummary,
) {
    let game_dir = utils::get_exe_directory();
    runtime::foundation::logging::write_bootstrap_marker(
        "bootstrap.preloader.premain.begin phase=oep-gated-bootstrap-thread",
    );

    // PreLoader is the first third-party DLL loaded after DllMain returns. Its
    // synchronous LoadLibrary call covers LeviLamina initialization while the
    // Minecraft startup thread remains blocked at the OEP gate.
    let preloader_summary = core::preloader_proxy::try_load(&game_dir);
    runtime::foundation::logging::write_bootstrap_marker(&format!(
        "bootstrap.preloader.premain.done discovered={} state={:?}",
        preloader_summary.discovered,
        preloader_summary.state,
    ));

    runtime::foundation::logging::write_bootstrap_marker(
        "bootstrap.native_preload.premain.begin",
    );
    let loaded_summary = if preloader_summary.active() {
        runtime::foundation::logging::write_bootstrap_marker(
            "bootstrap.native_preload.premain.delegated owner=PreLoader",
        );
        core::loader::NativePreloadSummary::default()
    } else {
        if preloader_summary.failed() {
            runtime::foundation::logging::write_bootstrap_marker(
                "bootstrap.native_preload.premain.fallback reason=preloader-failed",
            );
        }
        core::loader::load_native_preloads_in_dllmain(&game_dir)
    };
    runtime::foundation::logging::write_bootstrap_marker(&format!(
        "bootstrap.native_preload.premain.done discovered={} attempted={} verified={} failed={} required_failed={}",
        loaded_summary.discovered,
        loaded_summary.attempted,
        loaded_summary.verified,
        loaded_summary.failed,
        loaded_summary.required_failed,
    ));

    (preloader_summary, loaded_summary)
}

unsafe fn bootstrap(
    preloader_summary: core::preloader_proxy::PreloaderProxySummary,
    loaded_summary: core::loader::NativePreloadSummary,
) {
    let start_time = Instant::now();
    runtime::foundation::logging::write_bootstrap_marker("bootstrap.begin");

    let config = config::Config::load();
    config::ensure_config_watcher();
    config::Config::apply_update(&config);
    runtime::foundation::logging::write_bootstrap_marker("bootstrap.config.loaded");
    runtime::foundation::logging::init(&config.log_level);
    runtime::foundation::logging::write_bootstrap_marker("bootstrap.logging.ready");
    runtime::foundation::crash_report::spawn_external_logger(utils::loader_module_handle());
    runtime::foundation::i18n::init(&config);
    runtime::foundation::logging::write_bootstrap_marker("bootstrap.i18n.ready");

    logging::info_message("Minecraft symbol subsystem: disabled (not compiled in lightweight build).");

    if config.enable_debug_console {
        core::console::init_console();
        runtime::foundation::logging::write_bootstrap_marker("bootstrap.console.ready");
    }

    // The process-wide capture is installed only after all pre-main loaders have
    // completed and Minecraft's OEP has been released. PreLoader itself is never
    // wrapped in the temporary per-library CRT redirection path.
    runtime::foundation::native_stdio::install_process_capture();
    runtime::foundation::native_stdio::flush_pending();
    core::xuser_bridge::publish_pending_logs();
    setup_panic_hook();
    runtime::foundation::runtime_environment::log_summary();

    const PKG_NAME: &str = crate::runtime::foundation::build_info::NAME;
    const VERSION: &str = crate::runtime::foundation::build_info::VERSION;
    let application_path = utils::get_exe_path();
    let application_name = application_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unknown>");
    let application_version = utils::current_application_version()
        .unwrap_or_else(|| "unknown".to_string());

    let current_locale = runtime::foundation::i18n::current_locale();
    logging::startup_banner(
        PKG_NAME,
        VERSION,
        application_name,
        &application_version,
        &current_locale,
    );
    logging::info_message(&format!(
        "Host application: {} v{} | {}",
        application_name,
        application_version,
        application_path.display()
    ));
    logging::info_message(
        "Runtime profile: lightweight | panel=off | ArcUI=not-compiled | symbols=not-compiled | i18n=embedded",
    );

    match preloader_summary.state {
        core::preloader_proxy::PreloaderProxyState::Loaded => {
            logging::scoped_info_message(
                "preloader",
                "PreLoader and its delegated preload chain completed behind the Pre-Main Gate before Minecraft OEP execution.",
            );
        }
        core::preloader_proxy::PreloaderProxyState::Failed => {
            logging::scoped_warn_message(
                "preloader",
                "Priority PreLoader failed; BLoader completed the legacy native preload fallback before releasing Minecraft OEP.",
            );
        }
        core::preloader_proxy::PreloaderProxyState::NotFound => {
            logging::scoped_debug_message(
                "preloader",
                "No type=preload PreLoader.dll package was found; legacy native preloads were handled before Minecraft OEP release.",
            );
        }
    }

    logging::scoped_info_message(
        "premain-gate",
        &format!(
            "Critical preload phase completed before Minecraft main | native_discovered={} attempted={} verified={} failed={} required_failed={}",
            loaded_summary.discovered,
            loaded_summary.attempted,
            loaded_summary.verified,
            loaded_summary.failed,
            loaded_summary.required_failed,
        ),
    );

    let game_dir = utils::get_exe_directory();
    let native_preload_summary = core::loader::publish_native_preload_reports();
    let native_load_failure_message = core::loader::required_native_failure_message();
    let native_load_success_message = core::loader::native_success_notification_message();
    if native_load_failure_message.is_some() {
        runtime::foundation::logging::scoped_error_message(
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
                runtime::foundation::error_dialog::show_native_load_error(
                    "BLoader Native Module Load Failed",
                    &message,
                );
            });
    } else if let Some(message) = native_load_success_message {
        let _ = thread::Builder::new()
            .name("bloader-native-load-success".to_string())
            .spawn(move || {
                runtime::foundation::error_dialog::show_native_load_success(
                    "BLoader Native Module Loaded",
                    &message,
                );
            });
    }

    if !core::render_signal::install() {
        logging::warn_message("Graphics readiness hook unavailable; hot Mods will use window fallback.");
    }

    logging::info_message(&format!(
        "{} v{} initialized. {}",
        PKG_NAME,
        VERSION,
        runtime::foundation::i18n::tr("bootstrap.start")
    ));
    logging::info_message(&format!(
        "Locale={} | {}",
        runtime::foundation::i18n::current_locale(),
        runtime::foundation::i18n::tr("ui.pipeline.external")
    ));

    core::file_redirection::install(&config, &game_dir);
    core::network_hook::install(&config);

    runtime::foundation::logging::write_bootstrap_marker(
        "bootstrap.queue_hook.skipped_for_hudhook",
    );

    if config.disable_mod_loading {
        logging::info_message("Mod loading disabled by config.");
    } else {
        runtime::foundation::logging::write_bootstrap_marker("bootstrap.mods.load.begin");
        core::loader::load_mods(&game_dir);
        runtime::foundation::logging::write_bootstrap_marker("bootstrap.mods.load.done");
        bl::host::dispatch_bootstrap_complete();
    }

    let elapsed = start_time.elapsed();
    logging::info_message(&format!(
        "{} ({:.3}s).",
        runtime::foundation::i18n::tr("bootstrap.complete"),
        elapsed.as_secs_f64()
    ));

    if config.enable_debug_console {
        core::console::start_input_listener();
    }
}

fn report_bootstrap_panic(details: &str) {
    let message = format!("BLoader failed during startup.\n\n{details}");
    runtime::foundation::logging::emergency_error_message(
        "loader",
        &format!("Bootstrap thread panic: {details}"),
    );
    runtime::foundation::logging::write_bootstrap_marker(&format!(
        "bootstrap.thread.panic {details}"
    ));
    runtime::foundation::error_dialog::show_fatal_error("BLoader Startup Error", &message);
}

fn setup_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let error_msg = panic_info_to_string(info);
        runtime::foundation::crash_report::capture_rust_panic(&error_msg, true);
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

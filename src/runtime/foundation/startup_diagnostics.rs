use std::env;

use crate::runtime::foundation::{
    build_info, file_io_policy, logging, runtime_environment,
};
use crate::utils;

pub fn emit(configured_level: &str, effective_level: &str) {
    let exe_path = utils::get_exe_path();
    let exe_name = exe_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unknown>");
    let host_version = file_io_policy::host_version().unwrap_or("unknown");
    let loader_handle = utils::loader_module_handle();
    let loader_path = utils::get_module_path(loader_handle);
    let cwd = env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| format!("<unavailable:{error}>"));
    let sink_summary = if file_io_policy::writes_allowed() {
        "latest.log+archive+console+OutputDebugString"
    } else {
        "console+OutputDebugString (file sinks disabled)"
    };
    let runtime_kind = runtime_environment::prime_detection();

    logging::scoped_info_message(
        "identity",
        &format!(
            "{} v{} | {}",
            build_info::NAME,
            build_info::VERSION,
            build_info::DESCRIPTION,
        ),
    );
    logging::scoped_info_message(
        "identity",
        &format!(
            "license={} | open_source=true | repository={}",
            build_info::LICENSE,
            build_info::REPOSITORY,
        ),
    );
    logging::scoped_info_message(
        "host",
        &format!(
            "application={} | version={} | runtime={} | file_io={}",
            exe_name,
            host_version,
            runtime_kind.as_str(),
            file_io_policy::mode_label(),
        ),
    );

    logging::scoped_debug_message(
        "build",
        &format!(
            "profile={} | runtime_profile={} | mode={} | target={} | arch={} | env={} | opt_level={} | debug_info={}",
            build_info::BUILD_PROFILE,
            build_info::PROFILE,
            build_info::build_mode(),
            build_info::BUILD_TARGET,
            build_info::BUILD_TARGET_ARCH,
            build_info::BUILD_TARGET_ENV,
            build_info::BUILD_OPT_LEVEL,
            build_info::BUILD_DEBUG_INFO,
        ),
    );
    logging::scoped_debug_message(
        "build",
        &format!(
            "rustc={} | git_commit={} | source_date_epoch={} | features={}",
            build_info::RUSTC_VERSION,
            build_info::GIT_COMMIT,
            build_info::SOURCE_DATE_EPOCH,
            build_info::enabled_features(),
        ),
    );
    logging::scoped_debug_message(
        "process",
        &format!(
            "pid={} | thread={:?} | cwd={} | loader_handle=0x{:X}",
            std::process::id(),
            std::thread::current().id(),
            cwd,
            loader_handle,
        ),
    );
    logging::scoped_debug_message(
        "paths",
        &format!(
            "exe={} | loader={} | loader_dir={}",
            exe_path.display(),
            if loader_path.as_os_str().is_empty() {
                "<unknown>".to_string()
            } else {
                loader_path.display().to_string()
            },
            utils::get_loader_directory().display(),
        ),
    );
    logging::scoped_debug_message(
        "logging",
        &format!(
            "configured_level={} | effective_level={} | sinks={} | file_writes_allowed={} | policy={}",
            configured_level,
            effective_level,
            sink_summary,
            file_io_policy::writes_allowed(),
            file_io_policy::mode_label(),
        ),
    );
    logging::scoped_debug_message(
        "capabilities",
        &format!(
            "crash_capture=VEH+SEH | external_crash_logger={} | native_stdio_capture={} | file_redirection={} | xuser_bridge=embedded | panel_ui={} | mc_symbols={}",
            file_io_policy::writes_allowed(),
            file_io_policy::writes_allowed(),
            file_io_policy::writes_allowed(),
            cfg!(feature = "panel-ui"),
            cfg!(feature = "mc-symbols"),
        ),
    );

    if file_io_policy::legacy_uwp_no_write() {
        logging::scoped_info_message(
            "compat",
            &format!(
                "legacy UWP compatibility active for Minecraft {}: BLoader file creation/modification is disabled; diagnostics remain available through console and OutputDebugString",
                host_version,
            ),
        );
    }

    logging::scoped_trace_message(
        "startup",
        "startup diagnostics emitted; subsequent TRACE records are enabled only when effective_level=trace",
    );
}

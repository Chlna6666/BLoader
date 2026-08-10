#[path = "logging.rs"]
mod inner;

pub use inner::{
    captured_mod_output, captured_process_output, debug_message, emergency_error_message,
    emergency_info_message, emergency_warn_message, error_message, info_message, is_ready,
    scoped_debug_message, scoped_error_message, scoped_info_message, scoped_trace_message,
    scoped_warn_message, set_console_handle, startup_banner, trace_message, warn_message,
    write_bootstrap_marker,
};

pub fn init(configured_level: &str) {
    let effective_level = if configured_level.trim().eq_ignore_ascii_case("trace") {
        "trace"
    } else {
        "debug"
    };

    inner::init(effective_level);
    inner::write_bootstrap_marker(&format!(
        "logging.validation.full-debug configured_level={} effective_level={} sinks=policy-selected",
        configured_level,
        effective_level,
    ));
    crate::runtime::foundation::startup_diagnostics::emit(configured_level, effective_level);
}

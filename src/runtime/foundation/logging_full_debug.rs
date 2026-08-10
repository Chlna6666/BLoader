#[path = "logging.rs"]
mod inner;

pub use inner::{
    captured_mod_output, captured_process_output, console_is_ready, debug_message,
    emergency_error_message, emergency_info_message, emergency_warn_message, error_message,
    info_message, is_ready, replay_console_message, scoped_debug_message, scoped_error_message,
    scoped_info_message, scoped_trace_message, scoped_warn_message, set_console_handle,
    startup_banner, trace_message, warn_message, write_bootstrap_marker,
};

pub fn init(configured_level: &str) {
    // Persistent diagnostics always keep at least DEBUG detail. The interactive
    // console independently follows the user's configured level so normal INFO
    // sessions stay as compact as a Java server console.
    inner::set_console_level(configured_level);
    let effective_level = if configured_level.trim().eq_ignore_ascii_case("trace") {
        "trace"
    } else {
        "debug"
    };

    inner::init(effective_level);
    inner::write_bootstrap_marker(&format!(
        "logging.validation.full-debug configured_level={} effective_level={} console_level={}",
        configured_level,
        effective_level,
        configured_level,
    ));
    crate::runtime::foundation::startup_diagnostics::emit(configured_level, effective_level);
}

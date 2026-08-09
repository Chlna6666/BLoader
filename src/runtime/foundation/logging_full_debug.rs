#[path = "logging.rs"]
mod inner;

pub use inner::{
    captured_mod_output, captured_process_output, debug_message, emergency_error_message,
    emergency_info_message, emergency_warn_message, error_message, info_message, is_ready,
    scoped_debug_message, scoped_error_message, scoped_info_message, scoped_trace_message,
    scoped_warn_message, set_console_handle, startup_banner, trace_message, warn_message,
    write_bootstrap_marker,
};

pub fn init(_configured_level: &str) {
    inner::init("debug");
    inner::write_bootstrap_marker(
        "logging.validation.full-debug configured_level=ignored effective_level=debug sinks=latest+archive+console+OutputDebugString",
    );
}

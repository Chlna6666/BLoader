use std::ffi::c_void;

use crate::runtime::foundation::logging;

type D3D12RenderCallback = unsafe extern "system" fn(
    device: *mut c_void,
    command_list: *mut c_void,
    back_buffer: *mut c_void,
    width: u32,
    height: u32,
);

type D3D11RenderCallback = unsafe extern "system" fn(
    device: *mut c_void,
    context: *mut c_void,
    back_buffer: *mut c_void,
    width: u32,
    height: u32,
);

/// The lightweight runtime retains the ABI symbol so older Mods fail safely,
/// but no panel/renderer service is linked and callbacks are not stored.
pub fn register_d3d12_render_callback(callback: D3D12RenderCallback) {
    logging::warn_message(&format!(
        "D3D12 render callback ignored: owner={} callback=0x{:X} (renderer service not compiled)",
        crate::bl::host::active_mod_name_for_registration(),
        callback as usize
    ));
}

pub fn register_d3d11_render_callback(callback: D3D11RenderCallback) {
    logging::warn_message(&format!(
        "D3D11 render callback ignored: owner={} callback=0x{:X} (renderer service not compiled)",
        crate::bl::host::active_mod_name_for_registration(),
        callback as usize
    ));
}

pub fn invoke_d3d12_render_callback(
    _device_ptr: *mut c_void,
    _command_list_ptr: *mut c_void,
    _back_buffer_ptr: *mut c_void,
    _width: u32,
    _height: u32,
) {
}

pub fn d3d12_callback_summaries() -> Vec<String> {
    Vec::new()
}

pub fn d3d11_callback_summaries() -> Vec<String> {
    Vec::new()
}

// Compatibility no-ops for source that remains behind the disabled panel feature.
pub fn initialize_d3d12_device() {}

pub fn initialize_d3d11_device() {}

#[unsafe(export_name = "bl_register_d3d12_render_callback")]
pub unsafe extern "system" fn bl_register_d3d12_render_callback(callback: D3D12RenderCallback) {
    register_d3d12_render_callback(callback);
}

#[unsafe(export_name = "bl_register_d3d11_render_callback")]
pub unsafe extern "system" fn bl_register_d3d11_render_callback(callback: D3D11RenderCallback) {
    register_d3d11_render_callback(callback);
}

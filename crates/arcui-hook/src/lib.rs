pub mod dx11;
pub mod dx12;
pub mod overlay_window;

mod dummy_hwnd;

pub type Dx12RenderCallback = unsafe extern "system" fn(
    device: *mut core::ffi::c_void,
    command_list: *mut core::ffi::c_void,
    render_target: *mut core::ffi::c_void,
    width: u32,
    height: u32,
);

pub type DrawDataCallback = fn(input: &arcui_core::InputSnapshot) -> arcui_core::DrawData;

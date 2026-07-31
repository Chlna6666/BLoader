use std::ffi::c_void;

use windows::Win32::Graphics::Direct3D12::{
    ID3D12Device, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::core::Interface;

pub struct Dx12PostProcessContext<'a> {
    pub device: &'a ID3D12Device,
    pub command_list: &'a ID3D12GraphicsCommandList,
    pub render_target: &'a ID3D12Resource,
    pub width: u32,
    pub height: u32,
    pub format: DXGI_FORMAT,
}

pub fn render(ctx: &Dx12PostProcessContext<'_>) {
    crate::core::d3d12_queue::invoke_d3d12_render_callback(
        ctx.device.as_raw(),
        ctx.command_list.as_raw(),
        ctx.render_target.as_raw(),
        ctx.width,
        ctx.height,
    );
}

pub unsafe extern "system" fn render_raw(
    device: *mut c_void,
    command_list: *mut c_void,
    render_target: *mut c_void,
    width: u32,
    height: u32,
) {
    if device.is_null()
        || command_list.is_null()
        || render_target.is_null()
        || width == 0
        || height == 0
    {
        return;
    }

    let Some(device) = (unsafe { ID3D12Device::from_raw_borrowed(&device) }) else {
        return;
    };
    let Some(command_list) =
        (unsafe { ID3D12GraphicsCommandList::from_raw_borrowed(&command_list) })
    else {
        return;
    };
    let Some(render_target) = (unsafe { ID3D12Resource::from_raw_borrowed(&render_target) }) else {
        return;
    };

    let ctx = Dx12PostProcessContext {
        device,
        command_list,
        render_target,
        width,
        height,
        format: DXGI_FORMAT::default(),
    };
    render(&ctx);
}

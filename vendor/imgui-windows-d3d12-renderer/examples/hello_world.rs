use std::error::Error;
use std::time::Instant;

use imgui::{FontConfig, FontSource};
use imgui_winit_support::{HiDpiMode, WinitPlatform};
use windows::core::{w, Interface};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_CPU_DESCRIPTOR_HANDLE, D3D12_DESCRIPTOR_HEAP_DESC,
    D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
    D3D12_DESCRIPTOR_HEAP_TYPE_RTV, D3D12_FENCE_FLAG_NONE, D3D12_MAX_DEPTH, D3D12_MIN_DEPTH,
    D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATE_RENDER_TARGET,
    D3D12_RESOURCE_TRANSITION_BARRIER,
};
use windows::Win32::Graphics::Dxgi::DXGI_MWA_NO_ALT_ENTER;
use windows::Win32::System::Threading::{CreateEventA, WaitForSingleObject, INFINITE};
use windows::Win32::{
    Foundation::{HANDLE, HWND, RECT},
    Graphics::{
        Direct3D::D3D_FEATURE_LEVEL_11_0,
        Direct3D12::{
            D3D12CreateDevice, D3D12GetDebugInterface, ID3D12CommandAllocator, ID3D12CommandQueue,
            ID3D12Debug, ID3D12DescriptorHeap, ID3D12Device, ID3D12Fence,
            ID3D12GraphicsCommandList, ID3D12Resource, D3D12_COMMAND_LIST_TYPE_DIRECT,
            D3D12_COMMAND_QUEUE_DESC, D3D12_VIEWPORT,
        },
        Dxgi::{
            Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
            CreateDXGIFactory2, IDXGIAdapter1, IDXGIFactory4, IDXGISwapChain3, DXGI_ADAPTER_FLAG,
            DXGI_ADAPTER_FLAG_NONE, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_CREATE_FACTORY_DEBUG,
            DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
        },
    },
};
use winit::dpi::LogicalSize;
use winit::platform::windows::WindowExtWindows;
use winit::{
    event::{Event, WindowEvent},
    event_loop::EventLoop,
    window::{Window, WindowBuilder},
};

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new();

    let builder = WindowBuilder::new()
        .with_inner_size(LogicalSize {
            width: 1024,
            height: 768,
        })
        .with_resizable(false);
    let window = builder.build(&event_loop)?;

    let mut hello_world = HelloWorld::new()?;
    hello_world.bind_to_window(&window)?;

    event_loop.run(move |event, _, control_flow| match event {
        Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } => {
            control_flow.set_exit();
        }
        event => hello_world.handle_event(&event, &window),
    });
}

struct HelloWorld {
    dxgi_factory: IDXGIFactory4,
    device: ID3D12Device,
    resources: Option<Resources>,
    last_frame: Instant,
}

impl HelloWorld {
    fn new() -> windows::core::Result<Self> {
        let (dxgi_factory, device) = create_device()?;

        Ok(HelloWorld {
            dxgi_factory,
            device,
            resources: None,
            last_frame: Instant::now(),
        })
    }

    fn bind_to_window(&mut self, window: &Window) -> windows::core::Result<()> {
        unsafe {
            let command_queue: ID3D12CommandQueue =
                self.device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })?;

            let size = window.inner_size();

            let swap_chain_desc = DXGI_SWAP_CHAIN_DESC1 {
                BufferCount: FRAME_COUNT,
                Width: size.width,
                Height: size.height,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    ..Default::default()
                },
                ..Default::default()
            };

            let hwnd = HWND(window.hwnd());

            let swap_chain: IDXGISwapChain3 = self
                .dxgi_factory
                .CreateSwapChainForHwnd(&command_queue, hwnd, &swap_chain_desc, None, None)?
                .cast()?;

            self.dxgi_factory
                .MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)?;

            let frame_index = swap_chain.GetCurrentBackBufferIndex();

            let rtv_heap: ID3D12DescriptorHeap =
                self.device
                    .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        NumDescriptors: FRAME_COUNT,
                        Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                        ..Default::default()
                    })?;

            let rtv_descriptor_size = self
                .device
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV)
                as usize;
            let rtv_handle = rtv_heap.GetCPUDescriptorHandleForHeapStart();

            let render_targets: [ID3D12Resource; FRAME_COUNT as usize] =
                array_init::try_array_init(|i: usize| -> windows::core::Result<ID3D12Resource> {
                    let render_target: ID3D12Resource = swap_chain.GetBuffer(i as u32)?;

                    self.device.CreateRenderTargetView(
                        &render_target,
                        None,
                        D3D12_CPU_DESCRIPTOR_HANDLE {
                            ptr: rtv_handle.ptr + i * rtv_descriptor_size,
                        },
                    );
                    Ok(render_target)
                })?;

            let viewport = D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: size.width as f32,
                Height: size.height as f32,
                MinDepth: D3D12_MIN_DEPTH,
                MaxDepth: D3D12_MAX_DEPTH,
            };

            let scissor_rect = RECT {
                left: 0,
                top: 0,
                right: size.width as i32,
                bottom: size.height as i32,
            };

            let srv_heap: ID3D12DescriptorHeap =
                self.device
                    .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                        NumDescriptors: 1,
                        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                        NodeMask: 1,
                    })?;

            srv_heap.SetName(w!("srv_heap"))?;

            let command_allocator = self
                .device
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;

            let command_list: ID3D12GraphicsCommandList = self.device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &command_allocator,
                None,
            )?;

            command_list.Close()?;

            let fence = self.device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;

            let fence_value = 1;

            let fence_event = CreateEventA(None, false, false, None)?;

            let mut imgui = imgui::Context::create();
            let mut winit_platform = WinitPlatform::init(&mut imgui);

            winit_platform.attach_window(imgui.io_mut(), window, HiDpiMode::Rounded);

            let hidpi_factor = winit_platform.hidpi_factor();
            let font_size = (13.0 * hidpi_factor) as f32;
            imgui.fonts().add_font(&[FontSource::DefaultFontData {
                config: Some(FontConfig {
                    size_pixels: font_size,
                    ..FontConfig::default()
                }),
            }]);

            imgui.io_mut().font_global_scale = (1.0 / hidpi_factor) as f32;

            let renderer = imgui_windows_d3d12_renderer::Renderer::new(
                &mut imgui,
                self.device.clone(),
                FRAME_COUNT as usize,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                srv_heap.GetCPUDescriptorHandleForHeapStart(),
                srv_heap.GetGPUDescriptorHandleForHeapStart(),
            )?;

            self.resources = Some(Resources {
                command_queue,
                swap_chain,
                frame_index,
                render_targets,
                rtv_heap,
                rtv_descriptor_size,
                srv_heap,
                viewport,
                scissor_rect,
                command_allocator,
                command_list,
                fence,
                fence_value,
                fence_event,
                imgui,
                winit_platform,
                renderer,
            });
        }

        Ok(())
    }

    fn handle_event<T>(&mut self, event: &Event<T>, window: &Window) {
        if let Some(ref mut resources) = self.resources {
            let io = resources.imgui.io_mut();

            match event {
                Event::NewEvents(_) => {
                    let now = Instant::now();
                    io.update_delta_time(now - self.last_frame);
                    self.last_frame = now;
                }
                Event::MainEventsCleared => {
                    resources
                        .winit_platform
                        .prepare_frame(io, window)
                        .expect("Failed to start frame");
                    window.request_redraw();
                }
                Event::RedrawRequested(_) => {
                    // redraw
                    self.draw(window);
                }
                Event::WindowEvent {
                    event: WindowEvent::Resized(_),
                    ..
                } => resources.winit_platform.handle_event(io, window, event),
                event => resources.winit_platform.handle_event(io, window, event),
            }
        }
    }

    fn draw(&mut self, window: &Window) {
        if let Some(resources) = &mut self.resources {
            let imgui = &mut resources.imgui;
            resources.renderer.new_frame(imgui).unwrap();

            let ui = imgui.new_frame();
            ui.show_demo_window(&mut true);

            resources.winit_platform.prepare_render(ui, window);

            populate_command_list(resources);

            // Execute the command list.
            let command_list = Some(resources.command_list.cast().unwrap());
            unsafe { resources.command_queue.ExecuteCommandLists(&[command_list]) };

            // Present the frame.
            unsafe { resources.swap_chain.Present(1, 0) }.ok().unwrap();

            wait_for_previous_frame(resources);
        }
    }
}

fn populate_command_list(resources: &mut Resources) {
    unsafe {
        resources.command_allocator.Reset().unwrap();

        let command_list = &resources.command_list;

        command_list
            .Reset(&resources.command_allocator, None)
            .unwrap();

        command_list.RSSetViewports(&[resources.viewport]);
        command_list.RSSetScissorRects(&[resources.scissor_rect]);

        let barrier = transition_barrier(
            &resources.render_targets[resources.frame_index as usize],
            D3D12_RESOURCE_STATE_PRESENT,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
        );
        command_list.ResourceBarrier(&[barrier]);

        let rtv_handle = D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: resources.rtv_heap.GetCPUDescriptorHandleForHeapStart().ptr
                + resources.frame_index as usize * resources.rtv_descriptor_size,
        };

        command_list.OMSetRenderTargets(1, Some(&rtv_handle), false, None);

        command_list.ClearRenderTargetView(rtv_handle, &[0.0_f32, 0.2_f32, 0.4_f32, 1.0_f32], None);

        command_list.SetDescriptorHeaps(&[Some(resources.srv_heap.clone())]);
        resources
            .renderer
            .render_draw_data(resources.imgui.render(), command_list);

        command_list.ResourceBarrier(&[transition_barrier(
            &resources.render_targets[resources.frame_index as usize],
            D3D12_RESOURCE_STATE_RENDER_TARGET,
            D3D12_RESOURCE_STATE_PRESENT,
        )]);

        command_list.Close().unwrap();
    }
}

fn wait_for_previous_frame(resources: &mut Resources) {
    // WAITING FOR THE FRAME TO COMPLETE BEFORE CONTINUING IS NOT BEST
    // PRACTICE. This is code implemented as such for simplicity. The
    // D3D12HelloFrameBuffering sample illustrates how to use fences for
    // efficient resource usage and to maximize GPU utilization.

    // Signal and increment the fence value.
    let fence = resources.fence_value;

    unsafe { resources.command_queue.Signal(&resources.fence, fence) }
        .ok()
        .unwrap();

    resources.fence_value += 1;

    // Wait until the previous frame is finished.
    if unsafe { resources.fence.GetCompletedValue() } < fence {
        unsafe {
            resources
                .fence
                .SetEventOnCompletion(fence, resources.fence_event)
        }
        .ok()
        .unwrap();

        unsafe { WaitForSingleObject(resources.fence_event, INFINITE) };
    }

    resources.frame_index = unsafe { resources.swap_chain.GetCurrentBackBufferIndex() };
}

const FRAME_COUNT: u32 = 2;

struct Resources {
    command_queue: ID3D12CommandQueue,
    swap_chain: IDXGISwapChain3,
    frame_index: u32,
    render_targets: [ID3D12Resource; FRAME_COUNT as usize],
    rtv_heap: ID3D12DescriptorHeap,
    rtv_descriptor_size: usize,
    srv_heap: ID3D12DescriptorHeap,
    viewport: D3D12_VIEWPORT,
    scissor_rect: RECT,
    command_allocator: ID3D12CommandAllocator,
    command_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_value: u64,
    fence_event: HANDLE,
    imgui: imgui::Context,
    winit_platform: WinitPlatform,
    renderer: imgui_windows_d3d12_renderer::Renderer,
}

fn create_device() -> windows::core::Result<(IDXGIFactory4, ID3D12Device)> {
    unsafe {
        let mut debug: Option<ID3D12Debug> = None;
        if let Some(debug) = D3D12GetDebugInterface(&mut debug).ok().and(debug) {
            debug.EnableDebugLayer();
        }
    }

    let dxgi_factory_flags = DXGI_CREATE_FACTORY_DEBUG;
    let dxgi_factory: IDXGIFactory4 = unsafe { CreateDXGIFactory2(dxgi_factory_flags) }?;

    let adapter = get_hardware_adapter(&dxgi_factory)?;

    let mut device: Option<ID3D12Device> = None;
    unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }?;
    Ok((dxgi_factory, device.unwrap()))
}

fn get_hardware_adapter(factory: &IDXGIFactory4) -> windows::core::Result<IDXGIAdapter1> {
    for i in 0.. {
        let adapter = unsafe { factory.EnumAdapters1(i)? };

        let mut desc = Default::default();
        unsafe { adapter.GetDesc1(&mut desc)? };

        if (DXGI_ADAPTER_FLAG(desc.Flags as i32) & DXGI_ADAPTER_FLAG_SOFTWARE)
            != DXGI_ADAPTER_FLAG_NONE
        {
            // Don't select the Basic Render Driver adapter. If you want a
            // software adapter, pass in "/warp" on the command line.
            continue;
        }

        // Check to see whether the adapter supports Direct3D 12, but don't
        // create the actual device yet.
        if unsafe {
            D3D12CreateDevice(
                &adapter,
                D3D_FEATURE_LEVEL_11_0,
                std::ptr::null_mut::<Option<ID3D12Device>>(),
            )
        }
        .is_ok()
        {
            return Ok(adapter);
        }
    }

    unreachable!()
}

fn transition_barrier(
    resource: &ID3D12Resource,
    state_before: D3D12_RESOURCE_STATES,
    state_after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: unsafe { std::mem::transmute_copy(resource) },
                StateBefore: state_before,
                StateAfter: state_after,
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
            }),
        },
    }
}

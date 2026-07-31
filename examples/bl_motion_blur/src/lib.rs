#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{c_void, CString};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use bl_sdk::bl::BLoader;
use bl_sdk::bl::BlUiCallback;
use bl_sdk::bl_export_mod;
use bl_sdk::effects;
use bl_sdk::i18n;
use bl_sdk::mc::{BlEventCallback, BL_EVENT_KEY, BL_EVENT_TICK};
use bl_sdk::{BlFeatureToggleCallback, FeaturePanelRegistration, FeatureToggleRegistration};
use windows::core::{Interface, Result, PCSTR};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D12::{
    D3D12SerializeRootSignature, ID3D12DescriptorHeap, ID3D12Device, ID3D12GraphicsCommandList,
    ID3D12PipelineState, ID3D12Resource, ID3D12RootSignature, D3D12_BLEND_DESC,
    D3D12_BLEND_INV_SRC_ALPHA, D3D12_BLEND_ONE, D3D12_BLEND_OP_ADD, D3D12_BLEND_SRC_ALPHA,
    D3D12_COLOR_WRITE_ENABLE_ALL, D3D12_COMPARISON_FUNC_ALWAYS,
    D3D12_CONSERVATIVE_RASTERIZATION_MODE_OFF, D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
    D3D12_CULL_MODE_NONE, D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING, D3D12_DEPTH_STENCIL_DESC,
    D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
    D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
    D3D12_DESCRIPTOR_HEAP_TYPE_RTV, D3D12_DESCRIPTOR_RANGE, D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND,
    D3D12_DESCRIPTOR_RANGE_TYPE_SRV, D3D12_FILL_MODE_SOLID, D3D12_FILTER_MIN_MAG_MIP_LINEAR,
    D3D12_GRAPHICS_PIPELINE_STATE_DESC, D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES,
    D3D12_HEAP_TYPE_DEFAULT, D3D12_INPUT_LAYOUT_DESC, D3D12_LOGIC_OP_CLEAR,
    D3D12_MEMORY_POOL_UNKNOWN, D3D12_PIPELINE_STATE_FLAGS, D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
    D3D12_RASTERIZER_DESC, D3D12_RENDER_TARGET_BLEND_DESC, D3D12_RESOURCE_BARRIER,
    D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATES,
    D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE,
    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_RENDER_TARGET,
    D3D12_RESOURCE_TRANSITION_BARRIER, D3D12_ROOT_CONSTANTS, D3D12_ROOT_DESCRIPTOR_TABLE,
    D3D12_ROOT_PARAMETER, D3D12_ROOT_PARAMETER_0, D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
    D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE, D3D12_ROOT_SIGNATURE_DESC,
    D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT, D3D12_ROOT_SIGNATURE_FLAG_NONE,
    D3D12_SHADER_BYTECODE, D3D12_SHADER_RESOURCE_VIEW_DESC, D3D12_SHADER_RESOURCE_VIEW_DESC_0,
    D3D12_SHADER_VISIBILITY_PIXEL, D3D12_SRV_DIMENSION_TEXTURE2D,
    D3D12_STATIC_BORDER_COLOR_OPAQUE_BLACK, D3D12_STATIC_SAMPLER_DESC, D3D12_TEX2D_SRV,
    D3D12_TEXTURE_ADDRESS_MODE_CLAMP, D3D12_TEXTURE_LAYOUT_UNKNOWN, D3D12_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};

const VS_HLSL: &str = r#"
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VSOut main(uint vid : SV_VertexID) {
    float2 pos[3] = { float2(-1.0, -1.0), float2(-1.0, 3.0), float2(3.0, -1.0) };
    float2 uv[3]  = { float2(0.0, 1.0), float2(0.0, -1.0), float2(2.0, 1.0) };
    VSOut o; o.pos = float4(pos[vid], 0.0, 1.0); o.uv = uv[vid]; return o;
}
"#;

const PS_HLSL: &str = r#"
Texture2D<float4> gTex : register(t0);
cbuffer BlurParams : register(b0) { float gOpacity; float gWidth; float gHeight; float gPad; };
struct PSIn { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
float4 main(PSIn input) : SV_Target {
    float2 px = input.uv * float2(gWidth, gHeight);
    int2 coord = int2(clamp(px, 0.0, float2(gWidth - 1.0, gHeight - 1.0)));
    float4 c = gTex.Load(int3(coord, 0));
    c.a *= gOpacity;
    return c;
}
"#;

const RESIZE_COOLDOWN_FRAMES: u32 = 4;

static ENABLED: AtomicBool = AtomicBool::new(true);
static HISTORY_FRAMES: AtomicU32 = AtomicU32::new(6);
static STRENGTH: AtomicU32 = AtomicU32::new(10);
static FRAME_INDEX: AtomicU64 = AtomicU64::new(0);
static RESIZE_COOLDOWN: AtomicU32 = AtomicU32::new(0);

struct D3D12History {
    textures: Vec<ID3D12Resource>,
    heaps: Vec<ID3D12DescriptorHeap>,
    next_slot: usize,
    filled: usize,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
}

impl D3D12History {
    fn new() -> Self {
        Self {
            textures: Vec::new(),
            heaps: Vec::new(),
            next_slot: 0,
            filled: 0,
            width: 0,
            height: 0,
            format: DXGI_FORMAT_UNKNOWN,
        }
    }

    fn clear(&mut self) {
        self.textures.clear();
        self.heaps.clear();
        self.next_slot = 0;
        self.filled = 0;
        self.width = 0;
        self.height = 0;
        self.format = DXGI_FORMAT_UNKNOWN;
    }

    fn ensure(
        &mut self,
        device: &ID3D12Device,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        count: usize,
    ) -> Result<bool> {
        if count == 0 {
            self.clear();
            return Ok(true);
        }

        let mut recreated = false;
        if self.width != width
            || self.height != height
            || self.format != format
            || self.textures.len() != count
        {
            self.clear();
            recreated = true;
            self.width = width;
            self.height = height;
            self.format = format;
            for _ in 0..count {
                let (texture, heap) = create_texture_with_srv(device, width, height, format)?;
                self.textures.push(texture);
                self.heaps.push(heap);
            }
        }

        if self.next_slot >= self.textures.len() {
            self.next_slot = 0;
        }
        Ok(recreated)
    }

    fn push_frame(&mut self, cmd: &ID3D12GraphicsCommandList, render_target: &ID3D12Resource) {
        if self.textures.is_empty() {
            return;
        }

        let slot = self.next_slot;
        let texture = &self.textures[slot];
        let to_copy_source = create_transition_barrier(
            render_target,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        let texture_before = if self.filled < self.textures.len() {
            D3D12_RESOURCE_STATE_COMMON
        } else {
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
        };
        let to_copy_dest =
            create_transition_barrier(texture, texture_before, D3D12_RESOURCE_STATE_COPY_DEST);
        unsafe {
            cmd.ResourceBarrier(&[to_copy_source, to_copy_dest]);
            cmd.CopyResource(texture, render_target);
        }

        let render_target_back = create_transition_barrier(
            render_target,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
        );
        let texture_to_srv = create_transition_barrier(
            texture,
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
        );
        unsafe {
            cmd.ResourceBarrier(&[render_target_back, texture_to_srv]);
        }

        self.next_slot = (self.next_slot + 1) % self.textures.len();
        self.filled = self.filled.saturating_add(1).min(self.textures.len());
    }

    fn visible_indices(&self) -> Vec<usize> {
        let used = self.filled.min(self.textures.len());
        if used <= 1 {
            return Vec::new();
        }
        let count = used - 1;
        let start = if self.filled < self.textures.len() {
            0
        } else {
            self.next_slot
        };
        (0..count)
            .map(|offset| (start + offset) % self.textures.len())
            .collect()
    }
}

struct BlurPipeline {
    format: DXGI_FORMAT,
    root_signature: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

static D3D12_HISTORY: OnceLock<Mutex<D3D12History>> = OnceLock::new();
static BLUR_PIPELINE: OnceLock<Mutex<Option<BlurPipeline>>> = OnceLock::new();

fn history() -> &'static Mutex<D3D12History> {
    D3D12_HISTORY.get_or_init(|| Mutex::new(D3D12History::new()))
}

fn pipeline_cache() -> &'static Mutex<Option<BlurPipeline>> {
    BLUR_PIPELINE.get_or_init(|| Mutex::new(None))
}

fn history_frames() -> u32 {
    HISTORY_FRAMES.load(Ordering::Relaxed).clamp(1, 16)
}

fn strength() -> f32 {
    STRENGTH.load(Ordering::Relaxed) as f32 / 16.0
}

fn max_opacity() -> f32 {
    let base = strength();
    if base <= 0.0 {
        0.0
    } else {
        base.min(1.0) * 0.55
    }
}

fn clear_history() {
    if let Some(lock) = D3D12_HISTORY.get() {
        if let Ok(mut guard) = lock.lock() {
            guard.clear();
        }
    }
    if let Some(lock) = BLUR_PIPELINE.get() {
        if let Ok(mut guard) = lock.lock() {
            *guard = None;
        }
    }
    RESIZE_COOLDOWN.store(0, Ordering::Relaxed);
}

fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        clear_history();
    }
}

fn set_strength(value: f32) {
    let clamped = value.clamp(0.0, 1.0);
    let scaled = (clamped * 16.0).round() as u32;
    STRENGTH.store(scaled.clamp(0, 16), Ordering::Relaxed);
}

fn set_history(value: u32) {
    let clamped = value.clamp(1, 16);
    if HISTORY_FRAMES.swap(clamped, Ordering::Relaxed) != clamped {
        clear_history();
    }
}

fn render_settings_controls(loader: &BLoader) {
    let ui = loader.ui();
    let mut enabled = ENABLED.load(Ordering::Relaxed);
    if ui.checkbox(&i18n::tr("motion_blur.ui.enabled"), &mut enabled) {
        set_enabled(enabled);
    }

    let mut history = history_frames() as f32;
    if ui.slider_float(&i18n::tr("motion_blur.ui.history"), &mut history, 1.0, 16.0) {
        set_history(history.round() as u32);
    }

    let mut intensity = STRENGTH.load(Ordering::Relaxed) as f32 / 16.0;
    if ui.slider_float(
        &i18n::tr("motion_blur.ui.strength"),
        &mut intensity,
        0.0,
        1.0,
    ) {
        set_strength(intensity);
    }

    ui.separator();
    ui.text(&format!(
        "{}: {}",
        i18n::tr("motion_blur.ui.frame_index"),
        FRAME_INDEX.load(Ordering::Relaxed)
    ));
    if let Some(backend) = loader.ui_backend() {
        ui.text(&format!(
            "{}: {backend}",
            i18n::tr("motion_blur.ui.backend")
        ));
    }

    if ui.button(&i18n::tr("motion_blur.ui.clear_history")) {
        clear_history();
    }
}

unsafe extern "system" fn on_feature_panel(_user_data: *mut c_void) {
    let Some(loader) = bl_sdk::current_bloader() else {
        return;
    };
    render_settings_controls(&loader);
}

unsafe extern "system" fn on_event(event_id: u32, payload: *const c_void, _user_data: *mut c_void) {
    match event_id {
        BL_EVENT_TICK => {
            if !payload.is_null() {
                let tick = &*(payload as *const bl_sdk::mc::BlTickEvent);
                FRAME_INDEX.store(tick.frame_index, Ordering::Relaxed);
            }
        }
        BL_EVENT_KEY => {
            if payload.is_null() {
                return;
            }
            let key = &*(payload as *const bl_sdk::mc::BlKeyEvent);
            if key.is_down == 0 || key.is_repeat != 0 {
                return;
            }
            match key.virtual_key {
                0x48 => set_enabled(!ENABLED.load(Ordering::Relaxed)),
                0x4A => set_history(history_frames().saturating_sub(1).max(1)),
                0x4B => set_history(history_frames().saturating_add(1).min(16)),
                0x4C => {
                    let current = STRENGTH.load(Ordering::Relaxed);
                    let next = if current >= 16 { 0 } else { current + 1 };
                    set_strength(next as f32 / 16.0);
                }
                _ => {}
            }
        }
        _ => {}
    }
}

unsafe extern "system" fn on_feature_toggle(enabled: u8, _user_data: *mut c_void) {
    set_enabled(enabled != 0);
}

unsafe extern "system" fn on_render_d3d12(
    device: *mut c_void,
    command_list: *mut c_void,
    back_buffer: *mut c_void,
    width: u32,
    height: u32,
) {
    if !ENABLED.load(Ordering::Relaxed)
        || device.is_null()
        || command_list.is_null()
        || back_buffer.is_null()
        || width == 0
        || height == 0
    {
        return;
    }

    // The host owns these COM objects for the current frame. Borrow them instead of
    // constructing owned wrappers that would call Release on return.
    let Some(device) = (unsafe { ID3D12Device::from_raw_borrowed(&device) }) else {
        return;
    };
    let Some(command_list) =
        (unsafe { ID3D12GraphicsCommandList::from_raw_borrowed(&command_list) })
    else {
        return;
    };
    let Some(back_buffer) = (unsafe { ID3D12Resource::from_raw_borrowed(&back_buffer) }) else {
        return;
    };
    let desc = back_buffer.GetDesc();
    let target_width = desc.Width as u32;
    let target_height = desc.Height;

    if target_width < 2
        || target_height < 2
        || width < 2
        || height < 2
        || target_width != width
        || target_height != height
        || history_frames() < 2
        || desc.Format == DXGI_FORMAT_UNKNOWN
    {
        clear_history();
        return;
    }

    let Ok(mut history) = history().lock() else {
        clear_history();
        return;
    };
    let recreated = match history.ensure(
        device,
        target_width,
        target_height,
        desc.Format,
        history_frames() as usize,
    ) {
        Ok(value) => value,
        Err(_) => {
            drop(history);
            clear_history();
            return;
        }
    };
    if recreated {
        RESIZE_COOLDOWN.store(RESIZE_COOLDOWN_FRAMES, Ordering::Relaxed);
        return;
    }

    let cooldown = RESIZE_COOLDOWN.load(Ordering::Relaxed);
    if cooldown > 0 {
        RESIZE_COOLDOWN.store(cooldown - 1, Ordering::Relaxed);
        return;
    }

    history.push_frame(command_list, back_buffer);
    drop(history);

    let _ = draw_motion_blur(
        device,
        command_list,
        back_buffer,
        target_width,
        target_height,
        desc.Format,
    );
}

fn draw_motion_blur(
    device: &ID3D12Device,
    command_list: &ID3D12GraphicsCommandList,
    render_target: &ID3D12Resource,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<()> {
    let Ok(mut cache) = pipeline_cache().lock() else {
        return Ok(());
    };
    if cache
        .as_ref()
        .map(|pipeline| pipeline.format != format)
        .unwrap_or(true)
    {
        *cache = Some(build_pipeline(device, format)?);
    }
    let Some(pipeline) = cache.as_ref() else {
        return Ok(());
    };

    let rtv_heap_desc = D3D12_DESCRIPTOR_HEAP_DESC {
        Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
        NumDescriptors: 1,
        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
        NodeMask: 0,
    };
    let rtv_heap: ID3D12DescriptorHeap = unsafe { device.CreateDescriptorHeap(&rtv_heap_desc)? };
    let rtv_handle = unsafe { rtv_heap.GetCPUDescriptorHandleForHeapStart() };
    unsafe {
        device.CreateRenderTargetView(render_target, None, rtv_handle);
    }

    let Ok(history) = history().lock() else {
        return Ok(());
    };
    let indices = history.visible_indices();
    if indices.is_empty() || max_opacity() <= 0.0 {
        return Ok(());
    }

    unsafe {
        command_list.OMSetRenderTargets(1, Some(&rtv_handle), false, None);
        command_list.RSSetViewports(&[D3D12_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: width as f32,
            Height: height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        }]);
        command_list.RSSetScissorRects(&[RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        }]);
        command_list.SetGraphicsRootSignature(&pipeline.root_signature);
        command_list.SetPipelineState(&pipeline.pso);
        command_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    }

    let count = indices.len() as f32;
    for (order, index) in indices.iter().enumerate() {
        let age = (order as f32 + 1.0) / count;
        let opacity = max_opacity() * age.powf(1.35);
        if opacity <= 0.001 {
            continue;
        }

        let heap = history.heaps[*index].clone();
        unsafe {
            command_list.SetDescriptorHeaps(&[Some(heap.clone())]);
            command_list.SetGraphicsRoot32BitConstants(
                0,
                4,
                [opacity, width as f32, height as f32, 0.0].as_ptr() as *const _,
                0,
            );
            command_list
                .SetGraphicsRootDescriptorTable(1, heap.GetGPUDescriptorHandleForHeapStart());
            command_list.DrawInstanced(3, 1, 0, 0);
        }
    }

    Ok(())
}

fn build_pipeline(device: &ID3D12Device, format: DXGI_FORMAT) -> Result<BlurPipeline> {
    let range = D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
        NumDescriptors: 1,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND,
    };

    let mut ranges = [range];
    let params = [
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Constants: D3D12_ROOT_CONSTANTS {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                    Num32BitValues: 4,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: ranges.as_mut_ptr(),
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        },
    ];

    let sampler = D3D12_STATIC_SAMPLER_DESC {
        Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D12_COMPARISON_FUNC_ALWAYS,
        BorderColor: D3D12_STATIC_BORDER_COLOR_OPAQUE_BLACK,
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
        ShaderRegister: 0,
        RegisterSpace: 0,
        ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
    };

    let root_desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: params.len() as u32,
        pParameters: params.as_ptr(),
        NumStaticSamplers: 1,
        pStaticSamplers: &sampler,
        Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE
            | D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
    };

    let mut sig_blob: Option<ID3DBlob> = None;
    let mut err_blob: Option<ID3DBlob> = None;
    unsafe {
        D3D12SerializeRootSignature(
            &root_desc,
            windows::Win32::Graphics::Direct3D12::D3D_ROOT_SIGNATURE_VERSION_1,
            &mut sig_blob,
            Some(&mut err_blob),
        )?;
    }
    let sig_blob = sig_blob.ok_or_else(windows::core::Error::empty)?;
    let root_signature: ID3D12RootSignature =
        unsafe { device.CreateRootSignature(0, blob_bytes(&sig_blob))? };

    let vs_blob = compile_shader(VS_HLSL, "main", "vs_5_0")?;
    let ps_blob = compile_shader(PS_HLSL, "main", "ps_5_0")?;

    let mut blend = D3D12_BLEND_DESC::default();
    blend.AlphaToCoverageEnable = false.into();
    blend.IndependentBlendEnable = false.into();
    blend.RenderTarget[0] = D3D12_RENDER_TARGET_BLEND_DESC {
        BlendEnable: true.into(),
        LogicOpEnable: false.into(),
        SrcBlend: D3D12_BLEND_SRC_ALPHA,
        DestBlend: D3D12_BLEND_INV_SRC_ALPHA,
        BlendOp: D3D12_BLEND_OP_ADD,
        SrcBlendAlpha: D3D12_BLEND_ONE,
        DestBlendAlpha: D3D12_BLEND_INV_SRC_ALPHA,
        BlendOpAlpha: D3D12_BLEND_OP_ADD,
        LogicOp: D3D12_LOGIC_OP_CLEAR,
        RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
    };

    let mut rast = D3D12_RASTERIZER_DESC::default();
    rast.FillMode = D3D12_FILL_MODE_SOLID;
    rast.CullMode = D3D12_CULL_MODE_NONE;
    rast.FrontCounterClockwise = false.into();
    rast.DepthBias = 0;
    rast.DepthBiasClamp = 0.0;
    rast.SlopeScaledDepthBias = 0.0;
    rast.DepthClipEnable = true.into();
    rast.MultisampleEnable = false.into();
    rast.AntialiasedLineEnable = false.into();
    rast.ForcedSampleCount = 0;
    rast.ConservativeRaster = D3D12_CONSERVATIVE_RASTERIZATION_MODE_OFF;

    let mut depth = D3D12_DEPTH_STENCIL_DESC::default();
    depth.DepthEnable = false.into();
    depth.StencilEnable = false.into();

    let pso_desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
        pRootSignature: ManuallyDrop::new(Some(root_signature.clone())),
        VS: D3D12_SHADER_BYTECODE {
            pShaderBytecode: unsafe { vs_blob.GetBufferPointer() },
            BytecodeLength: unsafe { vs_blob.GetBufferSize() },
        },
        PS: D3D12_SHADER_BYTECODE {
            pShaderBytecode: unsafe { ps_blob.GetBufferPointer() },
            BytecodeLength: unsafe { ps_blob.GetBufferSize() },
        },
        DS: D3D12_SHADER_BYTECODE::default(),
        HS: D3D12_SHADER_BYTECODE::default(),
        GS: D3D12_SHADER_BYTECODE::default(),
        StreamOutput: windows::Win32::Graphics::Direct3D12::D3D12_STREAM_OUTPUT_DESC::default(),
        BlendState: blend,
        SampleMask: u32::MAX,
        RasterizerState: rast,
        DepthStencilState: depth,
        InputLayout: D3D12_INPUT_LAYOUT_DESC::default(),
        IBStripCutValue:
            windows::Win32::Graphics::Direct3D12::D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED,
        PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
        NumRenderTargets: 1,
        RTVFormats: [
            format,
            DXGI_FORMAT_UNKNOWN,
            DXGI_FORMAT_UNKNOWN,
            DXGI_FORMAT_UNKNOWN,
            DXGI_FORMAT_UNKNOWN,
            DXGI_FORMAT_UNKNOWN,
            DXGI_FORMAT_UNKNOWN,
            DXGI_FORMAT_UNKNOWN,
        ],
        DSVFormat: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        NodeMask: 0,
        CachedPSO: windows::Win32::Graphics::Direct3D12::D3D12_CACHED_PIPELINE_STATE::default(),
        Flags: D3D12_PIPELINE_STATE_FLAGS::default(),
    };

    let pso: ID3D12PipelineState = unsafe { device.CreateGraphicsPipelineState(&pso_desc)? };
    Ok(BlurPipeline {
        format,
        root_signature,
        pso,
    })
}

fn create_texture_with_srv(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<(ID3D12Resource, ID3D12DescriptorHeap)> {
    let heap_props = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: width as u64,
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    };

    let mut texture: Option<ID3D12Resource> = None;
    unsafe {
        device.CreateCommittedResource(
            &heap_props,
            D3D12_HEAP_FLAG_NONE,
            &desc,
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut texture,
        )?;
    }
    let texture = texture.ok_or_else(windows::core::Error::empty)?;

    let heap_desc = D3D12_DESCRIPTOR_HEAP_DESC {
        Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
        NumDescriptors: 1,
        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
        NodeMask: 0,
    };
    let heap: ID3D12DescriptorHeap = unsafe { device.CreateDescriptorHeap(&heap_desc)? };

    let srv_desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
                PlaneSlice: 0,
                ResourceMinLODClamp: 0.0,
            },
        },
    };
    let cpu_handle = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    unsafe {
        device.CreateShaderResourceView(&texture, Some(&srv_desc), cpu_handle);
    }

    Ok((texture, heap))
}

fn create_transition_barrier(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

fn compile_shader(source: &str, entry: &str, profile: &str) -> Result<ID3DBlob> {
    let mut shader_blob: Option<ID3DBlob> = None;
    let mut error_blob: Option<ID3DBlob> = None;
    let entry = CString::new(entry).map_err(|_| windows::core::Error::empty())?;
    let profile = CString::new(profile).map_err(|_| windows::core::Error::empty())?;
    unsafe {
        D3DCompile(
            source.as_ptr() as *const _,
            source.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry.as_ptr() as *const u8),
            PCSTR(profile.as_ptr() as *const u8),
            0,
            0,
            &mut shader_blob,
            Some(&mut error_blob),
        )?;
    }
    shader_blob.ok_or_else(windows::core::Error::empty)
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}

fn on_load(host: &BLoader) -> i32 {
    ENABLED.store(true, Ordering::Relaxed);
    HISTORY_FRAMES.store(6, Ordering::Relaxed);
    STRENGTH.store(10, Ordering::Relaxed);
    FRAME_INDEX.store(0, Ordering::Relaxed);
    clear_history();

    let _ = i18n::register_lang("zh_CN", include_str!("../lang/zh_CN.lang"));
    let _ = i18n::register_lang("en_US", include_str!("../lang/en_US.lang"));

    let feature_title = i18n::tr("motion_blur.feature.title");
    let feature_description = i18n::tr("motion_blur.feature.description");

    host.register_event(
        "motion_blur.events",
        on_event as BlEventCallback,
        std::ptr::null_mut(),
    );
    host.register_feature_toggle(
        FeatureToggleRegistration {
            id: "motion_blur.enabled",
            title: &feature_title,
            description: &feature_description,
            default_enabled: true,
        },
        on_feature_toggle as BlFeatureToggleCallback,
        std::ptr::null_mut(),
    );
    host.register_feature_panel(
        FeaturePanelRegistration {
            id: "motion_blur.settings",
            title: &i18n::tr("motion_blur.ui.title"),
            description: &feature_description,
        },
        on_feature_panel as BlUiCallback,
        std::ptr::null_mut(),
    );
    effects::register_d3d12_render_callback(on_render_d3d12);
    bl_sdk::info!("Motion blur loaded as MOD-owned DX12 post-process with inline loader settings");
    0
}

fn on_unload() {
    set_enabled(false);
    clear_history();
}

bl_export_mod!(
    mod_id: "demo.motion_blur",
    mod_name: "Motion Blur",
    on_load: on_load,
    on_unload: on_unload
);

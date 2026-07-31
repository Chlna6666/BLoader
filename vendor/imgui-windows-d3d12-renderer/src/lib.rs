//
// D3D12 ImGui Renderer.  Based on
// https://github.com/ocornut/imgui/blob/master/backends/imgui_impl_dx12.h
//

use std::{
    ffi::c_void,
    io::{self, Write},
};

use imgui::{
    internal::RawWrapper, BackendFlags, Context, DrawCmd, DrawData, DrawIdx, DrawVert, TextureId,
};

use offset::offset_of;
//
use windows::{
    core::{s, w, Interface, Result, HSTRING, PCSTR},
    Win32::{
        Foundation::{CloseHandle, FALSE, RECT, TRUE},
        Graphics::{
            Direct3D::{Fxc::D3DCompile, ID3DBlob, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST},
            Direct3D12::{
                D3D12SerializeRootSignature, ID3D12CommandAllocator, ID3D12CommandQueue,
                ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList, ID3D12PipelineState,
                ID3D12Resource, ID3D12RootSignature, D3D12_BLEND_DESC, D3D12_BLEND_INV_SRC_ALPHA,
                D3D12_BLEND_ONE, D3D12_BLEND_OP_ADD, D3D12_BLEND_SRC_ALPHA,
                D3D12_COLOR_WRITE_ENABLE_ALL, D3D12_COMMAND_LIST_TYPE_DIRECT,
                D3D12_COMMAND_QUEUE_DESC, D3D12_COMPARISON_FUNC_ALWAYS,
                D3D12_CPU_DESCRIPTOR_HANDLE, D3D12_CULL_MODE_NONE, D3D12_DEFAULT_DEPTH_BIAS,
                D3D12_DEFAULT_DEPTH_BIAS_CLAMP, D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                D3D12_DEFAULT_SLOPE_SCALED_DEPTH_BIAS, D3D12_DEPTH_STENCILOP_DESC,
                D3D12_DEPTH_STENCIL_DESC, D3D12_DEPTH_WRITE_MASK_ALL, D3D12_DESCRIPTOR_RANGE,
                D3D12_DESCRIPTOR_RANGE_TYPE_SRV, D3D12_FENCE_FLAG_NONE, D3D12_FILL_MODE_SOLID,
                D3D12_FILTER_MIN_MAG_MIP_LINEAR, D3D12_GPU_DESCRIPTOR_HANDLE,
                D3D12_GRAPHICS_PIPELINE_STATE_DESC, D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES,
                D3D12_HEAP_TYPE_DEFAULT, D3D12_HEAP_TYPE_UPLOAD, D3D12_INDEX_BUFFER_VIEW,
                D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA, D3D12_INPUT_ELEMENT_DESC,
                D3D12_INPUT_LAYOUT_DESC, D3D12_LOGIC_OP_NOOP, D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
                D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE, D3D12_RANGE, D3D12_RASTERIZER_DESC,
                D3D12_RENDER_TARGET_BLEND_DESC, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
                D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_BUFFER,
                D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_GENERIC_READ, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_TRANSITION_BARRIER, D3D12_ROOT_CONSTANTS,
                D3D12_ROOT_DESCRIPTOR_TABLE, D3D12_ROOT_PARAMETER, D3D12_ROOT_PARAMETER_0,
                D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE, D3D12_ROOT_SIGNATURE_DESC,
                D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
                D3D12_ROOT_SIGNATURE_FLAG_DENY_DOMAIN_SHADER_ROOT_ACCESS,
                D3D12_ROOT_SIGNATURE_FLAG_DENY_GEOMETRY_SHADER_ROOT_ACCESS,
                D3D12_ROOT_SIGNATURE_FLAG_DENY_HULL_SHADER_ROOT_ACCESS, D3D12_SHADER_BYTECODE,
                D3D12_SHADER_RESOURCE_VIEW_DESC, D3D12_SHADER_RESOURCE_VIEW_DESC_0,
                D3D12_SHADER_VISIBILITY_PIXEL, D3D12_SHADER_VISIBILITY_VERTEX,
                D3D12_SRV_DIMENSION_TEXTURE2D, D3D12_STATIC_BORDER_COLOR_TRANSPARENT_BLACK,
                D3D12_STATIC_SAMPLER_DESC, D3D12_STENCIL_OP_KEEP, D3D12_SUBRESOURCE_FOOTPRINT,
                D3D12_TEX2D_SRV, D3D12_TEXTURE_ADDRESS_MODE_WRAP, D3D12_TEXTURE_COPY_LOCATION,
                D3D12_TEXTURE_COPY_LOCATION_0, D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX, D3D12_TEXTURE_DATA_PITCH_ALIGNMENT,
                D3D12_TEXTURE_LAYOUT_ROW_MAJOR, D3D12_VERTEX_BUFFER_VIEW, D3D12_VIEWPORT,
                D3D_ROOT_SIGNATURE_VERSION_1,
            },
            Dxgi::Common::{
                DXGI_FORMAT, DXGI_FORMAT_R16_UINT, DXGI_FORMAT_R32G32_FLOAT, DXGI_FORMAT_R32_UINT,
                DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
            },
        },
        System::Threading::{CreateEventA, WaitForSingleObject, INFINITE},
    },
};

pub struct Renderer {
    device: ID3D12Device,
    rtv_format: DXGI_FORMAT,
    font_srv_cpu_desc_handle: D3D12_CPU_DESCRIPTOR_HANDLE,
    font_srv_gpu_desc_handle: D3D12_GPU_DESCRIPTOR_HANDLE,
    num_frames_in_flight: usize,
    frame_index: usize,
    device_objects: Option<DeviceObjects>,
}

struct DeviceObjects {
    root_signature: ID3D12RootSignature,
    pipeline_state: ID3D12PipelineState,
    _font_texture: ID3D12Resource, // keep reference alive
    frame_resources: Vec<RenderBuffers>,
}

#[derive(Default)]
struct RenderBuffers {
    index_buffer: Option<ID3D12Resource>,
    vertex_buffer: Option<ID3D12Resource>,
    index_buffer_size: usize,
    vertex_buffer_size: usize,
}

impl Renderer {
    pub fn new(
        context: &mut Context,
        device: ID3D12Device,
        num_frames_in_flight: usize,
        rtv_format: DXGI_FORMAT,
        font_srv_cpu_desc_handle: D3D12_CPU_DESCRIPTOR_HANDLE,
        font_srv_gpu_desc_handle: D3D12_GPU_DESCRIPTOR_HANDLE,
    ) -> Result<Self> {
        context.set_renderer_name(Some(format!(
            "imgui-windows-d3d12-renderer {}",
            env!("CARGO_PKG_VERSION")
        )));

        context
            .io_mut()
            .backend_flags
            .insert(BackendFlags::RENDERER_HAS_VTX_OFFSET);

        Ok(Renderer {
            device,
            rtv_format,
            font_srv_cpu_desc_handle,
            font_srv_gpu_desc_handle,
            num_frames_in_flight,
            frame_index: usize::MAX,
            device_objects: None,
        })
    }

    pub fn new_frame(&mut self, context: &mut Context) -> Result<()> {
        if self.device_objects.is_none() {
            self.create_device_objects(context)?;
        }

        Ok(())
    }

    pub fn invalidate_device_objects(&mut self, context: &mut Context) {
        context.fonts().tex_id = TextureId::new(0);
        self.device_objects = None;
    }

    pub fn create_device_objects(&mut self, context: &mut Context) -> Result<()> {
        if self.device_objects.is_some() {
            self.invalidate_device_objects(context);
        }

        self.device_objects = Some(DeviceObjects::new(
            context,
            &self.device,
            self.rtv_format,
            self.font_srv_cpu_desc_handle,
            self.font_srv_gpu_desc_handle,
            self.num_frames_in_flight,
        )?);

        Ok(())
    }
}

impl DeviceObjects {
    fn new(
        context: &mut Context,
        device: &ID3D12Device,
        rtv_format: DXGI_FORMAT,
        font_srv_cpu_desc_handle: D3D12_CPU_DESCRIPTOR_HANDLE,
        font_srv_gpu_desc_handle: D3D12_GPU_DESCRIPTOR_HANDLE,
        num_frames_in_flight: usize,
    ) -> Result<Self> {
        let root_signature = Self::create_root_signature(device)?;
        let pipeline_state = Self::create_pipeline_state(device, rtv_format, &root_signature)?;

        let font_texture = Self::create_fonts_texture(
            device,
            context,
            font_srv_cpu_desc_handle,
            font_srv_gpu_desc_handle,
        )?;

        let mut frame_resources: Vec<RenderBuffers> = Vec::new();
        frame_resources.resize_with(num_frames_in_flight, RenderBuffers::default);

        Ok(DeviceObjects {
            root_signature,
            pipeline_state,
            _font_texture: font_texture,
            frame_resources,
        })
    }

    fn create_pipeline_state(
        device: &ID3D12Device,
        rtv_format: DXGI_FORMAT,
        root_signature: &ID3D12RootSignature,
    ) -> Result<ID3D12PipelineState> {
        let (vertex_shader, input_layout) = Self::create_vertex_shader()?;
        let pixel_shader = Self::create_pixel_shader()?;

        let shader_bytecode = |shader: &ID3DBlob| unsafe {
            D3D12_SHADER_BYTECODE {
                pShaderBytecode: shader.GetBufferPointer(),
                BytecodeLength: shader.GetBufferSize(),
            }
        };

        let default_stencilop = D3D12_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D12_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
            StencilPassOp: D3D12_STENCIL_OP_KEEP,
            StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
        };

        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: unsafe { std::mem::transmute_copy(root_signature) },
            VS: shader_bytecode(&vertex_shader),
            PS: shader_bytecode(&pixel_shader),
            BlendState: D3D12_BLEND_DESC {
                AlphaToCoverageEnable: false.into(),
                IndependentBlendEnable: false.into(),
                RenderTarget: [
                    D3D12_RENDER_TARGET_BLEND_DESC {
                        BlendEnable: true.into(),
                        LogicOpEnable: false.into(),
                        SrcBlend: D3D12_BLEND_SRC_ALPHA,
                        DestBlend: D3D12_BLEND_INV_SRC_ALPHA,
                        BlendOp: D3D12_BLEND_OP_ADD,
                        SrcBlendAlpha: D3D12_BLEND_ONE,
                        DestBlendAlpha: D3D12_BLEND_INV_SRC_ALPHA,
                        BlendOpAlpha: D3D12_BLEND_OP_ADD,
                        LogicOp: D3D12_LOGIC_OP_NOOP,
                        RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
                    },
                    D3D12_RENDER_TARGET_BLEND_DESC::default(),
                    D3D12_RENDER_TARGET_BLEND_DESC::default(),
                    D3D12_RENDER_TARGET_BLEND_DESC::default(),
                    D3D12_RENDER_TARGET_BLEND_DESC::default(),
                    D3D12_RENDER_TARGET_BLEND_DESC::default(),
                    D3D12_RENDER_TARGET_BLEND_DESC::default(),
                    D3D12_RENDER_TARGET_BLEND_DESC::default(),
                ],
            },
            SampleMask: u32::MAX,
            RasterizerState: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_NONE,
                DepthBias: D3D12_DEFAULT_DEPTH_BIAS,
                DepthBiasClamp: D3D12_DEFAULT_DEPTH_BIAS_CLAMP,
                SlopeScaledDepthBias: D3D12_DEFAULT_SLOPE_SCALED_DEPTH_BIAS,
                DepthClipEnable: TRUE,
                ..Default::default()
            },
            DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                DepthEnable: FALSE,
                DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL,
                DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
                StencilEnable: FALSE,
                FrontFace: default_stencilop,
                BackFace: default_stencilop,
                ..Default::default()
            },
            InputLayout: D3D12_INPUT_LAYOUT_DESC {
                pInputElementDescs: input_layout.as_ptr(),
                NumElements: input_layout.len() as u32,
            },
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        desc.RTVFormats[0] = rtv_format;

        unsafe { device.CreateGraphicsPipelineState(&desc) }
    }

    fn create_root_signature(device: &ID3D12Device) -> Result<ID3D12RootSignature> {
        let desc_range = D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: 1,
            ..Default::default()
        };

        let param = [
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS {
                        ShaderRegister: 0,
                        RegisterSpace: 0,
                        Num32BitValues: 16,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_VERTEX,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                        NumDescriptorRanges: 1,
                        pDescriptorRanges: &desc_range,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
            },
        ];

        // Bilinear sampling is required by default. Set 'io.Fonts->Flags |=
        // ImFontAtlasFlags_NoBakedLines' or 'style.AntiAliasedLinesUseTex =
        // false' to allow point/nearest sampling.
        let static_sampler = D3D12_STATIC_SAMPLER_DESC {
            Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
            AddressV: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
            AddressW: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
            MipLODBias: 0.0,
            MaxAnisotropy: 0,
            ComparisonFunc: D3D12_COMPARISON_FUNC_ALWAYS,
            BorderColor: D3D12_STATIC_BORDER_COLOR_TRANSPARENT_BLACK,
            MinLOD: 0.0,
            MaxLOD: 0.0,
            ShaderRegister: 0,
            RegisterSpace: 0,
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        };

        let desc = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: param.len() as u32,
            pParameters: param.as_ptr(),
            NumStaticSamplers: 1,
            pStaticSamplers: &static_sampler,
            Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT
                | D3D12_ROOT_SIGNATURE_FLAG_DENY_HULL_SHADER_ROOT_ACCESS
                | D3D12_ROOT_SIGNATURE_FLAG_DENY_DOMAIN_SHADER_ROOT_ACCESS
                | D3D12_ROOT_SIGNATURE_FLAG_DENY_GEOMETRY_SHADER_ROOT_ACCESS,
        };

        let mut signature = None;
        let signature = unsafe {
            D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut signature, None)
        }
        .map(|()| signature.unwrap())?;

        unsafe {
            device.CreateRootSignature(
                0,
                std::slice::from_raw_parts(
                    signature.GetBufferPointer() as _,
                    signature.GetBufferSize(),
                ),
            )
        }
    }

    fn create_vertex_shader() -> Result<(ID3DBlob, [D3D12_INPUT_ELEMENT_DESC; 3])> {
        let shader = compile_shader(
            r"
    cbuffer vertexBuffer: register(b0) {
                float4x4 ProjectionMatrix;
            };
    
            struct VS_INPUT {
                float2 pos: POSITION;
                float4 col: COLOR0;
                float2 uv: TEXCOORD0;
            };
    
            struct PS_INPUT {
                float4 pos: SV_POSITION;
                float4 col: COLOR0;
                float2 uv: TEXCOORD0;
            };
    
            PS_INPUT main(VS_INPUT input) {
                PS_INPUT output;
                output.pos = mul(ProjectionMatrix, float4(input.pos.xy, 0.f, 1.f));
                output.col = input.col;
                output.uv = input.uv;
                return output;
            }
    ",
            s!("main"),
            s!("vs_5_1"),
        )?;

        macro_rules! element {
            ($semantic:expr, $format:expr, $offset:expr) => {
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: s!($semantic),
                    SemanticIndex: 0,
                    Format: $format,
                    InputSlot: 0,
                    AlignedByteOffset: $offset,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                }
            };
        }

        let layout: [D3D12_INPUT_ELEMENT_DESC; 3] = [
            element!(
                "POSITION",
                DXGI_FORMAT_R32G32_FLOAT,
                offset_of!(DrawVert::pos).as_u32()
            ),
            element!(
                "TEXCOORD",
                DXGI_FORMAT_R32G32_FLOAT,
                offset_of!(DrawVert::uv).as_u32()
            ),
            element!(
                "COLOR",
                DXGI_FORMAT_R8G8B8A8_UNORM,
                offset_of!(DrawVert::col).as_u32()
            ),
        ];

        Ok((shader, layout))
    }

    fn create_pixel_shader() -> Result<ID3DBlob> {
        compile_shader(
            r"
    struct PS_INPUT {
        float4 pos: SV_POSITION;
        float4 col: COLOR0;
        float2 uv: TEXCOORD0;
    };
    
    sampler sampler0;
    Texture2D texture0;
    
    float4 main(PS_INPUT input): SV_Target {
        float4 out_col = input.col * texture0.Sample(sampler0, input.uv);
        return out_col;
    }
    ",
            s!("main"),
            s!("ps_5_1"),
        )
    }

    fn create_fonts_texture(
        device: &ID3D12Device,
        context: &mut Context,
        font_srv_cpu_desc_handle: D3D12_CPU_DESCRIPTOR_HANDLE,
        font_srv_gpu_desc_handle: D3D12_GPU_DESCRIPTOR_HANDLE,
    ) -> Result<ID3D12Resource> {
        let mut fonts = context.fonts();
        let font_atlas_texture = fonts.build_rgba32_texture();

        // Upload texture to graphics system
        unsafe {
            // Create the destination texture resource
            let resource_desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: font_atlas_texture.width as u64,
                Height: font_atlas_texture.height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                ..Default::default()
            };

            let mut texture: Option<ID3D12Resource> = None;

            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &resource_desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut texture,
            )?;
            let texture = texture.unwrap();
            texture.SetName(w!("imgui font texture")).unwrap();

            // Create the upload buffer resource
            let upload_pitch =
                (font_atlas_texture.width * 4).next_multiple_of(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT);
            let upload_size = font_atlas_texture.height * upload_pitch;

            let resource_desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: upload_size as u64,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            };

            let mut upload_buffer: Option<ID3D12Resource> = None;

            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_UPLOAD,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &resource_desc,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut upload_buffer,
            )?;

            let upload_buffer = upload_buffer.unwrap();

            // Copy the texture data into the upload buffer
            let mut mapped = std::ptr::null_mut();
            upload_buffer.Map(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: upload_size as usize,
                }),
                Some(&mut mapped),
            )?;
            let mapped: *mut u8 = mapped.cast();

            for y in 0..font_atlas_texture.height {
                let size = font_atlas_texture.width as usize * 4;

                let dest = std::slice::from_raw_parts_mut(
                    mapped.offset((y * upload_pitch) as isize),
                    size,
                );

                let src_start = (y * font_atlas_texture.width * 4) as usize;
                let src_end = src_start + size;

                let src = &font_atlas_texture.data[src_start..src_end];

                dest.copy_from_slice(src);
            }
            upload_buffer.Unmap(0, None);

            // Copy the upload buffer into the destination texture
            let src_location = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::ManuallyDrop::new(std::mem::transmute_copy(&upload_buffer)),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                            Width: font_atlas_texture.width,
                            Height: font_atlas_texture.height,
                            Depth: 1,
                            RowPitch: upload_pitch,
                        },
                        ..Default::default()
                    },
                },
            };

            let dst_location = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&texture),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };

            let barrier = D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: std::mem::transmute_copy(&texture),
                        Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: D3D12_RESOURCE_STATE_COPY_DEST,
                        StateAfter: D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                    }),
                },
                ..Default::default()
            };

            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let event = CreateEventA(None, false, false, None)?;

            let cmd_queue: ID3D12CommandQueue =
                device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    NodeMask: 1,
                    ..Default::default()
                })?;

            let cmd_allocator: ID3D12CommandAllocator =
                device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;

            let cmd_list: ID3D12GraphicsCommandList = device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &cmd_allocator,
                None,
            )?;

            cmd_list.CopyTextureRegion(&dst_location, 0, 0, 0, &src_location, None);
            cmd_list.ResourceBarrier(&[barrier]);
            cmd_list.Close()?;

            cmd_queue.ExecuteCommandLists(&[Some(cmd_list.cast().unwrap())]);
            cmd_queue.Signal(&fence, 1)?;

            fence.SetEventOnCompletion(1, event)?;
            WaitForSingleObject(event, INFINITE);

            CloseHandle(event)?;

            // Create the texture view
            device.CreateShaderResourceView(
                &texture,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_SRV {
                            MipLevels: 1,
                            ..Default::default()
                        },
                    },
                }),
                font_srv_cpu_desc_handle,
            );

            // Store the identifier
            fonts.tex_id = TextureId::new(font_srv_gpu_desc_handle.ptr as usize);

            Ok(texture)
        }
    }
}

fn compile_shader(hlsl: &str, entry_point: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut shader = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = unsafe {
        D3DCompile(
            hlsl.as_ptr() as *const c_void,
            hlsl.len(),
            None,
            None,
            None,
            entry_point,
            target,
            0,
            0,
            &mut shader,
            Some(&mut errors),
        )
    }
    .map(|()| shader.unwrap());

    if result.is_err() {
        unsafe {
            let error_blob = errors.unwrap();
            let _ = io::stdout().write_all(std::slice::from_raw_parts(
                error_blob.GetBufferPointer() as *const u8,
                error_blob.GetBufferSize(),
            ));
        }
    }

    result
}

// render_draw_data

impl Renderer {
    pub fn render_draw_data(
        &mut self,
        draw_data: &DrawData,
        graphics_command_list: &ID3D12GraphicsCommandList,
    ) {
        if draw_data.display_size.iter().any(|size| *size <= 0.0) {
            return;
        }

        if let Some(device_objects) = self.device_objects.as_mut() {
            self.frame_index = self.frame_index.wrapping_add(1);
            device_objects.render_draw_data(
                &self.device,
                self.frame_index % self.num_frames_in_flight,
                draw_data,
                graphics_command_list,
            );
        }
    }
}

impl DeviceObjects {
    fn render_draw_data(
        &mut self,
        device: &ID3D12Device,
        frame_index: usize,
        draw_data: &DrawData,
        graphics_command_list: &ID3D12GraphicsCommandList,
    ) {
        unsafe {
            self.frame_resources[frame_index].render_draw_data(
                device,
                &self.root_signature,
                &self.pipeline_state,
                draw_data,
                graphics_command_list,
            )
        }
    }
}

impl RenderBuffers {
    unsafe fn render_draw_data(
        &mut self,
        device: &ID3D12Device,
        root_signature: &ID3D12RootSignature,
        pipeline_state: &ID3D12PipelineState,
        draw_data: &DrawData,
        graphics_command_list: &ID3D12GraphicsCommandList,
    ) {
        // Create and grow vertex/index buffers if needed
        if self.vertex_buffer.is_none()
            || self.vertex_buffer_size < draw_data.total_vtx_count as usize
        {
            self.vertex_buffer_size = draw_data.total_vtx_count as usize + 5000;
            self.vertex_buffer = None;
            self.vertex_buffer = Some(
                Self::create_buffer(
                    device,
                    self.vertex_buffer_size * std::mem::size_of::<DrawVert>(),
                )
                .unwrap(),
            );

            static mut VBCOUNT: usize = 0;
            self.vertex_buffer
                .as_mut()
                .unwrap()
                .SetName(&HSTRING::from(format!("imgui VB {}", VBCOUNT)))
                .unwrap();
            VBCOUNT += 1;
        }

        if self.index_buffer.is_none()
            || self.index_buffer_size < draw_data.total_idx_count as usize
        {
            self.index_buffer_size = draw_data.total_idx_count as usize + 10000;
            self.index_buffer = None;
            self.index_buffer = Some(
                Self::create_buffer(
                    device,
                    self.index_buffer_size * std::mem::size_of::<DrawIdx>(),
                )
                .unwrap(),
            );

            static mut IBCOUNT: usize = 0;
            self.vertex_buffer
                .as_mut()
                .unwrap()
                .SetName(&HSTRING::from(format!("imgui IB {}", IBCOUNT)))
                .unwrap();
            IBCOUNT += 1;
        }

        // Upload vertex/index data into a single contiguous GPU buffer
        let vertex_buffer = self.vertex_buffer.as_ref().unwrap();
        let index_buffer = self.index_buffer.as_ref().unwrap();

        let vtx_resource = Self::map(vertex_buffer);
        let idx_resource = Self::map(index_buffer);

        let vtx_dest = std::slice::from_raw_parts_mut(
            vtx_resource.cast::<DrawVert>(),
            self.vertex_buffer_size,
        );

        let idx_dest =
            std::slice::from_raw_parts_mut(idx_resource.cast::<DrawIdx>(), self.index_buffer_size);

        let mut vtx_dest_index = 0;
        let mut idx_dest_index = 0;

        for cmd_list in draw_data.draw_lists() {
            let num_verts = cmd_list.vtx_buffer().len();
            vtx_dest[vtx_dest_index..vtx_dest_index + num_verts]
                .copy_from_slice(cmd_list.vtx_buffer());
            vtx_dest_index += num_verts;

            let num_indices = cmd_list.idx_buffer().len();
            idx_dest[idx_dest_index..idx_dest_index + num_indices]
                .copy_from_slice(cmd_list.idx_buffer());
            idx_dest_index += num_indices;
        }

        vertex_buffer.Unmap(0, None);
        index_buffer.Unmap(0, None);

        self.setup_render_state(
            root_signature,
            pipeline_state,
            draw_data,
            graphics_command_list,
        );

        // Render command lists
        // (Because we merged all buffers into a single one, we maintain our own offset into them)

        let mut global_vtx_offset = 0;
        let mut global_idx_offset = 0;

        for cmd_list in draw_data.draw_lists() {
            for cmd in cmd_list.commands() {
                match cmd {
                    DrawCmd::Elements { count, cmd_params } => {
                        // Project scissor/clipping rectangles into framebuffer space
                        let clip_off = draw_data.display_pos;
                        let clip_min = [
                            cmd_params.clip_rect[0] - clip_off[0],
                            cmd_params.clip_rect[1] - clip_off[1],
                        ];
                        let clip_max = [
                            cmd_params.clip_rect[2] - clip_off[0],
                            cmd_params.clip_rect[3] - clip_off[1],
                        ];

                        if clip_max[0] <= clip_min[0] || clip_max[1] <= clip_min[1] {
                            continue;
                        }

                        // Apply scissor/clipping rectangle, bind texture, Draw
                        let r = RECT {
                            left: clip_min[0] as i32,
                            top: clip_min[1] as i32,
                            right: clip_max[0] as i32,
                            bottom: clip_max[1] as i32,
                        };

                        let texture_handle = D3D12_GPU_DESCRIPTOR_HANDLE {
                            ptr: cmd_params.texture_id.id() as u64,
                        };

                        graphics_command_list.SetGraphicsRootDescriptorTable(1, texture_handle);
                        graphics_command_list.RSSetScissorRects(&[r]);
                        graphics_command_list.DrawIndexedInstanced(
                            count as u32,
                            1,
                            (cmd_params.idx_offset + global_idx_offset) as u32,
                            (cmd_params.vtx_offset + global_vtx_offset) as i32,
                            0,
                        );
                    }
                    DrawCmd::ResetRenderState => self.setup_render_state(
                        root_signature,
                        pipeline_state,
                        draw_data,
                        graphics_command_list,
                    ),
                    DrawCmd::RawCallback { callback, raw_cmd } => {
                        callback(cmd_list.raw(), raw_cmd);
                    }
                }
            }
            global_idx_offset += cmd_list.idx_buffer().len();
            global_vtx_offset += cmd_list.vtx_buffer().len();
        }
    }

    unsafe fn setup_render_state(
        &self,
        root_signature: &ID3D12RootSignature,
        pipeline_state: &ID3D12PipelineState,
        draw_data: &DrawData,
        graphics_command_list: &ID3D12GraphicsCommandList,
    ) {
        #[repr(C)]
        struct VertexConstantBuffer {
            mvp: [[f32; 4]; 4],
        }

        // Setup orthographic projection matrix into our constant buffer
        // Our visible imgui space lies from draw_data->DisplayPos (top left) to draw_data->DisplayPos+data_data->DisplaySize (bottom right).

        let l = draw_data.display_pos[0];
        let r = draw_data.display_pos[0] + draw_data.display_size[0];
        let t = draw_data.display_pos[1];
        let b = draw_data.display_pos[1] + draw_data.display_size[1];

        let vertex_constant_buffer = VertexConstantBuffer {
            mvp: [
                [2.0 / (r - l), 0.0, 0.0, 0.0],
                [0.0, 2.0 / (t - b), 0.0, 0.0],
                [0.0, 0.0, 0.5, 0.0],
                [(r + l) / (l - r), (t + b) / (b - t), 0.5, 1.0],
            ],
        };

        // Setup viewport
        let vp = D3D12_VIEWPORT {
            Width: draw_data.display_size[0],
            Height: draw_data.display_size[1],
            MinDepth: 0.0,
            MaxDepth: 1.0,
            ..Default::default()
        };
        unsafe {
            graphics_command_list.RSSetViewports(&[vp]);
        }

        // Bind shader and vertex buffers
        let stride = std::mem::size_of::<DrawVert>();
        let vbv = D3D12_VERTEX_BUFFER_VIEW {
            BufferLocation: self.vertex_buffer.as_ref().unwrap().GetGPUVirtualAddress(),
            SizeInBytes: (self.vertex_buffer_size * stride) as u32,
            StrideInBytes: stride as u32,
        };
        graphics_command_list.IASetVertexBuffers(0, Some(&[vbv]));

        let stride = std::mem::size_of::<DrawIdx>();
        let ibv = D3D12_INDEX_BUFFER_VIEW {
            BufferLocation: self.index_buffer.as_ref().unwrap().GetGPUVirtualAddress(),
            SizeInBytes: (self.index_buffer_size * stride) as u32,
            Format: if stride == 2 {
                DXGI_FORMAT_R16_UINT
            } else {
                DXGI_FORMAT_R32_UINT
            },
        };
        graphics_command_list.IASetIndexBuffer(Some(&ibv));

        graphics_command_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        graphics_command_list.SetGraphicsRootSignature(root_signature);
        graphics_command_list.SetPipelineState(pipeline_state);
        graphics_command_list.SetGraphicsRoot32BitConstants(
            0,
            16,
            std::ptr::addr_of!(vertex_constant_buffer.mvp) as *const c_void,
            0,
        );

        // Setup blend factor
        graphics_command_list.OMSetBlendFactor(Some(&[0.0, 0.0, 0.0, 0.0]));
    }

    fn create_buffer(device: &ID3D12Device, width: usize) -> Result<ID3D12Resource> {
        let mut resource: Option<ID3D12Resource> = None;

        unsafe {
            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_UPLOAD,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: width as u64,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut resource,
            )?;
        }

        Ok(resource.unwrap())
    }

    unsafe fn map(resource: &ID3D12Resource) -> *mut u8 {
        let mut mapped = std::ptr::null_mut();
        resource.Map(0, None, Some(&mut mapped)).unwrap();
        mapped as *mut u8
    }
}

use std::ffi::c_void;
use std::mem::{self, ManuallyDrop, offset_of};
use std::{ptr, slice};

use arcui_core::{DrawCommandKind, DrawData, Vertex};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Error, HRESULT, Result, s};

#[derive(Debug)]
pub struct Dx12Renderer {
    device: ID3D12Device,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_handle: D3D12_CPU_DESCRIPTOR_HANDLE,
    root_signature: ID3D12RootSignature,
    pipeline_state: ID3D12PipelineState,
    vertex_buffer: UploadBuffer<Vertex>,
    index_buffer: UploadBuffer<u32>,
    projection: [[f32; 4]; 4],
    format: DXGI_FORMAT,
}

impl Dx12Renderer {
    pub fn new(device: ID3D12Device, format: DXGI_FORMAT) -> Result<Self> {
        let rtv_heap: ID3D12DescriptorHeap = unsafe {
            device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                NumDescriptors: 1,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
                NodeMask: 0,
            })
        }?;
        let rtv_handle = unsafe { rtv_heap.GetCPUDescriptorHandleForHeapStart() };
        let (root_signature, pipeline_state) = unsafe { create_shader_program(&device, format) }?;

        Ok(Self {
            device: device.clone(),
            rtv_heap,
            rtv_handle,
            root_signature,
            pipeline_state,
            vertex_buffer: UploadBuffer::new(&device, 4096)?,
            index_buffer: UploadBuffer::new(&device, 8192)?,
            projection: Default::default(),
            format,
        })
    }

    pub fn format(&self) -> DXGI_FORMAT {
        self.format
    }

    pub fn render(
        &mut self,
        command_list: &ID3D12GraphicsCommandList,
        render_target: &ID3D12Resource,
        draw_data: &DrawData,
    ) -> Result<()> {
        if draw_data.lists.is_empty() {
            return Ok(());
        }

        let total_vertices: usize = draw_data.lists.iter().map(|list| list.vertices.len()).sum();
        let total_indices: usize = draw_data.lists.iter().map(|list| list.indices.len()).sum();
        if total_vertices == 0 || total_indices == 0 {
            return Ok(());
        }

        self.vertex_buffer.clear();
        self.index_buffer.clear();

        for list in &draw_data.lists {
            self.vertex_buffer.extend(list.vertices.iter().copied());
            self.index_buffer.extend(list.indices.iter().copied());
        }

        self.vertex_buffer.upload(&self.device)?;
        self.index_buffer.upload(&self.device)?;

        self.projection =
            orthographic_projection(draw_data.display_size.x, draw_data.display_size.y);

        unsafe {
            self.device
                .CreateRenderTargetView(render_target, None, self.rtv_handle);

            command_list.OMSetRenderTargets(1, Some(&self.rtv_handle), false, None);
            command_list.RSSetViewports(&[D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: draw_data.display_size.x,
                Height: draw_data.display_size.y,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]);
            command_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            command_list.SetPipelineState(&self.pipeline_state);
            command_list.SetGraphicsRootSignature(&self.root_signature);
            command_list.SetGraphicsRoot32BitConstants(
                0,
                16,
                self.projection.as_ptr() as *const c_void,
                0,
            );
            command_list.IASetVertexBuffers(
                0,
                Some(&[D3D12_VERTEX_BUFFER_VIEW {
                    BufferLocation: self.vertex_buffer.resource.GetGPUVirtualAddress(),
                    SizeInBytes: (self.vertex_buffer.len() * mem::size_of::<Vertex>()) as u32,
                    StrideInBytes: mem::size_of::<Vertex>() as u32,
                }]),
            );
            command_list.IASetIndexBuffer(Some(&D3D12_INDEX_BUFFER_VIEW {
                BufferLocation: self.index_buffer.resource.GetGPUVirtualAddress(),
                SizeInBytes: (self.index_buffer.len() * mem::size_of::<u32>()) as u32,
                Format: DXGI_FORMAT_R32_UINT,
            }));
        }

        for list in &draw_data.lists {
            for cmd in &list.commands {
                if !matches!(cmd.kind, DrawCommandKind::Primitive(_)) || cmd.index_count == 0 {
                    continue;
                }
                let clip_rect = RECT {
                    left: cmd.clip_rect.min.x.max(0.0) as i32,
                    top: cmd.clip_rect.min.y.max(0.0) as i32,
                    right: cmd.clip_rect.max.x.max(0.0) as i32,
                    bottom: cmd.clip_rect.max.y.max(0.0) as i32,
                };

                if clip_rect.right <= clip_rect.left || clip_rect.bottom <= clip_rect.top {
                    continue;
                }

                unsafe {
                    command_list.RSSetScissorRects(&[clip_rect]);
                    command_list.DrawIndexedInstanced(cmd.index_count, 1, cmd.index_start, 0, 0);
                }
            }
        }

        Ok(())
    }
}

fn orthographic_projection(width: f32, height: f32) -> [[f32; 4]; 4] {
    let right = width.max(1.0);
    let bottom = height.max(1.0);
    [
        [2.0 / right, 0.0, 0.0, 0.0],
        [0.0, -2.0 / bottom, 0.0, 0.0],
        [0.0, 0.0, 0.5, 0.0],
        [-1.0, 1.0, 0.5, 1.0],
    ]
}

unsafe fn create_shader_program(
    device: &ID3D12Device,
    format: DXGI_FORMAT,
) -> Result<(ID3D12RootSignature, ID3D12PipelineState)> {
    let parameters = [D3D12_ROOT_PARAMETER {
        ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
        Anonymous: D3D12_ROOT_PARAMETER_0 {
            Constants: D3D12_ROOT_CONSTANTS {
                ShaderRegister: 0,
                RegisterSpace: 0,
                Num32BitValues: 16,
            },
        },
        ShaderVisibility: D3D12_SHADER_VISIBILITY_VERTEX,
    }];

    let root_signature_desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: parameters.len() as u32,
        pParameters: parameters.as_ptr(),
        NumStaticSamplers: 0,
        pStaticSamplers: ptr::null(),
        Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT
            | D3D12_ROOT_SIGNATURE_FLAG_DENY_HULL_SHADER_ROOT_ACCESS
            | D3D12_ROOT_SIGNATURE_FLAG_DENY_DOMAIN_SHADER_ROOT_ACCESS
            | D3D12_ROOT_SIGNATURE_FLAG_DENY_GEOMETRY_SHADER_ROOT_ACCESS
            | D3D12_ROOT_SIGNATURE_FLAG_DENY_PIXEL_SHADER_ROOT_ACCESS,
    };

    let blob: ID3DBlob = try_out_err_blob(|v, err_blob| unsafe {
        D3D12SerializeRootSignature(
            &root_signature_desc,
            D3D_ROOT_SIGNATURE_VERSION_1,
            v,
            Some(err_blob),
        )
    })
    .map_err(|error| print_error_blob("Serializing root signature")(error))
    .expect("D3D12SerializeRootSignature");

    let root_signature: ID3D12RootSignature = unsafe {
        device.CreateRootSignature(
            0,
            slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()),
        )
    }?;

    const VS: &str = r#"
    cbuffer vertexBuffer : register(b0) {
      float4x4 ProjectionMatrix;
    };

    struct VS_INPUT {
      float2 pos: POSITION;
      float2 uv: TEXCOORD0;
      float4 col: COLOR0;
    };

    struct PS_INPUT {
      float4 pos: SV_POSITION;
      float4 col: COLOR0;
    };

    PS_INPUT main(VS_INPUT input) {
      PS_INPUT output;
      output.pos = mul(ProjectionMatrix, float4(input.pos.xy, 0.f, 1.f));
      output.col = input.col;
      return output;
    }"#;

    const PS: &str = r#"
    struct PS_INPUT {
      float4 pos: SV_POSITION;
      float4 col: COLOR0;
    };

    float4 main(PS_INPUT input): SV_Target {
      return input.col;
    }"#;

    let vertex_shader: ID3DBlob = try_out_err_blob(|v, err_blob| unsafe {
        D3DCompile(
            VS.as_ptr() as _,
            VS.len(),
            None,
            None,
            None,
            s!("main\0"),
            s!("vs_5_0\0"),
            0,
            0,
            v,
            Some(err_blob),
        )
    })
    .map_err(|error| print_error_blob("Compiling ArcUI DX12 vertex shader")(error))
    .expect("D3DCompile");

    let pixel_shader: ID3DBlob = try_out_err_blob(|v, err_blob| unsafe {
        D3DCompile(
            PS.as_ptr() as _,
            PS.len(),
            None,
            None,
            None,
            s!("main\0"),
            s!("ps_5_0\0"),
            0,
            0,
            v,
            Some(err_blob),
        )
    })
    .map_err(|error| print_error_blob("Compiling ArcUI DX12 pixel shader")(error))
    .expect("D3DCompile");

    let input_layout = [
        D3D12_INPUT_ELEMENT_DESC {
            SemanticName: s!("POSITION"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R32G32_FLOAT,
            InputSlot: 0,
            AlignedByteOffset: offset_of!(Vertex, position) as u32,
            InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        },
        D3D12_INPUT_ELEMENT_DESC {
            SemanticName: s!("TEXCOORD"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R32G32_FLOAT,
            InputSlot: 0,
            AlignedByteOffset: offset_of!(Vertex, uv) as u32,
            InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        },
        D3D12_INPUT_ELEMENT_DESC {
            SemanticName: s!("COLOR"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            InputSlot: 0,
            AlignedByteOffset: offset_of!(Vertex, color) as u32,
            InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        },
    ];

    let pipeline_state_desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
        pRootSignature: ManuallyDrop::new(Some(root_signature.clone())),
        VS: D3D12_SHADER_BYTECODE {
            pShaderBytecode: unsafe { vertex_shader.GetBufferPointer() },
            BytecodeLength: unsafe { vertex_shader.GetBufferSize() },
        },
        PS: D3D12_SHADER_BYTECODE {
            pShaderBytecode: unsafe { pixel_shader.GetBufferPointer() },
            BytecodeLength: unsafe { pixel_shader.GetBufferSize() },
        },
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
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
            ],
        },
        SampleMask: u32::MAX,
        RasterizerState: D3D12_RASTERIZER_DESC {
            FillMode: D3D12_FILL_MODE_SOLID,
            CullMode: D3D12_CULL_MODE_NONE,
            FrontCounterClockwise: false.into(),
            DepthBias: D3D12_DEFAULT_DEPTH_BIAS,
            DepthBiasClamp: D3D12_DEFAULT_DEPTH_BIAS_CLAMP,
            SlopeScaledDepthBias: D3D12_DEFAULT_SLOPE_SCALED_DEPTH_BIAS,
            DepthClipEnable: true.into(),
            MultisampleEnable: false.into(),
            AntialiasedLineEnable: false.into(),
            ForcedSampleCount: 0,
            ConservativeRaster: D3D12_CONSERVATIVE_RASTERIZATION_MODE_OFF,
        },
        DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
            DepthEnable: false.into(),
            StencilEnable: false.into(),
            ..Default::default()
        },
        InputLayout: D3D12_INPUT_LAYOUT_DESC {
            pInputElementDescs: input_layout.as_ptr(),
            NumElements: input_layout.len() as u32,
        },
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
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        ..Default::default()
    };

    let pipeline_state = unsafe { device.CreateGraphicsPipelineState(&pipeline_state_desc) }?;
    let _ = ManuallyDrop::into_inner(pipeline_state_desc.pRootSignature);
    Ok((root_signature, pipeline_state))
}

#[derive(Debug)]
struct UploadBuffer<T> {
    resource: ID3D12Resource,
    data: Vec<T>,
    capacity: usize,
}

impl<T> UploadBuffer<T> {
    fn new(device: &ID3D12Device, capacity: usize) -> Result<Self> {
        Ok(Self {
            resource: create_upload_buffer(device, capacity, mem::size_of::<T>())?,
            data: Vec::with_capacity(capacity),
            capacity,
        })
    }

    fn clear(&mut self) {
        self.data.clear();
    }

    fn extend<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = T>,
    {
        self.data.extend(iter);
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn upload(&mut self, device: &ID3D12Device) -> Result<()> {
        if self.data.capacity() > self.capacity {
            self.capacity = self.data.capacity();
            self.resource = create_upload_buffer(device, self.capacity, mem::size_of::<T>())?;
        }

        unsafe {
            let mut mapped = ptr::null_mut();
            self.resource.Map(0, None, Some(&mut mapped))?;
            ptr::copy_nonoverlapping(self.data.as_ptr(), mapped as *mut T, self.data.len());
            self.resource.Unmap(0, None);
        }

        Ok(())
    }
}

fn create_upload_buffer(
    device: &ID3D12Device,
    capacity: usize,
    item_size: usize,
) -> Result<ID3D12Resource> {
    let width = (capacity.max(1) * item_size.max(1)) as u64;
    unsafe {
        try_out_ptr(|v| {
            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_UPLOAD,
                    CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
                    MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
                    CreationNodeMask: 0,
                    VisibleNodeMask: 0,
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Alignment: 0,
                    Width: width,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_UNKNOWN,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    Flags: D3D12_RESOURCE_FLAG_NONE,
                },
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                v,
            )
        })
    }
}

fn try_out_ptr<T, F>(mut f: F) -> Result<T>
where
    F: FnMut(&mut Option<T>) -> Result<()>,
{
    let mut out = None;
    f(&mut out)?;
    out.ok_or_else(|| Error::from_hresult(HRESULT(-1)))
}

fn try_out_err_blob<T1, T2, F>(mut f: F) -> std::result::Result<T1, (windows::core::Error, T2)>
where
    F: FnMut(&mut Option<T1>, &mut Option<T2>) -> Result<()>,
{
    let mut t1 = None;
    let mut t2 = None;
    match f(&mut t1, &mut t2) {
        Ok(_) => t1.ok_or_else(|| (Error::from_hresult(HRESULT(-1)), t2.unwrap())),
        Err(e) => Err((e, t2.unwrap())),
    }
}

fn print_error_blob(
    label: &'static str,
) -> impl Fn((windows::core::Error, ID3DBlob)) -> windows::core::Error {
    move |(error, blob)| {
        let bytes = unsafe {
            slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
        };
        let message = String::from_utf8_lossy(bytes);
        eprintln!("{label}: {message}");
        error
    }
}

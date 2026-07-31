use arcui_core::DrawData;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::core::Result;

#[derive(Debug)]
pub struct Dx11Renderer {
    device: ID3D11Device,
}

impl Dx11Renderer {
    pub fn new(device: ID3D11Device) -> Self {
        Self { device }
    }

    pub fn render(&mut self, draw_data: &DrawData) -> Result<()> {
        let _ = (&self.device, draw_data);
        Ok(())
    }
}

use crate::geometry::{Rect, Vec2};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const WHITE: Self = Self::rgba(255, 255, 255, 255);
    pub const BLACK: Self = Self::rgba(0, 0, 0, 255);
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self((r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub position: Vec2,
    pub uv: Vec2,
    pub color: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineSegment {
    pub start: Vec2,
    pub end: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VectorIcon {
    pub viewport: f32,
    pub stroke_width: f32,
    pub segments: &'static [LineSegment],
}

#[derive(Clone, Debug, PartialEq)]
pub enum DrawPrimitive {
    Rect,
    RoundedRect { radius: f32 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum DrawCommandKind {
    Primitive(DrawPrimitive),
    Text {
        text: String,
        color: Color,
    },
    VectorIcon {
        icon: &'static VectorIcon,
        color: Color,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawCmd {
    pub clip_rect: Rect,
    pub texture_id: u64,
    pub index_start: u32,
    pub index_count: u32,
    pub bounds: Rect,
    pub kind: DrawCommandKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawList {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub commands: Vec<DrawCmd>,
}

impl DrawList {
    pub fn push_filled_rect(&mut self, rect: Rect, color: Color) {
        self.push_filled_rect_clipped(rect, color, rect);
    }

    pub fn push_filled_rect_clipped(&mut self, rect: Rect, color: Color, clip_rect: Rect) {
        self.push_shape(rect, color, clip_rect, DrawPrimitive::Rect);
    }

    pub fn push_rounded_rect(&mut self, rect: Rect, color: Color, radius: f32) {
        self.push_rounded_rect_clipped(rect, color, radius, rect);
    }

    pub fn push_rounded_rect_clipped(
        &mut self,
        rect: Rect,
        color: Color,
        radius: f32,
        clip_rect: Rect,
    ) {
        self.push_shape(
            rect,
            color,
            clip_rect,
            DrawPrimitive::RoundedRect {
                radius: radius.max(0.0),
            },
        );
    }

    pub fn push_text(
        &mut self,
        rect: Rect,
        text: impl Into<String>,
        color: Color,
        clip_rect: Rect,
    ) {
        self.commands.push(DrawCmd {
            clip_rect,
            texture_id: 0,
            index_start: 0,
            index_count: 0,
            bounds: rect,
            kind: DrawCommandKind::Text {
                text: text.into(),
                color,
            },
        });
    }

    pub fn push_vector_icon(
        &mut self,
        rect: Rect,
        icon: &'static VectorIcon,
        color: Color,
        clip_rect: Rect,
    ) {
        self.commands.push(DrawCmd {
            clip_rect,
            texture_id: 0,
            index_start: 0,
            index_count: 0,
            bounds: rect,
            kind: DrawCommandKind::VectorIcon { icon, color },
        });
    }

    fn push_shape(&mut self, rect: Rect, color: Color, clip_rect: Rect, primitive: DrawPrimitive) {
        let index_start = self.indices.len() as u32;
        let vertex_start = self.vertices.len() as u32;

        self.vertices.extend_from_slice(&[
            Vertex {
                position: rect.min,
                uv: Vec2::new(0.0, 0.0),
                color: color.0,
            },
            Vertex {
                position: Vec2::new(rect.max.x, rect.min.y),
                uv: Vec2::new(1.0, 0.0),
                color: color.0,
            },
            Vertex {
                position: rect.max,
                uv: Vec2::new(1.0, 1.0),
                color: color.0,
            },
            Vertex {
                position: Vec2::new(rect.min.x, rect.max.y),
                uv: Vec2::new(0.0, 1.0),
                color: color.0,
            },
        ]);

        self.indices.extend_from_slice(&[
            vertex_start,
            vertex_start + 1,
            vertex_start + 2,
            vertex_start,
            vertex_start + 2,
            vertex_start + 3,
        ]);

        self.commands.push(DrawCmd {
            clip_rect,
            texture_id: 0,
            index_start,
            index_count: 6,
            bounds: rect,
            kind: DrawCommandKind::Primitive(primitive),
        });
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawData {
    pub display_size: Vec2,
    pub lists: Vec<DrawList>,
}

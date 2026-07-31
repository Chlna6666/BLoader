#![forbid(unsafe_code)]

pub mod animation;
pub mod draw;
pub mod geometry;
pub mod input;
pub mod widget;

pub use animation::{animate_scalar, ease_out_cubic};
pub use draw::{
    Color, DrawCmd, DrawCommandKind, DrawData, DrawList, DrawPrimitive, LineSegment, VectorIcon,
    Vertex,
};
pub use geometry::{Rect, Vec2};
pub use input::{InputSnapshot, Key, MouseButton};
pub use widget::{ButtonColors, Frame, Memory, Response, Ui, WindowOptions};

use arcui_core::{ButtonColors, Color, Rect, Response, Ui, Vec2, VectorIcon};

pub use arcui_core::VectorIcon as Icon;

include!(concat!(env!("OUT_DIR"), "/icons_gen.rs"));

pub fn draw_icon_centered(
    ui: &mut Ui<'_>,
    rect: Rect,
    icon: &'static VectorIcon,
    color: Color,
    clip_rect: Rect,
) {
    let size = rect.width().min(rect.height()).max(0.0);
    let icon_rect = Rect::from_min_size(
        Vec2::new(
            rect.min.x + ((rect.width() - size) * 0.5),
            rect.min.y + ((rect.height() - size) * 0.5),
        ),
        Vec2::splat(size),
    );
    ui.vector_icon_in_rect(icon_rect, icon, color, clip_rect);
}

pub fn icon_button_in_rect(
    ui: &mut Ui<'_>,
    id_source: &str,
    rect: Rect,
    icon: &'static VectorIcon,
    clip_rect: Rect,
    colors: ButtonColors,
    icon_color: Color,
) -> Response {
    let response = ui.button_in_rect_clipped(id_source, rect, "", clip_rect, colors);
    draw_icon_centered(ui, rect.shrink(8.0), icon, icon_color, clip_rect);
    response
}

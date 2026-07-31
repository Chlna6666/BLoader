use arcui_core::widget::text_size;
use arcui_core::{Color, Rect, Ui, Vec2};

use crate::bl::host;
use crate::core::symbol_diagnostics;

const MARGIN_BOTTOM: f32 = 10.0;
const PADDING_X: f32 = 8.0;
const PADDING_Y: f32 = 5.0;

pub const fn is_enabled() -> bool {
    true
}

pub fn render_in_arcui(ui: &mut Ui<'_>) {
    let text = status_text(
        crate::runtime::foundation::build_info::VERSION,
        host::loaded_mods().len(),
        symbol_diagnostics::is_ready(),
    );
    let text_size = text_size(&text, 2.0);
    let rect = status_rect(ui.display_size(), text_size);

    ui.rounded_rect(rect, Color::rgba(7, 11, 8, 172), 4.0);
    ui.rounded_rect(
        Rect::from_min_max(rect.min, Vec2::new(rect.min.x + 3.0, rect.max.y)),
        Color::rgba(145, 202, 118, 224),
        3.0,
    );
    ui.text_at(
        &text,
        Vec2::new(rect.min.x + PADDING_X + 3.0, rect.min.y + PADDING_Y),
        Color::rgba(238, 238, 238, 240),
    );
}

fn status_text(version: &str, mod_count: usize, symbols_ready: bool) -> String {
    let mod_label = if mod_count == 1 { "mod" } else { "mods" };
    let symbols = if symbols_ready { "ready" } else { "missing" };
    format!("BLoader {version} | {mod_count} {mod_label} | symbols: {symbols}")
}

fn status_rect(display_size: Vec2, label_size: Vec2) -> Rect {
    let width = label_size.x + PADDING_X * 2.0 + 3.0;
    let height = label_size.y + PADDING_Y * 2.0;
    let x = ((display_size.x - width) * 0.5).max(0.0);
    let y = (display_size.y - height - MARGIN_BOTTOM).max(0.0);
    Rect::from_min_size(
        Vec2::new(x, y),
        Vec2::new(width.min(display_size.x), height),
    )
}

#[cfg(test)]
mod tests {
    use super::{status_rect, status_text};
    use arcui_core::Vec2;

    #[test]
    fn status_text_includes_loader_mod_count_and_symbol_state() {
        assert_eq!(
            status_text("0.2.2", 1, true),
            "BLoader 0.2.2 | 1 mod | symbols: ready"
        );
        assert_eq!(
            status_text("0.2.2", 0, false),
            "BLoader 0.2.2 | 0 mods | symbols: missing"
        );
    }

    #[test]
    fn status_rect_stays_on_screen_and_is_centered() {
        let rect = status_rect(Vec2::new(1280.0, 720.0), Vec2::new(320.0, 18.0));
        assert_eq!(rect.min.x, 470.5);
        assert_eq!(rect.max.x, 809.5);
        assert!(rect.min.y >= 0.0);
        assert!(rect.max.y <= 720.0);
    }
}

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use arcui_core::widget::text_size;
use arcui_core::{Color, Rect, Ui as ArcUi, Vec2};

use crate::bl::abi::BlStringView;
use crate::bl::host;
use crate::runtime::foundation::logging;

static TEXT_FRAME_ACTIVE: AtomicBool = AtomicBool::new(false);
static FIRST_FRAME_LOGGED: AtomicBool = AtomicBool::new(false);
static DOCUMENT: OnceLock<Mutex<OverlayDocument>> = OnceLock::new();

#[derive(Clone, Default)]
struct TextBlock {
    x: f32,
    y: f32,
    lines: Vec<String>,
}

#[derive(Default)]
struct OverlayDocument {
    blocks: Vec<TextBlock>,
    current: Option<TextBlock>,
}

fn document() -> &'static Mutex<OverlayDocument> {
    DOCUMENT.get_or_init(|| Mutex::new(OverlayDocument::default()))
}

pub fn ensure_started() {}

pub fn render_in_arcui(ui: &mut ArcUi<'_>) {
    begin_frame();
    TEXT_FRAME_ACTIVE.store(true, Ordering::SeqCst);
    host::dispatch_text_panels();
    TEXT_FRAME_ACTIVE.store(false, Ordering::SeqCst);
    end_frame();

    let body = Color::rgba(238, 238, 238, 255);
    let accent = Color::rgba(166, 220, 137, 255);
    let panel_bg = Color::rgba(10, 10, 10, 132);
    let panel_border = Color::rgba(255, 255, 255, 24);
    let line_height = 22.0f32;
    let padding_x = 7.0f32;
    let padding_y = 6.0f32;

    let doc = document().lock().unwrap_or_else(|e| e.into_inner());
    for block in &doc.blocks {
        let mut max_width = 0.0f32;
        for line in &block.lines {
            max_width = max_width.max(text_size(line, 2.0).x);
        }

        let height = (block.lines.len() as f32 * line_height) + padding_y * 2.0;
        let outer = Rect::from_min_max(
            Vec2::new(block.x - padding_x - 1.0, block.y - padding_y - 1.0),
            Vec2::new(
                block.x + max_width + padding_x + 1.0,
                block.y + height - 4.0 + 1.0,
            ),
        );
        let inner = Rect::from_min_max(
            Vec2::new(block.x - padding_x, block.y - padding_y),
            Vec2::new(block.x + max_width + padding_x, block.y + height - 4.0),
        );

        ui.rounded_rect(outer, panel_border, 6.0);
        ui.rounded_rect(inner, panel_bg, 6.0);

        let mut y = block.y;
        for (index, line) in block.lines.iter().enumerate() {
            let color = if index == 0 { accent } else { body };
            ui.text_at(line, Vec2::new(block.x, y), color);
            y += line_height;
        }
    }
}

fn begin_frame() {
    let mut doc = document().lock().unwrap_or_else(|e| e.into_inner());
    doc.blocks.clear();
    doc.current = None;
}

fn end_frame() {
    let mut doc = document().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(block) = doc.current.take() {
        doc.blocks.push(block);
    }
    if !FIRST_FRAME_LOGGED.swap(true, Ordering::Relaxed) && !doc.blocks.is_empty() {
        logging::info_message(&format!(
            "BL text overlay rendered first in-game frame with {} block(s).",
            doc.blocks.len()
        ));
    }
}

pub unsafe extern "system" fn host_hud_begin_block(_id: BlStringView, x: i32, y: i32) -> bool {
    if !TEXT_FRAME_ACTIVE.load(Ordering::SeqCst) {
        return false;
    }

    let mut doc = document().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(block) = doc.current.take() {
        doc.blocks.push(block);
    }
    doc.current = Some(TextBlock {
        x: x as f32,
        y: y as f32,
        lines: Vec::new(),
    });
    true
}

pub unsafe extern "system" fn host_hud_text_line(text: BlStringView) {
    if !TEXT_FRAME_ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let Some(text) = view_to_string(text) else {
        return;
    };

    let mut doc = document().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(block) = doc.current.as_mut() {
        block.lines.push(text);
    }
}

pub unsafe extern "system" fn host_hud_end_block() {
    if !TEXT_FRAME_ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let mut doc = document().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(block) = doc.current.take() {
        doc.blocks.push(block);
    }
}

fn view_to_string(view: BlStringView) -> Option<String> {
    if view.ptr.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(view.ptr as *const u8, view.len) };
    let c = CString::new(bytes).ok()?;
    Some(c.to_string_lossy().to_string())
}

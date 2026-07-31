use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::draw::{Color, DrawData, DrawList, VectorIcon};
use crate::geometry::{Rect, Vec2};
use crate::input::{InputSnapshot, Key, MouseButton};

const TITLE_BAR_HEIGHT: f32 = 28.0;
const WINDOW_PADDING: f32 = 12.0;
const ITEM_HEIGHT: f32 = 28.0;
const ITEM_SPACING: f32 = 8.0;
const RESIZE_GRIP_SIZE: f32 = 16.0;
const TEXT_SCALE: f32 = 2.0;
const BASE_FONT_SIZE: f32 = 8.0;
const BASE_LINE_GAP: f32 = 2.0;
const CARET_WIDTH: f32 = 2.0;
const WINDOW_RADIUS: f32 = 16.0;
const BUTTON_RADIUS: f32 = 10.0;
const SWITCH_RADIUS: f32 = 12.0;
const SLIDER_HEIGHT: f32 = 44.0;
const SLIDER_TRACK_HEIGHT: f32 = 8.0;
const SHADOW_LAYERS: [(f32, u8); 3] = [(18.0, 18), (10.0, 24), (4.0, 30)];

#[derive(Clone, Debug)]
struct WindowMemory {
    position: Vec2,
    size: Vec2,
}

#[derive(Clone, Debug)]
enum ActiveInteraction {
    WindowDrag { id: u64, grab_offset: Vec2 },
    WindowResize { id: u64, resize_offset: Vec2 },
    Button { id: u64 },
    DragFloat { id: u64 },
}

#[derive(Debug, Default)]
pub struct Memory {
    windows: HashMap<u64, WindowMemory>,
    active: Option<ActiveInteraction>,
    focused_text: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub hovered: bool,
    pub active: bool,
    pub clicked: bool,
    pub changed: bool,
    pub submitted: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ButtonColors {
    pub normal: Color,
    pub hovered: Color,
    pub active: Color,
    pub text: Color,
}

impl ButtonColors {
    pub const fn new(normal: Color, hovered: Color, active: Color, text: Color) -> Self {
        Self {
            normal,
            hovered,
            active,
            text,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WindowVisuals {
    pub body: Color,
    pub title_bar: Color,
    pub title_text: Color,
    pub shadow: bool,
    pub resize_grip: bool,
}

impl Default for WindowVisuals {
    fn default() -> Self {
        Self {
            body: Color::rgba(9, 14, 24, 224),
            title_bar: Color::rgba(56, 123, 255, 255),
            title_text: Color::WHITE,
            shadow: true,
            resize_grip: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WindowOptions {
    pub position: Vec2,
    pub size: Vec2,
    pub movable: bool,
    pub resizable: bool,
    pub visuals: WindowVisuals,
}

impl WindowOptions {
    pub fn new(position: Vec2, size: Vec2) -> Self {
        Self {
            position,
            size,
            movable: true,
            resizable: true,
            visuals: WindowVisuals::default(),
        }
    }
}

#[derive(Debug)]
struct WindowContext {
    id: u64,
    body_rect: Rect,
    content_rect: Rect,
    cursor: Vec2,
}

#[derive(Debug)]
pub struct Ui<'a> {
    input: InputSnapshot,
    draw_data: DrawData,
    memory: &'a mut Memory,
    current_window: Option<WindowContext>,
}

impl<'a> Ui<'a> {
    pub fn new(input: InputSnapshot, memory: &'a mut Memory) -> Self {
        if !input.mouse_down(MouseButton::Left) && !input.mouse_released(MouseButton::Left) {
            memory.active = None;
        }

        Self {
            draw_data: DrawData {
                display_size: input.display_size,
                ..DrawData::default()
            },
            input,
            memory,
            current_window: None,
        }
    }

    pub fn input(&self) -> &InputSnapshot {
        &self.input
    }

    pub fn draw_data(&self) -> &DrawData {
        &self.draw_data
    }

    pub fn display_size(&self) -> Vec2 {
        self.input.display_size
    }

    pub fn filled_rect(&mut self, rect: Rect, color: Color) {
        self.ensure_draw_list().push_filled_rect(rect, color);
    }

    pub fn filled_rect_clipped(&mut self, rect: Rect, color: Color, clip_rect: Rect) {
        self.ensure_draw_list()
            .push_filled_rect_clipped(rect, color, clip_rect);
    }

    pub fn rounded_rect(&mut self, rect: Rect, color: Color, radius: f32) {
        self.ensure_draw_list()
            .push_rounded_rect(rect, color, radius);
    }

    pub fn rounded_rect_clipped(&mut self, rect: Rect, color: Color, radius: f32, clip_rect: Rect) {
        self.ensure_draw_list()
            .push_rounded_rect_clipped(rect, color, radius, clip_rect);
    }

    pub fn shadowed_rounded_rect(&mut self, rect: Rect, color: Color, radius: f32) {
        for (spread, alpha) in SHADOW_LAYERS {
            let shadow_rect = Rect::from_min_max(
                Vec2::new(rect.min.x - spread, rect.min.y - spread * 0.35),
                Vec2::new(rect.max.x + spread, rect.max.y + spread),
            );
            self.ensure_draw_list().push_rounded_rect(
                shadow_rect,
                Color::rgba(0, 0, 0, alpha),
                radius + spread * 0.55,
            );
        }
        self.rounded_rect(rect, color, radius);
    }

    pub fn text(&mut self, text: &str, color: Color) {
        let height = text_size(text, TEXT_SCALE).y;
        let (rect, clip_rect) = self.layout_rect(0.0, height);
        self.draw_text_clipped(
            text,
            rect.min,
            color,
            TEXT_SCALE,
            clip_rect,
            rect.width(),
            false,
        );
    }

    pub fn label(&mut self, text: &str) {
        self.text(text, Color::rgba(228, 233, 242, 255));
    }

    pub fn text_at(&mut self, text: &str, position: Vec2, color: Color) {
        let clip = self.window_clip_rect();
        self.draw_text(text, position, color, TEXT_SCALE, clip);
    }

    pub fn text_at_clipped(&mut self, text: &str, position: Vec2, color: Color, clip_rect: Rect) {
        self.draw_text(text, position, color, TEXT_SCALE, clip_rect);
    }

    pub fn text_at_width(&mut self, text: &str, position: Vec2, max_width: f32, color: Color) {
        let clip = self.window_clip_rect();
        self.draw_text_clipped(text, position, color, TEXT_SCALE, clip, max_width, true);
    }

    pub fn text_at_width_clipped(
        &mut self,
        text: &str,
        position: Vec2,
        max_width: f32,
        color: Color,
        clip_rect: Rect,
    ) {
        self.draw_text_clipped(
            text, position, color, TEXT_SCALE, clip_rect, max_width, true,
        );
    }

    pub fn button(&mut self, label: &str) -> Response {
        let (rect, clip_rect) = self.layout_rect(0.0, ITEM_HEIGHT);
        self.button_in_rect_with_colors(
            &format!("button:{label}"),
            rect,
            label,
            clip_rect,
            ButtonColors::new(
                Color::rgba(33, 89, 188, 255),
                Color::rgba(49, 116, 227, 255),
                Color::rgba(40, 104, 214, 255),
                Color::WHITE,
            ),
        )
    }

    pub fn button_in_rect(
        &mut self,
        id_source: &str,
        rect: Rect,
        label: &str,
        colors: ButtonColors,
    ) -> Response {
        self.button_in_rect_with_colors(id_source, rect, label, self.window_clip_rect(), colors)
    }

    pub fn button_in_rect_clipped(
        &mut self,
        id_source: &str,
        rect: Rect,
        label: &str,
        clip_rect: Rect,
        colors: ButtonColors,
    ) -> Response {
        self.button_in_rect_with_colors(id_source, rect, label, clip_rect, colors)
    }

    pub fn selectable_in_rect(
        &mut self,
        id_source: &str,
        rect: Rect,
        label: &str,
        selected: bool,
    ) -> Response {
        let colors = if selected {
            ButtonColors::new(
                Color::rgba(26, 32, 46, 220),
                Color::rgba(26, 32, 46, 220),
                Color::rgba(26, 32, 46, 220),
                Color::WHITE,
            )
        } else {
            ButtonColors::new(
                Color::rgba(0, 0, 0, 0),
                Color::rgba(255, 255, 255, 18),
                Color::rgba(255, 255, 255, 28),
                Color::rgba(204, 212, 228, 255),
            )
        };

        let response = self.button_in_rect(id_source, rect, label, colors);
        if selected {
            let accent = Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height()));
            self.ensure_draw_list()
                .push_filled_rect(accent, Color::rgba(67, 130, 255, 255));
        }
        response
    }

    pub fn input_text(&mut self, label: &str, value: &mut String) -> Response {
        let id = self.widget_id(label, "input_text");
        let label_height = text_size(label, TEXT_SCALE).y;
        let (label_rect, clip_rect) = self.layout_rect(0.0, label_height);
        self.draw_text(
            label,
            label_rect.min,
            Color::rgba(203, 210, 224, 255),
            TEXT_SCALE,
            clip_rect,
        );

        let (rect, clip_rect) = self.layout_rect(0.0, ITEM_HEIGHT);
        let hovered = rect.contains(self.input.mouse_position);
        let focused_before = self.memory.focused_text == Some(id);
        let mut changed = false;
        let mut submitted = false;

        if hovered && self.input.mouse_pressed(MouseButton::Left) {
            self.memory.focused_text = Some(id);
        } else if self.input.mouse_pressed(MouseButton::Left) && !hovered && focused_before {
            self.memory.focused_text = None;
        }

        let focused = self.memory.focused_text == Some(id);
        if focused {
            for ch in self.input.typed_text.chars() {
                if !ch.is_control() {
                    value.push(ch);
                    changed = true;
                }
            }
            if self.input.key_pressed(Key::Backspace) && value.pop().is_some() {
                changed = true;
            }
            if self.input.key_pressed(Key::Delete) && !value.is_empty() {
                value.clear();
                changed = true;
            }
            if self.input.key_pressed(Key::Enter) {
                submitted = true;
            }
        }

        let fill = if focused {
            Color::rgba(24, 30, 44, 255)
        } else if hovered {
            Color::rgba(19, 24, 35, 255)
        } else {
            Color::rgba(14, 18, 28, 255)
        };
        self.ensure_draw_list()
            .push_rounded_rect_clipped(rect, fill, BUTTON_RADIUS, clip_rect);

        let text_height = text_size(value, TEXT_SCALE)
            .y
            .max(text_size(" ", TEXT_SCALE).y);
        let text_y = rect.min.y + ((rect.height() - text_height) * 0.5).floor();
        self.draw_text_clipped(
            value,
            Vec2::new(rect.min.x + 8.0, text_y),
            Color::WHITE,
            TEXT_SCALE,
            clip_rect,
            (rect.width() - 18.0).max(0.0),
            false,
        );

        if focused {
            let caret_x = rect.min.x + 8.0 + text_size(value, TEXT_SCALE).x + 2.0;
            let caret_height = (text_line_height(TEXT_SCALE) - 4.0).max(10.0);
            let caret_rect = Rect::from_min_size(
                Vec2::new(
                    caret_x,
                    rect.min.y + ((rect.height() - caret_height) * 0.5).floor(),
                ),
                Vec2::new(CARET_WIDTH, caret_height),
            );
            self.ensure_draw_list()
                .push_filled_rect_clipped(caret_rect, Color::WHITE, clip_rect);
        }

        Response {
            hovered,
            active: focused,
            changed,
            submitted,
            ..Response::default()
        }
    }

    pub fn switch_in_rect(&mut self, id_source: &str, rect: Rect, value: &mut bool) -> Response {
        self.switch_in_rect_clipped(id_source, rect, self.window_clip_rect(), value)
    }

    pub fn switch_in_rect_clipped(
        &mut self,
        id_source: &str,
        rect: Rect,
        clip_rect: Rect,
        value: &mut bool,
    ) -> Response {
        let id = self.widget_id(id_source, "switch");
        let visible = rect.intersects(clip_rect);
        let hovered = visible
            && clip_rect.contains(self.input.mouse_position)
            && rect.contains(self.input.mouse_position);
        if hovered && self.input.mouse_pressed(MouseButton::Left) {
            self.memory.active = Some(ActiveInteraction::Button { id });
        }

        let active = matches!(
            self.memory.active,
            Some(ActiveInteraction::Button { id: active_id }) if active_id == id
        );
        let clicked = active && hovered && self.input.mouse_released(MouseButton::Left);
        if clicked {
            *value = !*value;
        }

        let track_color = if *value {
            Color::rgba(59, 130, 246, 255)
        } else {
            Color::rgba(83, 91, 110, 200)
        };
        self.ensure_draw_list().push_rounded_rect_clipped(
            rect,
            track_color,
            SWITCH_RADIUS,
            clip_rect,
        );

        let knob_x = if *value {
            rect.max.x - rect.height() + 2.0
        } else {
            rect.min.x + 2.0
        };
        let knob = Rect::from_min_size(
            Vec2::new(knob_x, rect.min.y + 2.0),
            Vec2::new(rect.height() - 4.0, rect.height() - 4.0),
        );
        self.ensure_draw_list().push_rounded_rect_clipped(
            knob,
            Color::rgba(248, 250, 252, 255),
            knob.height() * 0.5,
            clip_rect,
        );

        Response {
            hovered,
            active,
            clicked,
            changed: clicked,
            ..Response::default()
        }
    }

    pub fn begin_window(&mut self, title: &str, options: WindowOptions) -> bool {
        let id = hash_id(("window", title));
        let mouse = self.input.mouse_position;
        let window = self
            .memory
            .windows
            .entry(id)
            .or_insert_with(|| WindowMemory {
                position: options.position,
                size: options.size,
            });

        if !options.movable {
            window.position = options.position;
        }
        if !options.resizable {
            window.size = options.size;
        }

        window.size.x = window.size.x.max(220.0);
        window.size.y = window.size.y.max(140.0);

        let rect = Rect::from_min_size(window.position, window.size);
        let title_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), TITLE_BAR_HEIGHT));
        let resize_rect = Rect::from_min_size(
            Vec2::new(rect.max.x - RESIZE_GRIP_SIZE, rect.max.y - RESIZE_GRIP_SIZE),
            Vec2::splat(RESIZE_GRIP_SIZE),
        );

        if options.movable
            && title_rect.contains(mouse)
            && self.input.mouse_pressed(MouseButton::Left)
        {
            self.memory.active = Some(ActiveInteraction::WindowDrag {
                id,
                grab_offset: mouse - window.position,
            });
        }

        if options.resizable
            && resize_rect.contains(mouse)
            && self.input.mouse_pressed(MouseButton::Left)
        {
            self.memory.active = Some(ActiveInteraction::WindowResize {
                id,
                resize_offset: window.size - (mouse - window.position),
            });
        }

        match self.memory.active {
            Some(ActiveInteraction::WindowDrag {
                id: active_id,
                grab_offset,
            }) if active_id == id => {
                if self.input.mouse_down(MouseButton::Left) {
                    window.position = mouse - grab_offset;
                }
            }
            Some(ActiveInteraction::WindowResize {
                id: active_id,
                resize_offset,
            }) if active_id == id => {
                if self.input.mouse_down(MouseButton::Left) {
                    let size = mouse - window.position + resize_offset;
                    window.size = Vec2::new(size.x.max(220.0), size.y.max(140.0));
                }
            }
            _ => {}
        }

        let rect = Rect::from_min_size(window.position, window.size);
        let title_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), TITLE_BAR_HEIGHT));
        let body_rect = Rect::from_min_max(
            Vec2::new(rect.min.x, rect.min.y + TITLE_BAR_HEIGHT),
            rect.max,
        );

        if options.visuals.shadow {
            self.shadowed_rounded_rect(rect, options.visuals.body, WINDOW_RADIUS);
        } else {
            self.rounded_rect(rect, options.visuals.body, WINDOW_RADIUS);
        }
        self.ensure_draw_list().push_rounded_rect_clipped(
            title_rect,
            options.visuals.title_bar,
            WINDOW_RADIUS,
            title_rect,
        );

        let title_height = text_size(title, TEXT_SCALE).y;
        let title_y = title_rect.min.y + ((title_rect.height() - title_height) * 0.5).floor();
        self.draw_text_clipped(
            title,
            Vec2::new(title_rect.min.x + 10.0, title_y),
            options.visuals.title_text,
            TEXT_SCALE,
            title_rect,
            (title_rect.width() - 20.0).max(0.0),
            true,
        );

        if options.resizable && options.visuals.resize_grip {
            let grip_a = Rect::from_min_size(
                Vec2::new(rect.max.x - 10.0, rect.max.y - 10.0),
                Vec2::splat(4.0),
            );
            let grip_b = Rect::from_min_size(
                Vec2::new(rect.max.x - 16.0, rect.max.y - 10.0),
                Vec2::splat(4.0),
            );
            let grip_c = Rect::from_min_size(
                Vec2::new(rect.max.x - 10.0, rect.max.y - 16.0),
                Vec2::splat(4.0),
            );
            self.ensure_draw_list()
                .push_filled_rect(grip_a, Color::rgba(205, 220, 255, 255));
            self.ensure_draw_list()
                .push_filled_rect(grip_b, Color::rgba(205, 220, 255, 255));
            self.ensure_draw_list()
                .push_filled_rect(grip_c, Color::rgba(205, 220, 255, 255));
        }

        self.current_window = Some(WindowContext {
            id,
            body_rect,
            content_rect: body_rect.shrink(WINDOW_PADDING),
            cursor: Vec2::new(
                body_rect.min.x + WINDOW_PADDING,
                body_rect.min.y + WINDOW_PADDING,
            ),
        });
        true
    }

    pub fn end_window(&mut self) {
        self.current_window = None;
    }

    pub fn current_window_body_rect(&self) -> Rect {
        self.current_window
            .as_ref()
            .map(|window| window.body_rect)
            .expect("window required")
    }

    pub fn current_window_content_rect(&self) -> Rect {
        self.current_window
            .as_ref()
            .map(|window| window.content_rect)
            .expect("window required")
    }

    pub fn cursor(&self) -> Vec2 {
        self.current_window
            .as_ref()
            .map(|window| window.cursor)
            .expect("window required")
    }

    pub fn set_cursor(&mut self, cursor: Vec2) {
        if let Some(window) = self.current_window.as_mut() {
            window.cursor = cursor;
        }
    }

    pub fn advance_cursor(&mut self, delta_y: f32) {
        if let Some(window) = self.current_window.as_mut() {
            window.cursor.y += delta_y;
        }
    }

    pub fn separator(&mut self, color: Color) {
        let (rect, clip_rect) = self.layout_rect(0.0, 1.0);
        self.ensure_draw_list()
            .push_filled_rect_clipped(rect, color, clip_rect);
        self.advance_cursor(ITEM_SPACING);
    }

    pub fn spacer(&mut self, height: f32) {
        self.advance_cursor(height);
    }

    pub fn progress_bar_in_rect(
        &mut self,
        rect: Rect,
        value: f32,
        min: f32,
        max: f32,
        clip_rect: Rect,
    ) {
        let track_rect = Rect::from_min_size(
            Vec2::new(
                rect.min.x,
                rect.min.y + ((rect.height() - SLIDER_TRACK_HEIGHT) * 0.5).floor(),
            ),
            Vec2::new(rect.width(), SLIDER_TRACK_HEIGHT),
        );
        self.ensure_draw_list().push_rounded_rect_clipped(
            track_rect,
            Color::rgba(58, 66, 82, 220),
            SLIDER_TRACK_HEIGHT * 0.5,
            clip_rect,
        );

        let ratio = if max > min {
            ((value - min) / (max - min)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let fill_rect = Rect::from_min_size(
            track_rect.min,
            Vec2::new(track_rect.width() * ratio, track_rect.height()),
        );
        self.ensure_draw_list().push_rounded_rect_clipped(
            fill_rect,
            Color::rgba(116, 171, 255, 255),
            SLIDER_TRACK_HEIGHT * 0.5,
            clip_rect,
        );
    }

    pub fn vector_icon_in_rect(
        &mut self,
        rect: Rect,
        icon: &'static VectorIcon,
        color: Color,
        clip_rect: Rect,
    ) {
        self.ensure_draw_list()
            .push_vector_icon(rect, icon, color, clip_rect);
    }

    pub fn drag_float_in_rect(
        &mut self,
        id_source: &str,
        rect: Rect,
        label: &str,
        value: &mut f32,
        min: f32,
        max: f32,
        clip_rect: Rect,
    ) -> Response {
        let id = self.widget_id(id_source, "drag_float");
        let label_height = text_size(label, TEXT_SCALE).y;
        let value_text = format!("{:.3}", *value);
        let value_width = text_size(&value_text, TEXT_SCALE).x.max(48.0);
        self.draw_text_clipped(
            label,
            rect.min,
            Color::rgba(232, 236, 243, 255),
            TEXT_SCALE,
            clip_rect,
            (rect.width() - value_width - 12.0).max(0.0),
            true,
        );
        self.draw_text_clipped(
            &value_text,
            Vec2::new(rect.max.x - value_width, rect.min.y),
            Color::rgba(186, 194, 210, 255),
            TEXT_SCALE,
            clip_rect,
            value_width,
            false,
        );

        let track_y = rect.min.y + label_height + 12.0;
        let track_rect = Rect::from_min_size(
            Vec2::new(rect.min.x, track_y),
            Vec2::new(rect.width(), SLIDER_TRACK_HEIGHT),
        );
        let hovered = rect.intersects(clip_rect)
            && clip_rect.contains(self.input.mouse_position)
            && track_rect.contains(self.input.mouse_position);
        if hovered && self.input.mouse_pressed(MouseButton::Left) {
            self.memory.active = Some(ActiveInteraction::DragFloat { id });
        }

        let active = matches!(
            self.memory.active,
            Some(ActiveInteraction::DragFloat { id: active_id }) if active_id == id
        );
        if !self.input.mouse_down(MouseButton::Left) && active {
            self.memory.active = None;
        }

        let mut changed = false;
        if active {
            let range = (max - min).max(f32::EPSILON);
            let ratio = ((self.input.mouse_position.x - track_rect.min.x) / track_rect.width())
                .clamp(0.0, 1.0);
            let next = min + ratio * range;
            if (*value - next).abs() > f32::EPSILON {
                *value = next;
                changed = true;
            }
        }

        self.progress_bar_in_rect(
            Rect::from_min_size(
                Vec2::new(
                    rect.min.x,
                    track_rect.min.y - ((rect.height() - SLIDER_TRACK_HEIGHT) * 0.0),
                ),
                Vec2::new(rect.width(), SLIDER_TRACK_HEIGHT),
            ),
            *value,
            min,
            max,
            clip_rect,
        );

        let ratio = if max > min {
            ((*value - min) / (max - min)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let knob_center_x = track_rect.min.x + track_rect.width() * ratio;
        let knob_rect = Rect::from_min_size(
            Vec2::new(knob_center_x - 7.0, track_rect.min.y - 5.0),
            Vec2::new(14.0, 18.0),
        );
        self.ensure_draw_list().push_rounded_rect_clipped(
            knob_rect,
            if active {
                Color::rgba(255, 255, 255, 255)
            } else if hovered {
                Color::rgba(241, 245, 255, 255)
            } else {
                Color::rgba(218, 225, 237, 255)
            },
            7.0,
            clip_rect,
        );

        Response {
            hovered,
            active,
            changed,
            ..Response::default()
        }
    }

    fn ensure_draw_list(&mut self) -> &mut DrawList {
        if self.draw_data.lists.is_empty() {
            self.draw_data.lists.push(DrawList::default());
        }
        &mut self.draw_data.lists[0]
    }

    fn layout_rect(&mut self, width: f32, height: f32) -> (Rect, Rect) {
        let window = self
            .current_window
            .as_mut()
            .expect("widget used outside of window");
        let rect = Rect::from_min_size(
            window.cursor,
            Vec2::new(
                if width <= 0.0 {
                    window.content_rect.width()
                } else {
                    width
                },
                height,
            ),
        );
        window.cursor.y += height + ITEM_SPACING;
        (rect, window.body_rect)
    }

    fn draw_text(&mut self, text: &str, position: Vec2, color: Color, scale: f32, clip_rect: Rect) {
        self.draw_text_clipped(text, position, color, scale, clip_rect, f32::MAX, false);
    }

    fn draw_text_clipped(
        &mut self,
        text: &str,
        position: Vec2,
        color: Color,
        scale: f32,
        clip_rect: Rect,
        max_width: f32,
        ellipsis: bool,
    ) {
        let content = if ellipsis && max_width.is_finite() {
            elide_text(text, scale, max_width)
        } else {
            text.to_string()
        };
        let size = text_size(&content, scale);
        let width = if max_width.is_finite() {
            max_width.max(0.0)
        } else {
            size.x.max(0.0)
        };
        let rect = Rect::from_min_size(position, Vec2::new(width, size.y.max(0.0)));
        self.ensure_draw_list()
            .push_text(rect, content, color, clip_rect);
    }

    fn window_clip_rect(&self) -> Rect {
        self.current_window
            .as_ref()
            .map(|window| window.body_rect)
            .unwrap_or(Rect::from_min_size(
                Vec2::new(0.0, 0.0),
                self.display_size(),
            ))
    }

    fn widget_id(&self, label: &str, kind: &str) -> u64 {
        let parent = self
            .current_window
            .as_ref()
            .map(|window| window.id)
            .unwrap_or(0);
        hash_id((kind, parent, label))
    }

    fn button_in_rect_with_colors(
        &mut self,
        id_source: &str,
        rect: Rect,
        label: &str,
        clip_rect: Rect,
        colors: ButtonColors,
    ) -> Response {
        let id = self.widget_id(id_source, "button_rect");
        let hovered = rect.contains(self.input.mouse_position);

        if hovered && self.input.mouse_pressed(MouseButton::Left) {
            self.memory.active = Some(ActiveInteraction::Button { id });
        }

        let active = matches!(
            self.memory.active,
            Some(ActiveInteraction::Button { id: active_id }) if active_id == id
        );
        let clicked = active && hovered && self.input.mouse_released(MouseButton::Left);

        let fill = if active {
            colors.active
        } else if hovered {
            colors.hovered
        } else {
            colors.normal
        };

        self.ensure_draw_list()
            .push_rounded_rect_clipped(rect, fill, BUTTON_RADIUS, clip_rect);
        let text_metrics = text_size(label, TEXT_SCALE);
        let text_height = text_metrics.y.max(text_size(" ", TEXT_SCALE).y);
        let text_x = rect.min.x + ((rect.width() - text_metrics.x) * 0.5).floor();
        let text_y = rect.min.y + ((rect.height() - text_height) * 0.5).floor();
        self.draw_text_clipped(
            label,
            Vec2::new(text_x.max(rect.min.x + 8.0), text_y),
            colors.text,
            TEXT_SCALE,
            clip_rect,
            (rect.width() - 16.0).max(0.0),
            true,
        );

        Response {
            hovered,
            active,
            clicked,
            ..Response::default()
        }
    }
}

#[derive(Debug)]
pub struct Frame<'a> {
    ui: Ui<'a>,
}

impl<'a> Frame<'a> {
    pub fn begin(input: InputSnapshot, memory: &'a mut Memory) -> Self {
        Self {
            ui: Ui::new(input, memory),
        }
    }

    pub fn ui(&mut self) -> &mut Ui<'a> {
        &mut self.ui
    }

    pub fn finish(self) -> DrawData {
        self.ui.draw_data
    }
}

pub fn text_size(text: &str, scale: f32) -> Vec2 {
    let mut line_width = 0.0;
    let mut max_width: f32 = 0.0;
    let mut line_count = 1usize;
    let font_size = text_font_size(scale);
    let line_height = text_line_height(scale);

    for ch in text.chars() {
        if ch == '\n' {
            max_width = max_width.max(line_width);
            line_width = 0.0;
            line_count += 1;
            continue;
        }
        line_width += char_advance(ch, font_size);
    }
    max_width = max_width.max(line_width);

    Vec2::new(
        max_width.max(0.0),
        (line_count as f32 * line_height).max(font_size + BASE_LINE_GAP),
    )
}

fn hash_id<T: Hash>(value: T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn text_font_size(scale: f32) -> f32 {
    (BASE_FONT_SIZE * scale).max(10.0)
}

fn text_line_height(scale: f32) -> f32 {
    text_font_size(scale) + BASE_LINE_GAP
}

fn char_advance(ch: char, font_size: f32) -> f32 {
    match ch {
        '\t' => font_size * 1.4,
        ' ' => font_size * 0.35,
        c if c.is_ascii_lowercase() => font_size * 0.52,
        c if c.is_ascii_uppercase() => font_size * 0.62,
        c if c.is_ascii_digit() => font_size * 0.56,
        c if c.is_ascii_punctuation() => font_size * 0.42,
        _ => font_size * 0.95,
    }
}

fn elide_text(text: &str, scale: f32, max_width: f32) -> String {
    if max_width <= 0.0 || text_size(text, scale).x <= max_width {
        return text.to_string();
    }

    let ellipsis = "...";
    let font_size = text_font_size(scale);
    let ellipsis_width = ellipsis
        .chars()
        .map(|ch| char_advance(ch, font_size))
        .sum::<f32>();
    if ellipsis_width >= max_width {
        return ellipsis.to_string();
    }

    let mut width = 0.0;
    let mut result = String::new();
    for ch in text.chars() {
        let char_width = char_advance(ch, font_size);
        if width + char_width + ellipsis_width > max_width {
            break;
        }
        result.push(ch);
        width += char_width;
    }
    result.push_str(ellipsis);
    result
}

use crate::geometry::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Extra1,
    Extra2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Insert,
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Space,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Character(char),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InputSnapshot {
    pub display_size: Vec2,
    pub mouse_position: Vec2,
    pub mouse_wheel_delta: f32,
    pub mouse_buttons: Vec<MouseButton>,
    pub mouse_buttons_pressed: Vec<MouseButton>,
    pub mouse_buttons_released: Vec<MouseButton>,
    pub pressed_keys: Vec<Key>,
    pub alt_down: bool,
    pub ctrl_down: bool,
    pub shift_down: bool,
    pub typed_text: String,
    pub delta_seconds: f32,
}

impl InputSnapshot {
    pub fn mouse_down(&self, button: MouseButton) -> bool {
        self.mouse_buttons.contains(&button)
    }

    pub fn mouse_pressed(&self, button: MouseButton) -> bool {
        self.mouse_buttons_pressed.contains(&button)
    }

    pub fn mouse_released(&self, button: MouseButton) -> bool {
        self.mouse_buttons_released.contains(&button)
    }

    pub fn key_pressed(&self, key: Key) -> bool {
        self.pressed_keys.contains(&key)
    }
}

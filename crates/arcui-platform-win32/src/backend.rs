use std::collections::HashSet;
use std::time::Instant;

use arcui_core::{InputSnapshot, Key, MouseButton, Vec2};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};

#[derive(Clone, Copy, Debug)]
pub struct PlatformEvent {
    pub hwnd: HWND,
    pub message: u32,
    pub wparam: WPARAM,
    pub lparam: LPARAM,
}

#[derive(Debug)]
pub struct Win32Platform {
    snapshot: InputSnapshot,
    mouse_down: HashSet<MouseButton>,
    pressed_buttons: Vec<MouseButton>,
    released_buttons: Vec<MouseButton>,
    pressed_keys: Vec<Key>,
    typed_text: String,
    last_frame: Instant,
}

impl Default for Win32Platform {
    fn default() -> Self {
        Self {
            snapshot: InputSnapshot::default(),
            mouse_down: HashSet::new(),
            pressed_buttons: Vec::new(),
            released_buttons: Vec::new(),
            pressed_keys: Vec::new(),
            typed_text: String::new(),
            last_frame: Instant::now(),
        }
    }
}

impl Win32Platform {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle_event(&mut self, event: PlatformEvent) {
        match event.message {
            WM_MOUSEMOVE => {
                self.update_mouse_from_lparam(event.lparam);
            }
            WM_MOUSEWHEEL => {
                self.on_mouse_wheel(event.wparam);
            }
            WM_LBUTTONDOWN => {
                self.update_mouse_from_lparam(event.lparam);
                self.on_mouse_down(MouseButton::Left);
            }
            WM_LBUTTONUP => {
                self.update_mouse_from_lparam(event.lparam);
                self.on_mouse_up(MouseButton::Left);
            }
            WM_RBUTTONDOWN => {
                self.update_mouse_from_lparam(event.lparam);
                self.on_mouse_down(MouseButton::Right);
            }
            WM_RBUTTONUP => {
                self.update_mouse_from_lparam(event.lparam);
                self.on_mouse_up(MouseButton::Right);
            }
            WM_MBUTTONDOWN => {
                self.update_mouse_from_lparam(event.lparam);
                self.on_mouse_down(MouseButton::Middle);
            }
            WM_MBUTTONUP => {
                self.update_mouse_from_lparam(event.lparam);
                self.on_mouse_up(MouseButton::Middle);
            }
            WM_XBUTTONDOWN => {
                self.update_mouse_from_lparam(event.lparam);
                if xbutton(event.wparam) == XBUTTON1 {
                    self.on_mouse_down(MouseButton::Extra1);
                } else if xbutton(event.wparam) == XBUTTON2 {
                    self.on_mouse_down(MouseButton::Extra2);
                }
            }
            WM_XBUTTONUP => {
                self.update_mouse_from_lparam(event.lparam);
                if xbutton(event.wparam) == XBUTTON1 {
                    self.on_mouse_up(MouseButton::Extra1);
                } else if xbutton(event.wparam) == XBUTTON2 {
                    self.on_mouse_up(MouseButton::Extra2);
                }
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                self.update_modifiers(event.wparam.0 as u32, true);
                let is_repeat = ((event.lparam.0 >> 30) & 1) != 0;
                if !is_repeat {
                    if let Some(key) = translate_key(event.wparam.0 as u32) {
                        self.pressed_keys.push(key);
                    }
                }
            }
            WM_KEYUP | WM_SYSKEYUP => self.update_modifiers(event.wparam.0 as u32, false),
            WM_CHAR => {
                if let Some(ch) = char::from_u32(event.wparam.0 as u32) {
                    self.typed_text.push(ch);
                }
            }
            _ => {}
        }
    }

    pub fn set_display_size(&mut self, width: u32, height: u32) {
        self.snapshot.display_size = Vec2::new(width as f32, height as f32);
    }

    pub fn take_snapshot(&mut self, _hwnd: HWND) -> InputSnapshot {
        let now = Instant::now();
        self.snapshot.delta_seconds = (now - self.last_frame).as_secs_f32().max(0.0);
        self.last_frame = now;
        self.snapshot.mouse_buttons = self.mouse_down.iter().copied().collect();
        self.snapshot.mouse_buttons_pressed = std::mem::take(&mut self.pressed_buttons);
        self.snapshot.mouse_buttons_released = std::mem::take(&mut self.released_buttons);
        self.snapshot.pressed_keys = std::mem::take(&mut self.pressed_keys);
        self.snapshot.typed_text = std::mem::take(&mut self.typed_text);
        let snapshot = self.snapshot.clone();
        self.snapshot.mouse_wheel_delta = 0.0;
        snapshot
    }

    fn on_mouse_down(&mut self, button: MouseButton) {
        if self.mouse_down.insert(button) {
            self.pressed_buttons.push(button);
        }
    }

    fn on_mouse_up(&mut self, button: MouseButton) {
        if self.mouse_down.remove(&button) {
            self.released_buttons.push(button);
        }
    }

    fn update_modifiers(&mut self, vk: u32, down: bool) {
        match vk {
            0x12 => self.snapshot.alt_down = down,
            0x11 => self.snapshot.ctrl_down = down,
            0x10 => self.snapshot.shift_down = down,
            _ => {}
        }
    }

    fn update_mouse_from_lparam(&mut self, lparam: LPARAM) {
        let x = loword_signed(lparam.0 as u32) as f32;
        let y = hiword_signed(lparam.0 as u32) as f32;
        self.snapshot.mouse_position = Vec2::new(x, y);
    }

    fn on_mouse_wheel(&mut self, wparam: WPARAM) {
        self.snapshot.mouse_wheel_delta += wheel_delta(wparam) as f32 / 120.0;
    }
}

fn translate_key(vk: u32) -> Option<Key> {
    Some(match vk {
        0x2D => Key::Insert,
        0x1B => Key::Escape,
        0x0D => Key::Enter,
        0x09 => Key::Tab,
        0x08 => Key::Backspace,
        0x2E => Key::Delete,
        0x20 => Key::Space,
        0x70 => Key::F1,
        0x71 => Key::F2,
        0x72 => Key::F3,
        0x73 => Key::F4,
        0x74 => Key::F5,
        0x75 => Key::F6,
        0x76 => Key::F7,
        0x77 => Key::F8,
        0x78 => Key::F9,
        0x79 => Key::F10,
        0x7A => Key::F11,
        0x7B => Key::F12,
        0x25 => Key::Left,
        0x27 => Key::Right,
        0x26 => Key::Up,
        0x28 => Key::Down,
        0x24 => Key::Home,
        0x23 => Key::End,
        value @ 0x30..=0x5A => Key::Character(char::from_u32(value).unwrap_or('?')),
        _ => return None,
    })
}

fn loword_signed(value: u32) -> i16 {
    (value & 0xFFFF) as i16
}

fn hiword_signed(value: u32) -> i16 {
    ((value >> 16) & 0xFFFF) as i16
}

fn xbutton(wparam: WPARAM) -> u16 {
    ((wparam.0 >> 16) & 0xFFFF) as u16
}

fn wheel_delta(wparam: WPARAM) -> i16 {
    ((wparam.0 >> 16) & 0xFFFF) as i16
}

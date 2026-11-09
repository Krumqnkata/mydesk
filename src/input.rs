//! Replays the viewer's mouse and keyboard on the host.
//!
//! Keys travel as *physical* key names (where the key sits on the keyboard), so
//! the host's own keyboard layout decides which letter appears — typing in
//! Bulgarian, English or any other layout works the same as sitting there.

use std::collections::HashSet;

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};

use crate::proto::DisplayInfo;

/// (egui key name, PC set-1 scan code, Linux evdev code)
const KEYS: &[(&str, u16, u16)] = &[
    ("Escape", 0x01, 1), ("1", 0x02, 2), ("2", 0x03, 3), ("3", 0x04, 4), ("4", 0x05, 5),
    ("5", 0x06, 6), ("6", 0x07, 7), ("7", 0x08, 8), ("8", 0x09, 9), ("9", 0x0A, 10),
    ("0", 0x0B, 11), ("Minus", 0x0C, 12), ("Equals", 0x0D, 13), ("Backspace", 0x0E, 14),
    ("Tab", 0x0F, 15), ("Q", 0x10, 16), ("W", 0x11, 17), ("E", 0x12, 18), ("R", 0x13, 19),
    ("T", 0x14, 20), ("Y", 0x15, 21), ("U", 0x16, 22), ("I", 0x17, 23), ("O", 0x18, 24),
    ("P", 0x19, 25), ("OpenBracket", 0x1A, 26), ("CloseBracket", 0x1B, 27), ("Enter", 0x1C, 28),
    ("ControlLeft", 0x1D, 29), ("A", 0x1E, 30), ("S", 0x1F, 31), ("D", 0x20, 32), ("F", 0x21, 33),
    ("G", 0x22, 34), ("H", 0x23, 35), ("J", 0x24, 36), ("K", 0x25, 37), ("L", 0x26, 38),
    ("Semicolon", 0x27, 39), ("Quote", 0x28, 40), ("Backtick", 0x29, 41), ("ShiftLeft", 0x2A, 42),
    ("Backslash", 0x2B, 43), ("Z", 0x2C, 44), ("X", 0x2D, 45), ("C", 0x2E, 46), ("V", 0x2F, 47),
    ("B", 0x30, 48), ("N", 0x31, 49), ("M", 0x32, 50), ("Comma", 0x33, 51), ("Period", 0x34, 52),
    ("Slash", 0x35, 53), ("ShiftRight", 0x36, 54), ("AltLeft", 0x38, 56), ("Space", 0x39, 57),
    ("CapsLock", 0x3A, 58), ("F1", 0x3B, 59), ("F2", 0x3C, 60), ("F3", 0x3D, 61), ("F4", 0x3E, 62),
    ("F5", 0x3F, 63), ("F6", 0x40, 64), ("F7", 0x41, 65), ("F8", 0x42, 66), ("F9", 0x43, 67),
    ("F10", 0x44, 68), ("NumLock", 0x45, 69), ("ScrollLock", 0x46, 70), ("IntlBackslash", 0x56, 86),
    ("F11", 0x57, 87), ("F12", 0x58, 88),
    // extended keys: scan code 0 = use a named key on Windows
    ("ArrowUp", 0, 103), ("ArrowDown", 0, 108), ("ArrowLeft", 0, 105), ("ArrowRight", 0, 106),
    ("Home", 0, 102), ("End", 0, 107), ("PageUp", 0, 104), ("PageDown", 0, 109),
    ("Insert", 0, 110), ("Delete", 0, 111), ("ControlRight", 0, 97), ("AltRight", 0, 100),
    ("SuperLeft", 0, 125), ("SuperRight", 0, 126), ("Menu", 0, 127), ("PrintScreen", 0, 99),
    ("Pause", 0, 119),
];

/// egui's key names for digits are "0".."9"; normalise the few that differ.
fn normalise(name: &str) -> &str {
    match name {
        "Num0" => "0",
        "Num1" => "1",
        "Num2" => "2",
        "Num3" => "3",
        "Num4" => "4",
        "Num5" => "5",
        "Num6" => "6",
        "Num7" => "7",
        "Num8" => "8",
        "Num9" => "9",
        "Esc" => "Escape",
        "Return" => "Enter",
        "Up" => "ArrowUp",
        "Down" => "ArrowDown",
        "Left" => "ArrowLeft",
        "Right" => "ArrowRight",
        "Del" => "Delete",
        "Ins" => "Insert",
        "PgUp" => "PageUp",
        "PgDn" => "PageDown",
        other => other,
    }
}

pub fn is_known_key(name: &str) -> bool {
    let n = normalise(name);
    KEYS.iter().any(|(k, _, _)| *k == n)
}

pub struct Injector {
    enigo: Option<Enigo>,
    pressed: HashSet<String>,
    buttons: HashSet<u8>,
}

impl Injector {
    pub fn new() -> Self {
        Self { enigo: Enigo::new(&Settings::default()).ok(), pressed: HashSet::new(), buttons: HashSet::new() }
    }

    pub fn cursor(&self) -> Option<(i32, i32)> {
        self.enigo.as_ref()?.location().ok()
    }

    pub fn move_to(&mut self, d: &DisplayInfo, nx: f32, ny: f32) {
        let Some(e) = self.enigo.as_mut() else { return };
        let x = d.x + (nx.clamp(0.0, 1.0) * (d.width.saturating_sub(1)) as f32).round() as i32;
        let y = d.y + (ny.clamp(0.0, 1.0) * (d.height.saturating_sub(1)) as f32).round() as i32;
        let _ = e.move_mouse(x, y, Coordinate::Abs);
    }

    pub fn button(&mut self, b: u8, down: bool) {
        let Some(e) = self.enigo.as_mut() else { return };
        let button = match b {
            0 => Button::Left,
            1 => Button::Right,
            2 => Button::Middle,
            3 => Button::Back,
            4 => Button::Forward,
            _ => return,
        };
        if down {
            if self.buttons.insert(b) {
                let _ = e.button(button, Direction::Press);
            }
        } else if self.buttons.remove(&b) {
            let _ = e.button(button, Direction::Release);
        }
    }

    pub fn scroll(&mut self, dx: i32, dy: i32) {
        let Some(e) = self.enigo.as_mut() else { return };
        if dy != 0 {
            let _ = e.scroll(dy, Axis::Vertical);
        }
        if dx != 0 {
            let _ = e.scroll(dx, Axis::Horizontal);
        }
    }

    pub fn key(&mut self, name: &str, down: bool) {
        let name = normalise(name).to_string();
        if down {
            // Auto-repeat is fine, but a key we never saw go down must not be released.
            self.pressed.insert(name.clone());
        } else if !self.pressed.remove(&name) {
            return;
        }
        let Some(&(_, scan, evdev)) = KEYS.iter().find(|(k, _, _)| *k == name) else { return };
        let Some(e) = self.enigo.as_mut() else { return };
        let dir = if down { Direction::Press } else { Direction::Release };
        let _ = (scan, evdev);
        #[cfg(windows)]
        {
            use enigo::Key;
            if scan != 0 {
                let _ = e.raw(scan, dir);
            } else {
                let key = match name.as_str() {
                    "ArrowUp" => Key::UpArrow,
                    "ArrowDown" => Key::DownArrow,
                    "ArrowLeft" => Key::LeftArrow,
                    "ArrowRight" => Key::RightArrow,
                    "Home" => Key::Home,
                    "End" => Key::End,
                    "PageUp" => Key::PageUp,
                    "PageDown" => Key::PageDown,
                    "Insert" => Key::Insert,
                    "Delete" => Key::Delete,
                    "ControlRight" => Key::RControl,
                    "AltRight" => Key::RMenu,
                    "SuperLeft" => Key::LWin,
                    "SuperRight" => Key::RWin,
                    "Menu" => Key::Apps,
                    "PrintScreen" => Key::PrintScr,
                    "Pause" => Key::Pause,
                    _ => return,
                };
                let _ = e.key(key, dir);
            }
        }
        #[cfg(not(windows))]
        {
            // X11 key codes are evdev codes + 8.
            let _ = e.raw(evdev + 8, dir);
        }
    }

    pub fn release_all(&mut self) {
        let keys: Vec<String> = self.pressed.iter().cloned().collect();
        for k in keys {
            self.key(&k, false);
        }
        let buttons: Vec<u8> = self.buttons.iter().copied().collect();
        for b in buttons {
            self.button(b, false);
        }
    }

    /// Ctrl+Alt+Del cannot be faked on Windows (it is a secure key sequence);
    /// we send Ctrl+Shift+Esc (Task Manager) there, and the real combo elsewhere.
    pub fn ctrl_alt_del(&mut self) {
        #[cfg(windows)]
        let combo = ["ControlLeft", "ShiftLeft", "Escape"];
        #[cfg(not(windows))]
        let combo = ["ControlLeft", "AltLeft", "Delete"];
        for k in combo {
            self.key(k, true);
        }
        for k in combo.iter().rev() {
            self.key(k, false);
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
    }
}

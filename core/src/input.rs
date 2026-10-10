//! Input state machine: turns platform-neutral key/mouse events into mrext
//! WebSocket messages. Pure logic (no I/O) so it can be unit-tested.

use std::collections::BTreeSet;

use crate::keymap::{hid, hid_to_linux};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, uniffi::Enum)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    fn name(self) -> &'static str {
        match self {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum KeyResult {
    /// Sent (or a state change was applied) on MiSTer.
    Forwarded,
    /// Unknown key, auto-repeat, or a release for a key MiSTer never saw.
    Ignored,
    /// The release chord (Right GUI + Right Alt + Esc) was pressed. Everything
    /// held has been released; the shell should leave capture mode.
    ExitCapture,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct InputSettings {
    /// Multiplier applied to relative mouse deltas.
    pub mouse_sensitivity: f64,
    /// Alt/Option + left click is sent as a middle click (for 3-button Unix
    /// desktops on single-button mice/trackpads).
    pub alt_click_is_middle: bool,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            mouse_sensitivity: 1.0,
            alt_click_is_middle: true,
        }
    }
}

#[derive(Default)]
pub struct InputState {
    pub settings: InputSettings,
    held_keys: BTreeSet<u16>,
    held_buttons: BTreeSet<MouseButton>,
    left_is_middle: bool,
}

impl InputState {
    pub fn key_event(&mut self, usage: u16, down: bool, out: &mut Vec<String>) -> KeyResult {
        let Some(code) = hid_to_linux(usage) else {
            return KeyResult::Ignored;
        };
        if down
            && usage == hid::ESCAPE
            && self.held_keys.contains(&hid::RIGHT_GUI)
            && self.held_keys.contains(&hid::RIGHT_ALT)
        {
            self.release_all(out);
            return KeyResult::ExitCapture;
        }
        if down {
            if !self.held_keys.insert(usage) {
                return KeyResult::Ignored;
            }
            out.push(format!("kbdRawDown:{code}"));
        } else {
            if !self.held_keys.remove(&usage) {
                return KeyResult::Ignored;
            }
            out.push(format!("kbdRawUp:{code}"));
        }
        KeyResult::Forwarded
    }

    pub fn mouse_button(&mut self, button: MouseButton, down: bool, out: &mut Vec<String>) {
        let mut button = button;
        if button == MouseButton::Left {
            if down && self.settings.alt_click_is_middle && self.alt_held() {
                // Let go of Alt on MiSTer so the core sees a plain middle click.
                for usage in [hid::LEFT_ALT, hid::RIGHT_ALT] {
                    if self.held_keys.remove(&usage) {
                        out.push(format!("kbdRawUp:{}", hid_to_linux(usage).unwrap()));
                    }
                }
                self.left_is_middle = true;
                button = MouseButton::Middle;
            } else if !down && self.left_is_middle {
                self.left_is_middle = false;
                button = MouseButton::Middle;
            }
        }
        let changed = if down {
            self.held_buttons.insert(button)
        } else {
            self.held_buttons.remove(&button)
        };
        if changed {
            let dir = if down { "down" } else { "up" };
            out.push(format!("mouseBtn:{}_{dir}", button.name()));
        }
    }

    /// Release everything MiSTer thinks is held.
    pub fn release_all(&mut self, out: &mut Vec<String>) {
        for usage in std::mem::take(&mut self.held_keys) {
            if let Some(code) = hid_to_linux(usage) {
                out.push(format!("kbdRawUp:{code}"));
            }
        }
        for button in std::mem::take(&mut self.held_buttons) {
            out.push(format!("mouseBtn:{}_up", button.name()));
        }
        self.left_is_middle = false;
    }

    fn alt_held(&self) -> bool {
        self.held_keys.contains(&hid::LEFT_ALT) || self.held_keys.contains(&hid::RIGHT_ALT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(f: impl FnOnce(&mut InputState, &mut Vec<String>)) -> Vec<String> {
        let mut state = InputState::default();
        let mut out = Vec::new();
        f(&mut state, &mut out);
        out
    }

    #[test]
    fn key_down_up_and_repeat() {
        let out = run(|s, o| {
            assert_eq!(s.key_event(0x04, true, o), KeyResult::Forwarded);
            assert_eq!(s.key_event(0x04, true, o), KeyResult::Ignored);
            assert_eq!(s.key_event(0x04, false, o), KeyResult::Forwarded);
            assert_eq!(s.key_event(0x04, false, o), KeyResult::Ignored);
        });
        assert_eq!(out, ["kbdRawDown:30", "kbdRawUp:30"]);
    }

    #[test]
    fn exit_chord_releases_and_swallows_esc() {
        let out = run(|s, o| {
            s.key_event(hid::RIGHT_GUI, true, o);
            s.key_event(hid::RIGHT_ALT, true, o);
            assert_eq!(s.key_event(hid::ESCAPE, true, o), KeyResult::ExitCapture);
            // Physical releases afterwards are ignored.
            assert_eq!(s.key_event(hid::RIGHT_GUI, false, o), KeyResult::Ignored);
        });
        assert_eq!(
            out,
            ["kbdRawDown:126", "kbdRawDown:100", "kbdRawUp:100", "kbdRawUp:126"]
        );
    }

    #[test]
    fn plain_esc_is_forwarded() {
        let out = run(|s, o| {
            s.key_event(hid::RIGHT_GUI, true, o);
            assert_eq!(s.key_event(hid::ESCAPE, true, o), KeyResult::Forwarded);
        });
        assert_eq!(out, ["kbdRawDown:126", "kbdRawDown:1"]);
    }

    #[test]
    fn alt_click_becomes_middle() {
        let out = run(|s, o| {
            s.key_event(hid::LEFT_ALT, true, o);
            s.mouse_button(MouseButton::Left, true, o);
            s.mouse_button(MouseButton::Left, false, o);
            s.key_event(hid::LEFT_ALT, false, o);
            s.mouse_button(MouseButton::Left, true, o);
        });
        assert_eq!(
            out,
            [
                "kbdRawDown:56",
                "kbdRawUp:56",
                "mouseBtn:middle_down",
                "mouseBtn:middle_up",
                "mouseBtn:left_down",
            ]
        );
    }

    #[test]
    fn release_all_covers_keys_and_buttons() {
        let out = run(|s, o| {
            s.key_event(0x04, true, o);
            s.mouse_button(MouseButton::Right, true, o);
            o.clear();
            s.release_all(o);
        });
        assert_eq!(out, ["kbdRawUp:30", "mouseBtn:right_up"]);
    }

    #[test]
    fn unpaired_button_up_is_dropped() {
        let out = run(|s, o| s.mouse_button(MouseButton::Left, false, o));
        assert!(out.is_empty());
    }
}

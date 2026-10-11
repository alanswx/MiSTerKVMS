//! USB HID keyboard usages (page 0x07) → Linux input keycodes (what mrext's
//! `kbdRawDown`/`kbdRawUp` expect, see linux/input-event-codes.h).
//!
//! Shells translate their OS keycodes to HID usages; HID is the platform-neutral
//! layer because it distinguishes left/right modifiers on every OS.

pub mod hid {
    pub const ESCAPE: u16 = 0x29;
    pub const F12: u16 = 0x45;
    pub const DELETE: u16 = 0x4C;
    pub const LEFT_CTRL: u16 = 0xE0;
    pub const LEFT_ALT: u16 = 0xE2;
    pub const RIGHT_ALT: u16 = 0xE6;
    pub const RIGHT_GUI: u16 = 0xE7;
}

pub fn hid_to_linux(usage: u16) -> Option<u16> {
    let code = match usage {
        // Letters A..Z
        0x04 => 30, // A
        0x05 => 48, // B
        0x06 => 46, // C
        0x07 => 32, // D
        0x08 => 18, // E
        0x09 => 33, // F
        0x0A => 34, // G
        0x0B => 35, // H
        0x0C => 23, // I
        0x0D => 36, // J
        0x0E => 37, // K
        0x0F => 38, // L
        0x10 => 50, // M
        0x11 => 49, // N
        0x12 => 24, // O
        0x13 => 25, // P
        0x14 => 16, // Q
        0x15 => 19, // R
        0x16 => 31, // S
        0x17 => 20, // T
        0x18 => 22, // U
        0x19 => 47, // V
        0x1A => 17, // W
        0x1B => 45, // X
        0x1C => 21, // Y
        0x1D => 44, // Z
        // Digits 1..9, 0
        0x1E..=0x26 => usage - 0x1E + 2,
        0x27 => 11,
        0x28 => 28, // Enter
        0x29 => 1,  // Esc
        0x2A => 14, // Backspace
        0x2B => 15, // Tab
        0x2C => 57, // Space
        0x2D => 12, // -
        0x2E => 13, // =
        0x2F => 26, // [
        0x30 => 27, // ]
        0x31 => 43, // backslash
        0x32 => 43, // Non-US # (same position as backslash on ISO)
        0x33 => 39, // ;
        0x34 => 40, // '
        0x35 => 41, // `
        0x36 => 51, // ,
        0x37 => 52, // .
        0x38 => 53, // /
        0x39 => 58, // Caps Lock
        // F1..F10, F11, F12
        0x3A..=0x43 => usage - 0x3A + 59,
        0x44 => 87,
        0x45 => 88,
        0x46 => 99,  // Print Screen (SysRq)
        0x47 => 70,  // Scroll Lock
        0x48 => 119, // Pause
        0x49 => 110, // Insert
        0x4A => 102, // Home
        0x4B => 104, // Page Up
        0x4C => 111, // Delete
        0x4D => 107, // End
        0x4E => 109, // Page Down
        0x4F => 106, // Right
        0x50 => 105, // Left
        0x51 => 108, // Down
        0x52 => 103, // Up
        0x53 => 69,  // Num Lock
        0x54 => 98,  // KP /
        0x55 => 55,  // KP *
        0x56 => 74,  // KP -
        0x57 => 78,  // KP +
        0x58 => 96,  // KP Enter
        0x59 => 79,  // KP 1
        0x5A => 80,  // KP 2
        0x5B => 81,  // KP 3
        0x5C => 75,  // KP 4
        0x5D => 76,  // KP 5
        0x5E => 77,  // KP 6
        0x5F => 71,  // KP 7
        0x60 => 72,  // KP 8
        0x61 => 73,  // KP 9
        0x62 => 82,  // KP 0
        0x63 => 83,  // KP .
        0x64 => 86,  // Non-US backslash (102nd key)
        0x65 => 127, // Application / Compose
        0x66 => 116, // Power
        0x67 => 117, // KP =
        // F13..F24
        0x68..=0x73 => usage - 0x68 + 183,
        0x75 => 138, // Help
        0x7F => 113, // Mute
        0x80 => 115, // Volume Up
        0x81 => 114, // Volume Down
        0x85 => 121, // KP ,
        0x87 => 89,  // International1 (Ro)
        0x89 => 124, // International3 (Yen)
        0x90 => 122, // LANG1 (Hangeul)
        0x91 => 123, // LANG2 (Hanja)
        // Modifiers
        0xE0 => 29,  // Left Ctrl
        0xE1 => 42,  // Left Shift
        0xE2 => 56,  // Left Alt
        0xE3 => 125, // Left Meta
        0xE4 => 97,  // Right Ctrl
        0xE5 => 54,  // Right Shift
        0xE6 => 100, // Right Alt
        0xE7 => 126, // Right Meta
        _ => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(hid_to_linux(0x1E), Some(2)); // 1
        assert_eq!(hid_to_linux(0x26), Some(10)); // 9
        assert_eq!(hid_to_linux(0x27), Some(11)); // 0
        assert_eq!(hid_to_linux(0x3A), Some(59)); // F1
        assert_eq!(hid_to_linux(0x43), Some(68)); // F10
        assert_eq!(hid_to_linux(hid::F12), Some(88));
        assert_eq!(hid_to_linux(0x68), Some(183)); // F13
        assert_eq!(hid_to_linux(0x73), Some(194)); // F24
        assert_eq!(hid_to_linux(0x00), None);
    }
}

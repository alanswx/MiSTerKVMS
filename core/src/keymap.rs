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

/// Linux evdev key code (what X11/Wayland report, minus 8) → USB HID usage:
/// the inverse of `hid_to_linux`. Linux codes are what mrext sends on, so
/// every key MiSTer can receive has an answer.
pub fn linux_to_hid(code: u16) -> Option<u16> {
    // 0x31 (backslash) comes before 0x32 (ISO #), which shares its code.
    (0x04..=0xE7u16).find(|&usage| hid_to_linux(usage) == Some(code))
}

/// PC keyboard scan code (set 1, as Windows reports it) → USB HID usage.
/// `extended` is the E0 prefix (LLKHF_EXTENDED). Layout-independent: the
/// key's position, not its label. Pause and Num Lock share scan code 0x45
/// on Windows, so the shell tells them apart by virtual key first.
pub fn scancode_to_hid(scan: u16, extended: bool) -> Option<u16> {
    let usage = if extended {
        match scan {
            0x1C => 0x58, // KP Enter
            0x1D => 0xE4, // Right Ctrl
            0x20 => 0x7F, // Mute
            0x2E => 0x81, // Volume Down
            0x30 => 0x80, // Volume Up
            0x35 => 0x54, // KP /
            0x37 => 0x46, // Print Screen
            0x38 => 0xE6, // Right Alt (AltGr)
            0x45 => 0x53, // Num Lock
            0x46 => 0x48, // Break (Ctrl+Pause)
            0x47 => 0x4A, // Home
            0x48 => 0x52, // Up
            0x49 => 0x4B, // Page Up
            0x4B => 0x50, // Left
            0x4D => 0x4F, // Right
            0x4F => 0x4D, // End
            0x50 => 0x51, // Down
            0x51 => 0x4E, // Page Down
            0x52 => 0x49, // Insert
            0x53 => 0x4C, // Delete
            0x5B => 0xE3, // Left Windows
            0x5C => 0xE7, // Right Windows
            0x5D => 0x65, // Application (Menu)
            0x5E => 0x66, // Power
            // E0 2A / E0 36 are the fake shifts Windows adds around
            // navigation keys with Num Lock on: not real keys.
            _ => return None,
        }
    } else {
        match scan {
            0x01 => 0x29, // Esc
            0x02..=0x0A => scan - 0x02 + 0x1E, // 1..9
            0x0B => 0x27, // 0
            0x0C => 0x2D, // -
            0x0D => 0x2E, // =
            0x0E => 0x2A, // Backspace
            0x0F => 0x2B, // Tab
            0x10 => 0x14, // Q
            0x11 => 0x1A, // W
            0x12 => 0x08, // E
            0x13 => 0x15, // R
            0x14 => 0x17, // T
            0x15 => 0x1C, // Y
            0x16 => 0x18, // U
            0x17 => 0x0C, // I
            0x18 => 0x12, // O
            0x19 => 0x13, // P
            0x1A => 0x2F, // [
            0x1B => 0x30, // ]
            0x1C => 0x28, // Enter
            0x1D => 0xE0, // Left Ctrl
            0x1E => 0x04, // A
            0x1F => 0x16, // S
            0x20 => 0x07, // D
            0x21 => 0x09, // F
            0x22 => 0x0A, // G
            0x23 => 0x0B, // H
            0x24 => 0x0D, // J
            0x25 => 0x0E, // K
            0x26 => 0x0F, // L
            0x27 => 0x33, // ;
            0x28 => 0x34, // '
            0x29 => 0x35, // `
            0x2A => 0xE1, // Left Shift
            0x2B => 0x31, // backslash (ISO: # key)
            0x2C => 0x1D, // Z
            0x2D => 0x1B, // X
            0x2E => 0x06, // C
            0x2F => 0x19, // V
            0x30 => 0x05, // B
            0x31 => 0x11, // N
            0x32 => 0x10, // M
            0x33 => 0x36, // ,
            0x34 => 0x37, // .
            0x35 => 0x38, // /
            0x36 => 0xE5, // Right Shift
            0x37 => 0x55, // KP *
            0x38 => 0xE2, // Left Alt
            0x39 => 0x2C, // Space
            0x3A => 0x39, // Caps Lock
            0x3B..=0x44 => scan - 0x3B + 0x3A, // F1..F10
            0x45 => 0x53, // Num Lock (Pause: see above)
            0x46 => 0x47, // Scroll Lock
            0x47 => 0x5F, // KP 7
            0x48 => 0x60, // KP 8
            0x49 => 0x61, // KP 9
            0x4A => 0x56, // KP -
            0x4B => 0x5C, // KP 4
            0x4C => 0x5D, // KP 5
            0x4D => 0x5E, // KP 6
            0x4E => 0x57, // KP +
            0x4F => 0x59, // KP 1
            0x50 => 0x5A, // KP 2
            0x51 => 0x5B, // KP 3
            0x52 => 0x62, // KP 0
            0x53 => 0x63, // KP .
            0x54 => 0x46, // SysRq (Alt+Print Screen)
            0x56 => 0x64, // Non-US backslash (102nd key)
            0x57 => 0x44, // F11
            0x58 => 0x45, // F12
            0x59 => 0x67, // KP =
            0x64..=0x6E => scan - 0x64 + 0x68, // F13..F23
            0x70 => 0x88, // International2 (Katakana/Hiragana)
            0x73 => 0x87, // International1 (Ro)
            0x76 => 0x73, // F24
            0x79 => 0x8A, // International4 (Henkan)
            0x7B => 0x8B, // International5 (Muhenkan)
            0x7D => 0x89, // International3 (Yen)
            0x7E => 0x85, // KP , (Brazil)
            0xF1 => 0x91, // LANG2 (Hanja)
            0xF2 => 0x90, // LANG1 (Hangeul)
            _ => return None,
        }
    };
    Some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linux key codes 1..=88 are the set-1 scan codes, so every plain
    /// key must come back as its own scan code.
    #[test]
    fn scancodes_round_trip_through_linux_codes() {
        for scan in 0x01..=0x58u16 {
            if scan == 0x54 || scan == 0x55 {
                continue; // SysRq (Linux 99); 0x55 is unused
            }
            let usage = scancode_to_hid(scan, false).unwrap_or_else(|| panic!("scan {scan:#x} unmapped"));
            assert_eq!(hid_to_linux(usage), Some(scan), "scan {scan:#x} → HID {usage:#x}");
        }
    }

    #[test]
    fn extended_scancodes_match_linux_codes() {
        let expected = [
            (0x1C, 96), (0x1D, 97), (0x35, 98), (0x37, 99), (0x38, 100), (0x47, 102),
            (0x48, 103), (0x49, 104), (0x4B, 105), (0x4D, 106), (0x4F, 107), (0x50, 108),
            (0x51, 109), (0x52, 110), (0x53, 111), (0x5B, 125), (0x5C, 126), (0x5D, 127),
            (0x45, 69), (0x46, 119),
        ];
        for (scan, linux) in expected {
            let usage = scancode_to_hid(scan, true).unwrap();
            assert_eq!(hid_to_linux(usage), Some(linux), "E0 {scan:#x}");
        }
        // Left/right modifiers stay distinct.
        assert_eq!(scancode_to_hid(0x1D, false), Some(0xE0));
        assert_eq!(scancode_to_hid(0x1D, true), Some(0xE4));
        assert_eq!(scancode_to_hid(0x38, true), Some(hid::RIGHT_ALT));
        assert_eq!(scancode_to_hid(0x5C, true), Some(hid::RIGHT_GUI));
        // Fake shifts around navigation keys are dropped.
        assert_eq!(scancode_to_hid(0x2A, true), None);
        assert_eq!(scancode_to_hid(0x36, true), None);
    }

    #[test]
    fn linux_codes_round_trip() {
        for usage in 0x04..=0xE7u16 {
            if let Some(code) = hid_to_linux(usage) {
                let back = linux_to_hid(code).unwrap();
                assert_eq!(hid_to_linux(back), Some(code), "HID {usage:#x}");
            }
        }
        assert_eq!(linux_to_hid(43), Some(0x31)); // backslash, not ISO #
        assert_eq!(linux_to_hid(100), Some(hid::RIGHT_ALT));
        assert_eq!(linux_to_hid(126), Some(hid::RIGHT_GUI));
        assert_eq!(linux_to_hid(0), None);
    }

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

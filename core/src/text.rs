//! Text and key names → HID usages (US layout), for typing text into a
//! machine and for key combos like "ctrl+alt+del". Shared by the CLI and the
//! app's paste-as-keystrokes.

use crate::keymap::hid;

pub const LEFT_CTRL: u16 = 0xE0;
pub const LEFT_SHIFT: u16 = 0xE1;

/// The key (and whether Shift is needed) that types `c` on a US keyboard.
pub fn key_for_char(c: char) -> Option<(u16, bool)> {
    let plain = |u: u16| Some((u, false));
    let shifted = |u: u16| Some((u, true));
    match c {
        'a'..='z' => plain(0x04 + (c as u16 - 'a' as u16)),
        'A'..='Z' => shifted(0x04 + (c as u16 - 'A' as u16)),
        '1'..='9' => plain(0x1E + (c as u16 - '1' as u16)),
        '0' => plain(0x27),
        '\n' | '\r' => plain(0x28),
        '\t' => plain(0x2B),
        ' ' => plain(0x2C),
        '\u{8}' => plain(0x2A),
        '\u{1b}' => plain(hid::ESCAPE),
        '-' => plain(0x2D),
        '_' => shifted(0x2D),
        '=' => plain(0x2E),
        '+' => shifted(0x2E),
        '[' => plain(0x2F),
        '{' => shifted(0x2F),
        ']' => plain(0x30),
        '}' => shifted(0x30),
        '\\' => plain(0x31),
        '|' => shifted(0x31),
        ';' => plain(0x33),
        ':' => shifted(0x33),
        '\'' => plain(0x34),
        '"' => shifted(0x34),
        '`' => plain(0x35),
        '~' => shifted(0x35),
        ',' => plain(0x36),
        '<' => shifted(0x36),
        '.' => plain(0x37),
        '>' => shifted(0x37),
        '/' => plain(0x38),
        '?' => shifted(0x38),
        '!' => shifted(0x1E),
        '@' => shifted(0x1F),
        '#' => shifted(0x20),
        '$' => shifted(0x21),
        '%' => shifted(0x22),
        '^' => shifted(0x23),
        '&' => shifted(0x24),
        '*' => shifted(0x25),
        '(' => shifted(0x26),
        ')' => shifted(0x27),
        _ => None,
    }
}

/// HID usage for a key name: "a", "5", "f12", "enter", "lctrl", "ralt", "/".
pub fn key_by_name(name: &str) -> Option<u16> {
    let n = name.trim().to_ascii_lowercase();
    if let Some(num) = n.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()) {
        return match num {
            1..=12 => Some(0x3A + num - 1),
            13..=24 => Some(0x68 + num - 13),
            _ => None,
        };
    }
    let usage = match n.as_str() {
        "ctrl" | "control" | "lctrl" => 0xE0,
        "shift" | "lshift" => 0xE1,
        "alt" | "lalt" | "option" => 0xE2,
        "meta" | "gui" | "win" | "cmd" | "command" | "super" | "lmeta" => 0xE3,
        "rctrl" => 0xE4,
        "rshift" => 0xE5,
        "ralt" | "altgr" | "roption" => 0xE6,
        "rmeta" | "rgui" | "rwin" | "rcmd" => 0xE7,
        "esc" | "escape" => 0x29,
        "enter" | "return" => 0x28,
        "tab" => 0x2B,
        "space" => 0x2C,
        "backspace" | "bs" => 0x2A,
        "delete" | "del" => 0x4C,
        "insert" | "ins" => 0x49,
        "home" => 0x4A,
        "end" => 0x4D,
        "pageup" | "pgup" => 0x4B,
        "pagedown" | "pgdn" => 0x4E,
        "up" => 0x52,
        "down" => 0x51,
        "left" => 0x50,
        "right" => 0x4F,
        "capslock" | "caps" => 0x39,
        "numlock" => 0x53,
        "scrolllock" => 0x47,
        "printscreen" | "prtsc" | "sysrq" => 0x46,
        "pause" | "break" => 0x48,
        "menu" | "osd" => 0x45, // MiSTer menu = F12
        "app" | "compose" => 0x65,
        _ => {
            // Single characters: their unshifted key.
            let mut chars = n.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            return key_for_char(c).map(|(u, _)| u);
        }
    };
    Some(usage)
}

/// Parse "ctrl+alt+del" into usages, in press order.
pub fn parse_combo(combo: &str) -> Result<Vec<u16>, String> {
    // "+" alone (or at the end, as in "shift++") means the plus key.
    let parts: Vec<&str> = if combo == "+" { vec!["+"] } else { combo.split('+').collect() };
    let mut keys = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        let p = if p.is_empty() && i == parts.len() - 1 { "=" } else { p };
        if p.is_empty() {
            continue;
        }
        keys.push(key_by_name(p).ok_or_else(|| format!("unknown key '{p}'"))?);
    }
    if keys.is_empty() {
        return Err("no keys given".into());
    }
    Ok(keys)
}

/// Replace C-style escapes (\n, \t, \e, \\) typed on a command line.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\u{1b}'),
            Some('b') => out.push('\u{8}'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chars() {
        assert_eq!(key_for_char('a'), Some((0x04, false)));
        assert_eq!(key_for_char('Z'), Some((0x1D, true)));
        assert_eq!(key_for_char('"'), Some((0x34, true)));
        assert_eq!(key_for_char('\n'), Some((0x28, false)));
        assert_eq!(key_for_char('é'), None);
    }

    #[test]
    fn combos() {
        assert_eq!(parse_combo("ctrl+alt+del"), Ok(vec![0xE0, 0xE2, 0x4C]));
        assert_eq!(parse_combo("F12"), Ok(vec![0x45]));
        assert_eq!(parse_combo("shift+/"), Ok(vec![0xE1, 0x38]));
        assert_eq!(parse_combo("rmeta+ralt+esc"), Ok(vec![0xE7, 0xE6, 0x29]));
        assert!(parse_combo("ctrl+nope").is_err());
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape(r#"LOAD "*",8,1\n"#), "LOAD \"*\",8,1\n");
        assert_eq!(unescape(r"a\\b"), r"a\b");
    }
}

//! What the controlling viewer is holding down on the MiSTer, read from the
//! mrext messages it sends, so the server can release it all when control
//! moves or the viewer disconnects (no stuck keys or buttons).

use std::collections::BTreeSet;

use crate::message::InputCmd;

#[derive(Debug, Default)]
pub struct HeldInput {
    keys: BTreeSet<u32>,
    buttons: BTreeSet<String>,
}

impl HeldInput {
    pub fn observe(&mut self, cmd: &InputCmd) {
        let InputCmd::Send { m } = cmd else { return };
        if let Some(code) = m.strip_prefix("kbdRawDown:").and_then(|c| c.parse().ok()) {
            self.keys.insert(code);
        } else if let Some(code) = m.strip_prefix("kbdRawUp:").and_then(|c| c.parse::<u32>().ok()) {
            self.keys.remove(&code);
        } else if let Some(b) = m.strip_prefix("mouseBtn:") {
            if let Some(name) = b.strip_suffix("_down") {
                self.buttons.insert(name.to_string());
            } else if let Some(name) = b.strip_suffix("_up") {
                self.buttons.remove(name);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }

    /// mrext messages that release everything held, and forget it.
    pub fn release_all(&mut self) -> Vec<String> {
        let mut out: Vec<String> = self.keys.iter().map(|k| format!("kbdRawUp:{k}")).collect();
        out.extend(self.buttons.iter().map(|b| format!("mouseBtn:{b}_up")));
        self.keys.clear();
        self.buttons.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(m: &str) -> InputCmd {
        InputCmd::Send { m: m.into() }
    }

    #[test]
    fn tracks_keys_and_buttons() {
        let mut h = HeldInput::default();
        for m in ["kbdRawDown:42", "kbdRawDown:30", "kbdRawUp:30", "mouseBtn:left_down", "mouseBtn:right_down", "mouseBtn:right_up"] {
            h.observe(&send(m));
        }
        h.observe(&InputCmd::Move { dx: 1.0, dy: 1.0 });
        assert_eq!(h.release_all(), vec!["kbdRawUp:42".to_string(), "mouseBtn:left_up".to_string()]);
        assert!(h.is_empty());
    }
}

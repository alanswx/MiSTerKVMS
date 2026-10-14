//! Who controls a stream. Any number of viewers watch; one at a time sends
//! input. Taking control always works, at once: the point is only that two
//! machines never type at the same time (docs/CAPTURE-SERVER.md §7).

pub type ViewerId = u32;

/// Control moved from one viewer (or nobody) to another (or nobody). The
/// server releases everything held on the MiSTer and tells every viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlChange {
    pub from: Option<ViewerId>,
    pub to: Option<ViewerId>,
}

#[derive(Debug, Default)]
pub struct Control {
    holder: Option<ViewerId>,
}

impl Control {
    pub fn holder(&self) -> Option<ViewerId> {
        self.holder
    }

    /// A viewer sent input. Returns whether to forward it, and the change if
    /// the viewer just took a free token. Input from anyone else while a
    /// holder exists is dropped.
    pub fn input(&mut self, viewer: ViewerId) -> (bool, Option<ControlChange>) {
        match self.holder {
            Some(h) if h == viewer => (true, None),
            Some(_) => (false, None),
            None => {
                self.holder = Some(viewer);
                (true, Some(ControlChange { from: None, to: Some(viewer) }))
            }
        }
    }

    /// "Take control": always granted.
    pub fn take(&mut self, viewer: ViewerId) -> Option<ControlChange> {
        if self.holder == Some(viewer) {
            return None;
        }
        let from = self.holder.replace(viewer);
        Some(ControlChange { from, to: Some(viewer) })
    }

    /// A viewer disconnected. Frees the token if it held it.
    pub fn leave(&mut self, viewer: ViewerId) -> Option<ControlChange> {
        if self.holder == Some(viewer) {
            self.holder = None;
            Some(ControlChange { from: Some(viewer), to: None })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_input_takes_a_free_token() {
        let mut c = Control::default();
        assert_eq!(c.input(1), (true, Some(ControlChange { from: None, to: Some(1) })));
        assert_eq!(c.input(1), (true, None));
        assert_eq!(c.input(2), (false, None), "only the holder's input goes through");
    }

    #[test]
    fn take_always_wins_and_can_be_taken_back() {
        let mut c = Control::default();
        c.input(1);
        assert_eq!(c.take(2), Some(ControlChange { from: Some(1), to: Some(2) }));
        assert_eq!(c.input(1), (false, None));
        assert_eq!(c.take(1), Some(ControlChange { from: Some(2), to: Some(1) }));
        assert_eq!(c.take(1), None, "already holding");
    }

    #[test]
    fn leaving_frees_the_token_only_for_the_holder() {
        let mut c = Control::default();
        c.take(1);
        assert_eq!(c.leave(2), None);
        assert_eq!(c.leave(1), Some(ControlChange { from: Some(1), to: None }));
        assert_eq!(c.holder(), None);
        assert_eq!(c.input(2).0, true);
    }
}

//! The share confirmation. Sharing sends the board to the relay server, outside this browser,
//! so nothing is sent until the person answers Share; Cancel (or Esc) keeps the board local.

/// The person's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Share,
    Cancel,
}

/// What a key does while the dialog is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskKey {
    /// Esc: the same as Cancel.
    Cancel,
    /// Tab or Shift+Tab: focus this button; focus never leaves the dialog.
    Focus(usize),
    /// Anything else: the browser's own handling (Enter or Space presses the focused button).
    None,
}

/// `at`: the focused button's index, if one of the `n` buttons has focus.
pub fn ask_key(key: &str, shift: bool, at: Option<usize>, n: usize) -> AskKey {
    match key {
        "Escape" => AskKey::Cancel,
        "Tab" if n > 0 => AskKey::Focus(match at {
            None if shift => n - 1,
            None => 0,
            Some(i) if shift => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
        }),
        _ => AskKey::None,
    }
}

/// Whether the dialog is open. A share starts only from an answer to an open dialog.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShareGate {
    open: bool,
}

impl ShareGate {
    /// Opens the dialog; `false` if it was already open.
    pub fn ask(&mut self) -> bool {
        !std::mem::replace(&mut self.open, true)
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Closes the dialog; `true` only for Share to an open dialog: the one case that may
    /// contact the relay.
    pub fn answer(&mut self, a: Answer) -> bool {
        std::mem::replace(&mut self.open, false) && a == Answer::Share
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page's wiring, in miniature: the relay is contacted only when `answer` says so.
    struct Page {
        gate: ShareGate,
        relay_calls: usize,
    }

    impl Page {
        fn share_now(&mut self) {
            self.gate.ask();
        }
        fn answer(&mut self, a: Answer) {
            if self.gate.answer(a) {
                self.relay_calls += 1;
            }
        }
        fn key(&mut self, k: &str) {
            if ask_key(k, false, None, 2) == AskKey::Cancel {
                self.answer(Answer::Cancel);
            }
        }
    }

    #[test]
    fn no_relay_connection_until_share_is_confirmed() {
        let mut p = Page { gate: ShareGate::default(), relay_calls: 0 };
        p.share_now();
        assert!(p.gate.is_open());
        assert_eq!(p.relay_calls, 0, "asking sends nothing");
        p.answer(Answer::Cancel);
        assert_eq!(p.relay_calls, 0, "Cancel keeps the board local");
        p.share_now();
        p.key("Escape");
        assert!(!p.gate.is_open());
        assert_eq!(p.relay_calls, 0, "Esc cancels");
        p.answer(Answer::Share);
        assert_eq!(p.relay_calls, 0, "a Share with no dialog open does nothing");
        p.share_now();
        assert!(!p.gate.ask(), "asking twice keeps one dialog");
        p.answer(Answer::Share);
        assert_eq!(p.relay_calls, 1, "Share connects, once");
        p.answer(Answer::Share);
        assert_eq!(p.relay_calls, 1);
    }

    #[test]
    fn tab_keeps_focus_inside_the_dialog() {
        assert_eq!(ask_key("Tab", false, Some(0), 2), AskKey::Focus(1));
        assert_eq!(ask_key("Tab", false, Some(1), 2), AskKey::Focus(0));
        assert_eq!(ask_key("Tab", true, Some(0), 2), AskKey::Focus(1));
        assert_eq!(ask_key("Tab", false, None, 2), AskKey::Focus(0));
        assert_eq!(ask_key("Tab", true, None, 2), AskKey::Focus(1));
        assert_eq!(ask_key("Escape", true, Some(1), 2), AskKey::Cancel);
        assert_eq!(ask_key("Enter", false, Some(1), 2), AskKey::None);
    }
}

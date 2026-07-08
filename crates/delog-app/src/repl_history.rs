/// Session-only REPL command history with shell-style up/down navigation.
///
/// `pos` tracks where the user is: `None` means they're editing the live draft;
/// `Some(i)` means they're viewing `entries[i]`. `draft` preserves the
/// in-progress line while navigating so it can be restored on the way back.
#[derive(Default)]
pub struct ReplHistory {
    entries: Vec<String>,
    pos: Option<usize>,
    draft: String,
}

impl ReplHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a submitted line, skipping blanks and consecutive duplicates, and
    /// reset navigation back to the live draft.
    pub fn push(&mut self, line: &str) {
        if !line.trim().is_empty() && self.entries.last().map(String::as_str) != Some(line) {
            self.entries.push(line.to_string());
        }
        self.pos = None;
        self.draft.clear();
    }

    /// Step to an older entry. `current` is the live buffer, saved as the draft
    /// the first time navigation leaves it. Returns the line to display, or
    /// `None` when there is nothing older to show.
    pub fn older(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.pos {
            None => {
                self.draft = current.to_string();
                self.entries.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.pos = Some(next);
        Some(self.entries[next].clone())
    }

    /// Step to a newer entry, or restore the draft once past the newest entry.
    /// Returns the line to display, or `None` when already at the draft.
    pub fn newer(&mut self) -> Option<String> {
        match self.pos {
            None => None,
            Some(i) if i + 1 < self.entries.len() => {
                self.pos = Some(i + 1);
                Some(self.entries[i + 1].clone())
            }
            Some(_) => {
                self.pos = None;
                Some(std::mem::take(&mut self.draft))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_skips_blank_and_consecutive_dups() {
        let mut h = ReplHistory::new();
        h.push("a");
        h.push("a");
        h.push("  ");
        h.push("b");
        assert_eq!(h.older(""), Some("b".into()));
        assert_eq!(h.older(""), Some("a".into()));
        assert_eq!(h.older(""), Some("a".into())); // clamped at the oldest
    }

    #[test]
    fn up_down_walks_and_restores_draft() {
        let mut h = ReplHistory::new();
        h.push("first");
        h.push("second");
        assert_eq!(h.older("draft"), Some("second".into())); // saves the draft
        assert_eq!(h.older("second"), Some("first".into()));
        assert_eq!(h.newer(), Some("second".into()));
        assert_eq!(h.newer(), Some("draft".into())); // draft restored past newest
        assert_eq!(h.newer(), None); // at the draft, nothing newer
    }

    #[test]
    fn older_on_empty_history_returns_none() {
        let mut h = ReplHistory::new();
        assert_eq!(h.older("x"), None);
    }

    #[test]
    fn push_resets_navigation() {
        let mut h = ReplHistory::new();
        h.push("a");
        h.push("b");
        h.older("d"); // now viewing "b"
        h.push("c"); // submitting resets navigation
        assert_eq!(h.older("d2"), Some("c".into()));
    }
}

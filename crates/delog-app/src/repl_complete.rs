/// Best-effort byte offset of the text cursor in a focused `TextEdit`. Falls
/// back to the end of `text` (the common Tab-at-end-of-line case) when the
/// cursor state is unavailable.
pub fn cursor_byte(ctx: &egui::Context, id: egui::Id, text: &str) -> usize {
    egui::text_edit::TextEditState::load(ctx, id)
        .and_then(|state| state.cursor.char_range())
        .map(|range| {
            let char_idx = range.primary.index;
            text.char_indices()
                .nth(char_idx)
                .map(|(b, _)| b)
                .unwrap_or(text.len())
        })
        .unwrap_or(text.len())
}

/// Move a focused `TextEdit`'s cursor to byte offset `byte` in `text` (a single
/// collapsed caret). No-op if the widget's state isn't loaded yet.
pub fn set_cursor_byte(ctx: &egui::Context, id: egui::Id, text: &str, byte: usize) {
    let char_idx = text
        .get(..byte.min(text.len()))
        .map(|s| s.chars().count())
        .unwrap_or_else(|| text.chars().count());
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) {
        let ccursor = egui::text::CCursor::new(char_idx);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(ccursor)));
        state.store(ctx, id);
    }
}

/// Build a monospace layout for one candidate, accenting the byte range
/// `[hi_start, hi_end)` (the matched fragment) against normal text. Falls back
/// to all-normal if the range isn't a valid slice of `text`.
fn match_highlight_job(
    ui: &egui::Ui,
    text: &str,
    hi_start: usize,
    hi_end: usize,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let normal = ui.visuals().text_color();
    let accent = ui.visuals().hyperlink_color;
    let (hi_start, hi_end) = if hi_start <= hi_end
        && hi_end <= text.len()
        && text.is_char_boundary(hi_start)
        && text.is_char_boundary(hi_end)
    {
        (hi_start, hi_end)
    } else {
        (0, 0)
    };
    let mut job = LayoutJob::default();
    for (segment, color) in [
        (&text[..hi_start], normal),
        (&text[hi_start..hi_end], accent),
        (&text[hi_end..], normal),
    ] {
        if !segment.is_empty() {
            job.append(
                segment,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color,
                    ..Default::default()
                },
            );
        }
    }
    job
}

/// The byte range and slice of the completable token ending at `cursor` (a byte
/// offset into `line`): the longest run of identifier characters and dots
/// immediately before the cursor. `None` when that run is empty.
pub fn completable_token(line: &str, cursor: usize) -> Option<(usize, &str)> {
    let cursor = cursor.min(line.len());
    let head = match line.get(..cursor) {
        Some(head) => head,
        None => return None,
    };
    let start = head
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
        .last()
        .map(|(i, _)| i)
        .unwrap_or(cursor);
    if start == cursor {
        None
    } else {
        Some((start, &line[start..cursor]))
    }
}

/// The longest string that every item starts with.
pub fn longest_common_prefix(items: &[String]) -> String {
    let mut iter = items.iter();
    let mut prefix = match iter.next() {
        Some(first) => first.clone(),
        None => return String::new(),
    };
    for item in iter {
        while !item.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                return String::new();
            }
        }
    }
    prefix
}

/// An open completion dropdown: the buffer byte range it will replace, the
/// candidates, and the highlighted index.
pub struct Popup {
    pub start: usize,
    pub end: usize,
    pub matches: Vec<String>,
    pub selected: usize,
}

struct Pending {
    seq: u64,
    start: usize,
    end: usize,
    token: String,
}

/// REPL completion state. One outstanding request at a time; the popup opens
/// only when a response returns multiple candidates.
pub struct ReplCompletion {
    next_seq: u64,
    pending: Option<Pending>,
    popup: Option<Popup>,
    /// Buffer byte offset the input cursor should jump to after a completion
    /// mutates the buffer; consumed by the UI once per frame.
    pending_cursor: Option<usize>,
}

impl ReplCompletion {
    pub fn new() -> Self {
        Self {
            next_seq: 0,
            pending: None,
            popup: None,
            pending_cursor: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.popup.is_some()
    }

    /// The pending post-completion cursor byte offset, cleared on read.
    pub fn take_pending_cursor(&mut self) -> Option<usize> {
        self.pending_cursor.take()
    }

    #[cfg(test)]
    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    /// Record a new outstanding request, superseding any earlier one and closing
    /// any open popup. `start..end` is the buffer byte range being completed.
    pub fn begin_request(&mut self, start: usize, end: usize, token: String) -> u64 {
        self.popup = None;
        self.next_seq += 1;
        let seq = self.next_seq;
        self.pending = Some(Pending {
            seq,
            start,
            end,
            token,
        });
        seq
    }

    /// Apply a completion response. Ignores stale responses (wrong `seq`) and
    /// responses whose token span no longer matches the buffer. Returns whether
    /// the buffer changed.
    pub fn on_completions(
        &mut self,
        seq: u64,
        matches: Vec<String>,
        buffer: &mut String,
    ) -> bool {
        let pending = match self.pending.take() {
            Some(p) if p.seq == seq => p,
            other => {
                self.pending = other;
                return false;
            }
        };
        if buffer.get(pending.start..pending.end) != Some(pending.token.as_str()) {
            return false;
        }
        match matches.len() {
            0 => false,
            1 => {
                buffer.replace_range(pending.start..pending.end, &matches[0]);
                self.pending_cursor = Some(pending.start + matches[0].len());
                true
            }
            _ => {
                let lcp = longest_common_prefix(&matches);
                let extended = lcp.len() > pending.token.len();
                let new_token = if extended { lcp } else { pending.token.clone() };
                if extended {
                    buffer.replace_range(pending.start..pending.end, &new_token);
                    self.pending_cursor = Some(pending.start + new_token.len());
                }
                let popup_end = pending.start + new_token.len();
                self.popup = Some(Popup {
                    start: pending.start,
                    end: popup_end,
                    matches,
                    selected: 0,
                });
                extended
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if let Some(p) = &mut self.popup {
            if p.matches.is_empty() {
                return;
            }
            let next = (p.selected as isize + delta).clamp(0, p.matches.len() as isize - 1);
            p.selected = next as usize;
        }
    }

    /// Replace the popup's range with the highlighted candidate and close.
    pub fn accept_selected(&mut self, buffer: &mut String) {
        if let Some(popup) = self.popup.take() {
            if let Some(choice) = popup.matches.get(popup.selected) {
                if buffer.is_char_boundary(popup.start)
                    && popup.end <= buffer.len()
                    && buffer.is_char_boundary(popup.end)
                {
                    buffer.replace_range(popup.start..popup.end, choice);
                    self.pending_cursor = Some(popup.start + choice.len());
                }
            }
        }
    }

    pub fn dismiss(&mut self) {
        self.popup = None;
    }

    /// Render the dropdown (when open) anchored under `input`, handling keyboard
    /// navigation and mouse selection. Returns true if it consumed an Enter this
    /// frame (so the caller must not also submit the line).
    pub fn handle_popup(
        &mut self,
        ui: &mut egui::Ui,
        input: &egui::Response,
        buffer: &mut String,
    ) -> bool {
        if self.popup.is_none() {
            return false;
        }
        let down = ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                || i.consume_key(egui::Modifiers::CTRL, egui::Key::N)
        });
        let up = ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                || i.consume_key(egui::Modifiers::CTRL, egui::Key::P)
        });
        let accept = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let dismiss = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if down {
            self.move_selection(1);
        }
        if up {
            self.move_selection(-1);
        }
        if dismiss {
            self.dismiss();
            input.request_focus();
            return false;
        }
        if accept {
            self.accept_selected(buffer);
            input.request_focus();
            return true;
        }

        let (selected, matches, token) = {
            let p = self.popup.as_ref().unwrap();
            let token = buffer.get(p.start..p.end).unwrap_or("").to_string();
            (p.selected, p.matches.clone(), token)
        };
        // Accent the fragment after the token's last dot (the part being typed).
        let hi_start = token.rfind('.').map(|i| i + 1).unwrap_or(0);
        let hi_end = token.len();
        let mut clicked: Option<usize> = None;
        // The console input sits at the bottom of the window, so grow the
        // dropdown upward from the input's top edge where there is room.
        egui::Area::new(ui.id().with("repl_completion_popup"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(input.rect.left_top())
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(input.rect.width().max(120.0));
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (i, m) in matches.iter().enumerate() {
                                let resp = ui.selectable_label(
                                    i == selected,
                                    match_highlight_job(ui, m, hi_start, hi_end),
                                );
                                if i == selected {
                                    resp.scroll_to_me(Some(egui::Align::Center));
                                }
                                if resp.clicked() {
                                    clicked = Some(i);
                                }
                            }
                        });
                });
            });
        if let Some(i) = clicked {
            if let Some(p) = &mut self.popup {
                p.selected = i;
            }
            self.accept_selected(buffer);
            input.request_focus();
        }
        false
    }
}

impl Default for ReplCompletion {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn token_is_trailing_identifier_run() {
        assert_eq!(completable_token("delog.fi", 8), Some((0, "delog.fi")));
        assert_eq!(completable_token("x = delog.fi", 12), Some((4, "delog.fi")));
        assert_eq!(completable_token("f.t", 3), Some((0, "f.t")));
    }

    #[test]
    fn token_stops_at_cursor_not_end() {
        assert_eq!(completable_token("abc def", 3), Some((0, "abc")));
    }

    #[test]
    fn token_none_when_empty() {
        assert_eq!(completable_token("x = ", 4), None);
        assert_eq!(completable_token("", 0), None);
    }

    #[test]
    fn token_none_when_cursor_not_on_char_boundary() {
        // "é" is two bytes; cursor 1 lands mid-character.
        assert_eq!(completable_token("é", 1), None);
    }

    #[test]
    fn lcp_finds_shared_prefix() {
        assert_eq!(
            longest_common_prefix(&s(&["delog.field", "delog.find", "delog.find_all"])),
            "delog.fi"
        );
        assert_eq!(longest_common_prefix(&s(&["abc"])), "abc");
        assert_eq!(longest_common_prefix(&s(&[])), "");
        assert_eq!(longest_common_prefix(&s(&["ab", "cd"])), "");
    }

    #[test]
    fn single_match_is_inserted_without_popup() {
        let mut c = ReplCompletion::new();
        let mut buf = String::from("de");
        let seq = c.begin_request(0, 2, "de".into());
        let changed = c.on_completions(seq, s(&["delog"]), &mut buf);
        assert!(changed);
        assert_eq!(buf, "delog");
        assert!(!c.is_open());
    }

    #[test]
    fn multiple_matches_open_popup_and_extend_prefix() {
        let mut c = ReplCompletion::new();
        let mut buf = String::from("delog.f");
        let seq = c.begin_request(0, 7, "delog.f".into());
        c.on_completions(seq, s(&["delog.field", "delog.find"]), &mut buf);
        assert_eq!(buf, "delog.fi"); // extended to the common prefix
        assert!(c.is_open());
        assert_eq!(c.popup().unwrap().matches.len(), 2);
        assert_eq!(c.popup().unwrap().selected, 0);
    }

    #[test]
    fn stale_response_is_ignored() {
        let mut c = ReplCompletion::new();
        let mut buf = String::from("de");
        let _seq = c.begin_request(0, 2, "de".into());
        let changed = c.on_completions(999, s(&["delog"]), &mut buf);
        assert!(!changed);
        assert_eq!(buf, "de");
    }

    #[test]
    fn edited_buffer_guards_application() {
        let mut c = ReplCompletion::new();
        let mut buf = String::from("de");
        let seq = c.begin_request(0, 2, "de".into());
        buf.push_str("XYZ"); // user typed after pressing Tab
        buf.replace_range(0..2, "zz"); // token span no longer matches
        let changed = c.on_completions(seq, s(&["delog"]), &mut buf);
        assert!(!changed);
    }

    #[test]
    fn accept_selected_inserts_and_closes() {
        let mut c = ReplCompletion::new();
        let mut buf = String::from("delog.fi");
        let seq = c.begin_request(0, 8, "delog.fi".into());
        c.on_completions(seq, s(&["delog.field", "delog.find"]), &mut buf);
        c.move_selection(1);
        c.accept_selected(&mut buf);
        assert_eq!(buf, "delog.find");
        assert!(!c.is_open());
    }

    #[test]
    fn single_match_moves_cursor_to_end() {
        let mut c = ReplCompletion::new();
        let mut buf = String::from("de");
        let seq = c.begin_request(0, 2, "de".into());
        c.on_completions(seq, s(&["delog"]), &mut buf);
        assert_eq!(c.take_pending_cursor(), Some("delog".len()));
    }

    #[test]
    fn accept_moves_cursor_to_end() {
        let mut c = ReplCompletion::new();
        let mut buf = String::from("delog.fi");
        let seq = c.begin_request(0, 8, "delog.fi".into());
        c.on_completions(seq, s(&["delog.field", "delog.find"]), &mut buf);
        c.move_selection(1);
        c.accept_selected(&mut buf);
        assert_eq!(c.take_pending_cursor(), Some("delog.find".len()));
    }

    #[test]
    fn move_selection_clamps() {
        let mut c = ReplCompletion::new();
        let mut buf = String::from("delog.fi");
        let seq = c.begin_request(0, 8, "delog.fi".into());
        c.on_completions(seq, s(&["delog.field", "delog.find"]), &mut buf);
        c.move_selection(-1);
        assert_eq!(c.popup().unwrap().selected, 0);
        c.move_selection(5);
        assert_eq!(c.popup().unwrap().selected, 1);
    }

    #[test]
    fn dismiss_closes_popup() {
        let mut c = ReplCompletion::new();
        let mut buf = String::from("delog.fi");
        let seq = c.begin_request(0, 8, "delog.fi".into());
        c.on_completions(seq, s(&["delog.field", "delog.find"]), &mut buf);
        assert!(c.is_open());
        c.dismiss();
        assert!(!c.is_open());
    }
}

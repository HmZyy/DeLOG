use delog_core::field_view::{FieldView, SampleMode};
use delog_core::identity::FieldId;
use delog_core::snapshot::StoreSnapshot;

use egui_extras::{Column, TableBuilder};

pub fn current_index(times: &[i64], playhead_us: i64, mode: SampleMode) -> Option<usize> {
    match mode {
        SampleMode::Next => {
            let index = times.partition_point(|&t| t < playhead_us);
            (index < times.len()).then_some(index)
        }
        SampleMode::Prev | SampleMode::Linear => {
            let count = times.partition_point(|&t| t <= playhead_us);
            count.checked_sub(1)
        }
    }
}

pub fn should_follow(last: Option<usize>, current: Option<usize>) -> bool {
    current.is_some() && last != current
}

pub struct TextViewer {
    field: FieldId,
    title: String,
    times: Vec<i64>,
    texts: Vec<String>,
    query: String,
    matched: Vec<usize>,
    matched_for: Option<String>,
    epoch: u64,
    last_current: Option<usize>,
    open: bool,
}

impl TextViewer {
    fn new(snapshot: &StoreSnapshot, field: FieldId) -> Self {
        let mut window = Self {
            field,
            title: crate::plotting::legend::trace_label(snapshot, field),
            times: Vec::new(),
            texts: Vec::new(),
            query: String::new(),
            matched: Vec::new(),
            matched_for: None,
            epoch: u64::MAX,
            last_current: None,
            open: true,
        };
        window.reload(snapshot);
        window
    }

    fn reload(&mut self, snapshot: &StoreSnapshot) {
        self.epoch = snapshot.epoch;
        self.matched_for = None;
        self.times.clear();
        self.texts.clear();
        let Some(range) = snapshot.global_time_range() else {
            return;
        };
        let Ok(view) = FieldView::new(snapshot, self.field) else {
            return;
        };
        let mut samples = view.string_samples_in_range(range, usize::MAX, None);
        samples.sort_by_key(|(t, _)| *t);
        self.times.reserve(samples.len());
        self.texts.reserve(samples.len());
        for (t_us, text) in samples {
            self.times.push(t_us);
            self.texts.push(text);
        }
    }
}

impl TextViewer {
    fn refresh_matches(&mut self) {
        if self.matched_for.as_deref() == Some(self.query.as_str()) {
            return;
        }
        let needle = self.query.trim().to_lowercase();
        self.matched.clear();
        if needle.is_empty() {
            self.matched.extend(0..self.texts.len());
        } else {
            self.matched.extend(
                self.texts
                    .iter()
                    .enumerate()
                    .filter(|(_, text)| text.to_lowercase().contains(needle.as_str()))
                    .map(|(index, _)| index),
            );
        }
        self.matched_for = Some(self.query.clone());
    }
}

#[derive(Default)]
pub struct TextViewers {
    windows: Vec<TextViewer>,
}

impl TextViewers {
    pub fn open(&mut self, snapshot: &StoreSnapshot, field: FieldId) {
        if let Some(window) = self.windows.iter_mut().find(|w| w.field == field) {
            window.open = true;
            return;
        }
        self.windows.push(TextViewer::new(snapshot, field));
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        snapshot: &StoreSnapshot,
        origin_us: i64,
        playhead_us: Option<i64>,
        mode: SampleMode,
    ) -> Option<i64> {
        let mut scrub_to = None;
        for window in &mut self.windows {
            if window.epoch != snapshot.epoch {
                window.reload(snapshot);
            }
            if let Some(time) = show_viewer(ctx, window, origin_us, playhead_us, mode) {
                scrub_to = Some(time);
            }
        }
        self.windows.retain(|window| window.open);
        scrub_to
    }
}

fn show_viewer(
    ctx: &egui::Context,
    window: &mut TextViewer,
    origin_us: i64,
    playhead_us: Option<i64>,
    mode: SampleMode,
) -> Option<i64> {
    let current = playhead_us.and_then(|t_us| current_index(&window.times, t_us, mode));
    let follow = should_follow(window.last_current, current);
    window.last_current = current;

    let mut scrub_to = None;
    let mut open = window.open;
    egui::Window::new(&window.title)
        .id(egui::Id::new(("field_text_viewer", window.field.0)))
        .open(&mut open)
        .collapsible(false)
        .default_width(520.0)
        .resizable(true)
        .show(ctx, |ui| {
            if window.times.is_empty() {
                ui.weak("No text logged for this field.");
                return;
            }
            ui.add(
                egui::TextEdit::singleline(&mut window.query)
                    .hint_text("Search text")
                    .desired_width(f32::INFINITY),
            );
            ui.separator();
            window.refresh_matches();
            if window.matched.is_empty() {
                ui.weak("No text matches the search.");
                return;
            }
            let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
            let past = ui.visuals().text_color();
            let later = past.gamma_multiply(0.45);
            let mut table = TableBuilder::new(ui)
                .id_salt(("field_text_viewer_table", window.field.0))
                .striped(true)
                .resizable(true)
                .animate_scrolling(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .auto_shrink([false, false])
                .column(Column::auto().at_least(88.0))
                .column(Column::remainder().clip(true));
            let current_row = current
                .filter(|_| follow)
                .and_then(|current| window.matched.iter().position(|&index| index == current));
            if let Some(row) = current_row {
                table = table.scroll_to_row(row, Some(egui::Align::Center));
            }
            table
                .header(row_height, |mut header| {
                    header.col(|ui| {
                        ui.strong("Time");
                    });
                    header.col(|ui| {
                        ui.strong("Text");
                    });
                })
                .body(|body| {
                    body.rows(row_height, window.matched.len(), |mut row| {
                        let index = window.matched[row.index()];
                        let is_current = Some(index) == current;
                        row.set_selected(is_current);
                        let color = if current.is_some_and(|current| index > current) {
                            later
                        } else {
                            past
                        };
                        let t_us = window.times[index];
                        row.col(|ui| {
                            let seconds = (t_us - origin_us) as f64 * 1e-6;
                            if ui
                                .add(
                                    egui::Label::new(
                                        egui::RichText::new(format!("{seconds:.3}s")).color(color),
                                    )
                                    .sense(egui::Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text("Jump playhead to this time")
                                .clicked()
                            {
                                scrub_to = Some(t_us);
                            }
                        });
                        row.col(|ui| {
                            ui.colored_label(color, window.texts[index].as_str());
                        });
                    });
                });
        });
    window.open = open;
    scrub_to
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Int64Array, StringArray};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    use super::*;

    const TIMES: [i64; 4] = [100, 200, 300, 400];

    fn snapshot_with_text() -> (StoreSnapshot, FieldId) {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "EVENT").unwrap();
        let field = identity.add_field(topic, "text").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "EVENT",
                [FieldSchema::new("text", DataType::Utf8, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(TIMES.to_vec()),
                vec![Arc::new(StringArray::from(vec![
                    "ARMED", "TAKEOFF", "LANDING", "DISARMED",
                ])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 7).unwrap();
        (snapshot, field)
    }

    fn render(
        ctx: &egui::Context,
        windows: &mut TextViewers,
        snapshot: &StoreSnapshot,
        playhead_us: Option<i64>,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Option<i64>) {
        let mut scrub = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_000.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                scrub = windows.show(ui.ctx(), snapshot, 0, playhead_us, SampleMode::Prev);
                let _ = ui;
            },
        );
        (output, scrub)
    }

    fn painted(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    out.push((text.galley.job.text.clone(), text.visual_bounding_rect()));
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn a_field_gets_one_window_however_often_it_is_opened() {
        let (snapshot, field) = snapshot_with_text();
        let mut windows = TextViewers::default();

        windows.open(&snapshot, field);
        windows.open(&snapshot, field);

        assert_eq!(windows.windows.len(), 1);
        assert_eq!(windows.windows[0].times.len(), TIMES.len());
    }

    #[test]
    fn the_highlighted_row_follows_the_playhead() {
        let (snapshot, field) = snapshot_with_text();
        let mut windows = TextViewers::default();
        windows.open(&snapshot, field);
        let ctx = egui::Context::default();

        for playhead in [150, 150, 150] {
            let _ = render(&ctx, &mut windows, &snapshot, Some(playhead), Vec::new());
        }
        assert_eq!(windows.windows[0].last_current, Some(0));

        let _ = render(&ctx, &mut windows, &snapshot, Some(350), Vec::new());
        assert_eq!(windows.windows[0].last_current, Some(2));

        let _ = render(&ctx, &mut windows, &snapshot, Some(50), Vec::new());
        assert_eq!(
            windows.windows[0].last_current, None,
            "a playhead before every message highlights nothing"
        );
    }

    #[test]
    fn every_message_is_listed_and_clicking_its_time_asks_for_that_playhead() {
        let (snapshot, field) = snapshot_with_text();
        let mut windows = TextViewers::default();
        windows.open(&snapshot, field);
        let ctx = egui::Context::default();

        let mut output = render(&ctx, &mut windows, &snapshot, Some(150), Vec::new()).0;
        for _ in 0..2 {
            output = render(&ctx, &mut windows, &snapshot, Some(150), Vec::new()).0;
        }
        let rows = painted(&output);
        for text in ["ARMED", "TAKEOFF", "LANDING", "DISARMED"] {
            assert!(
                rows.iter().any(|(painted, _)| painted == text),
                "{text} should be listed, got {rows:?}"
            );
        }

        let time = rows
            .iter()
            .find(|(text, _)| text == "0.000s")
            .unwrap_or_else(|| panic!("the first message time should paint, got {rows:?}"))
            .1
            .center();
        let click = vec![
            egui::Event::PointerMoved(time),
            egui::Event::PointerButton {
                pos: time,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: time,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let (_, scrub) = render(&ctx, &mut windows, &snapshot, Some(150), click);

        assert_eq!(scrub, Some(TIMES[0]));
    }

    fn painted_colors(output: &egui::FullOutput) -> Vec<(String, egui::Color32)> {
        fn walk(shape: &egui::epaint::Shape, out: &mut Vec<(String, egui::Color32)>) {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    let color = text
                        .galley
                        .job
                        .sections
                        .first()
                        .map(|section| section.format.color)
                        .filter(|color| *color != egui::Color32::PLACEHOLDER)
                        .unwrap_or(text.fallback_color);
                    out.push((text.galley.job.text.clone(), color));
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn rows_after_the_playhead_are_tinted_apart_from_the_ones_behind_it() {
        let (snapshot, field) = snapshot_with_text();
        let mut windows = TextViewers::default();
        windows.open(&snapshot, field);
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);

        let mut output = render(&ctx, &mut windows, &snapshot, Some(250), Vec::new()).0;
        for _ in 0..2 {
            output = render(&ctx, &mut windows, &snapshot, Some(250), Vec::new()).0;
        }
        let colors = painted_colors(&output);
        let color_of = |wanted: &str| {
            colors
                .iter()
                .find(|(text, _)| text == wanted)
                .unwrap_or_else(|| panic!("{wanted} should paint, got {colors:?}"))
                .1
        };

        assert_eq!(windows.windows[0].last_current, Some(1));
        let brightness = |color: egui::Color32| {
            u32::from(color.r()) + u32::from(color.g()) + u32::from(color.b())
        };
        let past = color_of("ARMED");
        let now = color_of("TAKEOFF");
        let later = color_of("LANDING");

        assert_eq!(
            later,
            color_of("DISARMED"),
            "every row after the playhead shares one tint"
        );
        assert!(
            brightness(later) < brightness(past),
            "rows that have not happened yet must read dimmer than rows behind the playhead, got \
             later {later:?} against past {past:?}"
        );
        assert!(
            brightness(now) >= brightness(past),
            "the current row must never read dimmer than the rows behind it, got now {now:?} \
             against past {past:?}"
        );
    }

    fn snapshot_with_many_lines(count: i64) -> (StoreSnapshot, FieldId) {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "EVENT").unwrap();
        let field = identity.add_field(topic, "text").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "EVENT",
                [FieldSchema::new("text", DataType::Utf8, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let times: Vec<i64> = (0..count).map(|i| i * 100).collect();
        let texts: Vec<String> = (0..count).map(|i| format!("line-{i:04}")).collect();
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(times),
                vec![Arc::new(StringArray::from(texts)) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 7).unwrap();
        (snapshot, field)
    }

    #[test]
    fn the_highlighted_row_stays_visible_in_both_directions() {
        let (snapshot, field) = snapshot_with_many_lines(400);
        let mut windows = TextViewers::default();
        windows.open(&snapshot, field);
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let _ = render(&ctx, &mut windows, &snapshot, Some(0), Vec::new());
        }

        let mut visible = |row: i64| {
            let mut output = render(&ctx, &mut windows, &snapshot, Some(row * 100), Vec::new()).0;
            output = render(&ctx, &mut windows, &snapshot, Some(row * 100), Vec::new()).0;
            let wanted = format!("line-{row:04}");
            let lines: Vec<String> = painted(&output)
                .iter()
                .map(|(text, _)| text.clone())
                .filter(|text| text.starts_with("line-"))
                .collect();
            painted(&output)
                .iter()
                .any(|(text, _)| *text == wanted)
                .then_some(())
                .ok_or_else(|| {
                    format!(
                        "{wanted} not painted; showing {:?}..{:?} ({} rows)",
                        lines.first(),
                        lines.last(),
                        lines.len()
                    )
                })
        };

        visible(300).expect("jumping forward must bring the highlighted row into view");
        visible(310).expect("stepping forward must keep the highlighted row in view");
        visible(60).expect("jumping backward must bring the highlighted row into view");
        visible(59).expect("stepping backward must keep the highlighted row in view");
    }

    fn render_filtered(
        ctx: &egui::Context,
        windows: &mut TextViewers,
        snapshot: &StoreSnapshot,
        query: &str,
    ) -> egui::FullOutput {
        windows.windows[0].query = query.to_owned();
        let mut output = render(ctx, windows, snapshot, Some(250), Vec::new()).0;
        for _ in 0..24 {
            output = render(ctx, windows, snapshot, Some(250), Vec::new()).0;
        }
        output
    }

    #[test]
    fn a_search_hides_the_rows_that_do_not_match_it() {
        let (snapshot, field) = snapshot_with_text();
        let mut windows = TextViewers::default();
        windows.open(&snapshot, field);
        let ctx = egui::Context::default();

        let output = render_filtered(&ctx, &mut windows, &snapshot, "arm");
        let texts: Vec<String> = painted(&output).iter().map(|(t, _)| t.clone()).collect();

        assert!(texts.iter().any(|text| text == "ARMED"));
        assert!(texts.iter().any(|text| text == "DISARMED"));
        assert!(
            !texts
                .iter()
                .any(|text| text == "TAKEOFF" || text == "LANDING"),
            "rows that do not match must be hidden, got {texts:?}"
        );
    }

    fn selection_bands(output: &egui::FullOutput, fill: egui::Color32) -> usize {
        fn close(a: egui::Color32, b: egui::Color32) -> bool {
            a.to_array()
                .iter()
                .zip(b.to_array())
                .all(|(a, b)| a.abs_diff(b) <= 8)
        }
        fn walk(shape: &egui::epaint::Shape, fill: egui::Color32, out: &mut usize) {
            match shape {
                egui::epaint::Shape::Rect(rect) if close(rect.fill, fill) => *out += 1,
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|s| walk(s, fill, out));
                }
                _ => {}
            }
        }
        let mut out = 0;
        for clipped in &output.shapes {
            walk(&clipped.shape, fill, &mut out);
        }
        out
    }

    #[test]
    fn a_search_that_keeps_the_current_row_keeps_it_highlighted() {
        let (snapshot, field) = snapshot_with_text();
        let mut windows = TextViewers::default();
        windows.open(&snapshot, field);
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let fill = ctx.style().visuals.selection.bg_fill;

        let unfiltered = selection_bands(&render_filtered(&ctx, &mut windows, &snapshot, ""), fill);
        let output = render_filtered(&ctx, &mut windows, &snapshot, "take");
        let texts: Vec<String> = painted(&output).iter().map(|(t, _)| t.clone()).collect();

        assert_eq!(windows.windows[0].last_current, Some(1));
        assert!(unfiltered > 0, "an unfiltered current row is highlighted");
        assert!(texts.iter().any(|text| text == "TAKEOFF"));
        assert_eq!(
            selection_bands(&output, fill),
            unfiltered,
            "the surviving current row keeps its highlight band"
        );
    }

    #[test]
    fn a_search_that_hides_the_current_row_highlights_nothing() {
        let (snapshot, field) = snapshot_with_text();
        let mut windows = TextViewers::default();
        windows.open(&snapshot, field);
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let fill = ctx.style().visuals.selection.bg_fill;

        let unfiltered = selection_bands(&render_filtered(&ctx, &mut windows, &snapshot, ""), fill);
        let output = render_filtered(&ctx, &mut windows, &snapshot, "arm");
        let colors = painted_colors(&output);
        let color_of = |wanted: &str| {
            colors
                .iter()
                .find(|(text, _)| text == wanted)
                .unwrap_or_else(|| panic!("{wanted} should paint, got {colors:?}"))
                .1
        };

        assert_eq!(
            windows.windows[0].last_current,
            Some(1),
            "the viewer keeps tracking the real current message"
        );
        assert!(unfiltered > 0, "an unfiltered current row is highlighted");
        assert_eq!(
            selection_bands(&output, fill),
            0,
            "no surviving row may stand in for a current row the search hid"
        );
        assert_ne!(
            color_of("DISARMED"),
            color_of("ARMED"),
            "rows on either side of the real current message keep their own shade"
        );
    }

    #[test]
    fn the_view_follows_only_when_the_highlighted_row_moves() {
        assert!(should_follow(None, Some(3)));
        assert!(should_follow(Some(2), Some(3)));
        assert!(!should_follow(Some(3), Some(3)));
        assert!(
            !should_follow(Some(3), None),
            "losing the highlight must not yank the list"
        );
    }

    #[test]
    fn prev_takes_the_last_message_at_or_before_the_playhead() {
        assert_eq!(current_index(&TIMES, 250, SampleMode::Prev), Some(1));
        assert_eq!(current_index(&TIMES, 200, SampleMode::Prev), Some(1));
        assert_eq!(current_index(&TIMES, 400, SampleMode::Prev), Some(3));
        assert_eq!(current_index(&TIMES, 900, SampleMode::Prev), Some(3));
    }

    #[test]
    fn next_takes_the_first_message_at_or_after_the_playhead() {
        assert_eq!(current_index(&TIMES, 250, SampleMode::Next), Some(2));
        assert_eq!(current_index(&TIMES, 200, SampleMode::Next), Some(1));
        assert_eq!(current_index(&TIMES, 100, SampleMode::Next), Some(0));
        assert_eq!(current_index(&TIMES, 50, SampleMode::Next), Some(0));
    }

    #[test]
    fn a_playhead_outside_the_messages_has_no_current_row() {
        assert_eq!(current_index(&TIMES, 50, SampleMode::Prev), None);
        assert_eq!(current_index(&TIMES, 900, SampleMode::Next), None);
        assert_eq!(current_index(&[], 100, SampleMode::Prev), None);
        assert_eq!(current_index(&[], 100, SampleMode::Next), None);
    }

    #[test]
    fn linear_falls_back_to_prev_because_text_cannot_be_interpolated() {
        for playhead in [50, 100, 250, 400, 900] {
            assert_eq!(
                current_index(&TIMES, playhead, SampleMode::Linear),
                current_index(&TIMES, playhead, SampleMode::Prev)
            );
        }
    }
}

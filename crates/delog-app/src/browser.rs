//! Data browser tree: Source → Topic → Field.
//!
//! A read-only tree with dtype/count/unit chips, plus fuzzy search, natural
//! sort, highlighting, drag and context menus. The tree model is built purely
//! from a [`StoreSnapshot`] so it is testable without a GUI; [`ui`] renders it.

use delog_core::identity::{FieldId, SourceId, TopicId};
use delog_core::snapshot::StoreSnapshot;
use delog_core::time::TimeRange;

/// A flattened, render-ready view of one snapshot's live sources.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct BrowserModel {
    pub sources: Vec<SourceNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceNode {
    pub id: SourceId,
    pub label: String,
    pub rows: u64,
    /// Effective time range (source offset applied).
    pub range: Option<TimeRange>,
    /// Per-source time offset.
    pub offset_us: i64,
    pub topics: Vec<TopicNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopicNode {
    pub id: TopicId,
    pub name: String,
    pub rows: u64,
    pub fields: Vec<FieldNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldNode {
    pub id: FieldId,
    pub name: String,
    pub dtype: &'static str,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub count: u64,
}

impl BrowserModel {
    /// Build the tree from a snapshot, skipping tombstoned entities and topics
    /// that carry no data yet.
    pub fn from_snapshot(snapshot: &StoreSnapshot) -> Self {
        let mut sources = Vec::new();
        for source in snapshot.sources.iter() {
            if source.entry.removed {
                continue;
            }
            let mut topics = Vec::new();
            let mut source_rows = 0u64;
            let mut source_range: Option<TimeRange> = None;

            for &topic_id in source.topics.iter() {
                let Some(topic) = snapshot.topic(topic_id) else {
                    continue;
                };
                if topic.entry.removed {
                    continue;
                }
                let Some(store) = snapshot.topic_store(topic_id) else {
                    continue;
                };
                let rows = store.rows;
                source_rows += rows;
                if let Some(range) = store.time_range() {
                    source_range = Some(match source_range {
                        Some(r) => r.union(range),
                        None => range,
                    });
                }

                let mut fields: Vec<FieldNode> = snapshot
                    .fields
                    .iter()
                    .filter(|f| f.topic == topic_id && !f.removed)
                    .map(|f| {
                        let schema = store.schema.field_by_name(&f.name);
                        FieldNode {
                            id: f.id,
                            name: f.name.clone(),
                            dtype: schema.map(|s| s.dtype_label()).unwrap_or("?"),
                            unit: schema.and_then(|s| s.unit.clone()),
                            description: schema.and_then(|s| s.description.clone()),
                            count: rows,
                        }
                    })
                    .collect();
                fields.sort_by(|a, b| natural_cmp(&a.name, &b.name));

                topics.push(TopicNode {
                    id: topic_id,
                    name: topic.entry.name.clone(),
                    rows,
                    fields,
                });
            }
            topics.sort_by(|a, b| natural_cmp(&a.name, &b.name));

            let offset_us = source.entry.offset_us;
            sources.push(SourceNode {
                id: source.entry.id,
                label: source.entry.label.clone(),
                rows: source_rows,
                range: source_range.and_then(|r| r.offset(offset_us)),
                offset_us,
                topics,
            });
        }
        Self { sources }
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Filter the tree by a search query over full `source/topic.field` paths.
    /// A field is kept when the query matches its full path; a
    /// match at topic or source level keeps the whole branch. Empty branches
    /// are pruned; a blank query is the identity.
    pub fn filtered(&self, query: &str) -> Self {
        if query.trim().is_empty() {
            return self.clone();
        }
        let mut sources = Vec::new();
        for source in &self.sources {
            if matches_query(query, &source.label) {
                sources.push(source.clone());
                continue;
            }
            let mut topics = Vec::new();
            for topic in &source.topics {
                let topic_path = format!("{}/{}", source.label, topic.name);
                if matches_query(query, &topic_path) {
                    topics.push(topic.clone());
                    continue;
                }
                let fields: Vec<FieldNode> = topic
                    .fields
                    .iter()
                    .filter(|f| matches_query(query, &format!("{topic_path}.{}", f.name)))
                    .cloned()
                    .collect();
                if !fields.is_empty() {
                    topics.push(TopicNode {
                        fields,
                        ..topic.clone()
                    });
                }
            }
            if !topics.is_empty() {
                sources.push(SourceNode {
                    topics,
                    ..source.clone()
                });
            }
        }
        Self { sources }
    }
}

/// How a click modifies the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMod {
    /// Plain click: the field becomes the whole selection.
    Replace,
    /// Ctrl-click: toggle the field in/out.
    Toggle,
    /// Shift-click: select the visible range from the anchor to the field.
    Range,
}

/// Multi-select state for the browser tree. Pure data —
/// `visible` is the tree's current field order so ranges and payloads follow
/// what the user sees.
#[derive(Debug, Default)]
pub struct Selection {
    selected: std::collections::HashSet<FieldId>,
    anchor: Option<FieldId>,
}

impl Selection {
    pub fn click(&mut self, field: FieldId, modifier: SelectMod, visible: &[FieldId]) {
        match modifier {
            SelectMod::Replace => {
                self.selected.clear();
                self.selected.insert(field);
                self.anchor = Some(field);
            }
            SelectMod::Toggle => {
                if !self.selected.remove(&field) {
                    self.selected.insert(field);
                    self.anchor = Some(field);
                }
            }
            SelectMod::Range => {
                let anchor = self.anchor.unwrap_or(field);
                let a = visible.iter().position(|f| *f == anchor);
                let b = visible.iter().position(|f| *f == field);
                self.selected.clear();
                match (a, b) {
                    (Some(a), Some(b)) => {
                        let (lo, hi) = (a.min(b), a.max(b));
                        self.selected.extend(visible[lo..=hi].iter().copied());
                    }
                    _ => {
                        self.selected.insert(field);
                    }
                }
                self.anchor = Some(anchor);
            }
        }
    }

    /// Start a drag from `field`, updating selection the same way file
    /// browsers do: dragging an already selected field preserves a multi-field
    /// payload, while dragging an unselected field makes it the selection.
    pub fn start_drag(&mut self, field: FieldId, modifier: SelectMod, visible: &[FieldId]) {
        if modifier == SelectMod::Replace && self.selected.contains(&field) {
            return;
        }
        self.click(field, modifier, visible);
    }

    pub fn contains(&self, field: FieldId) -> bool {
        self.selected.contains(&field)
    }

    /// The selection in visible (tree) order.
    pub fn ordered(&self, visible: &[FieldId]) -> Vec<FieldId> {
        visible
            .iter()
            .copied()
            .filter(|f| self.selected.contains(f))
            .collect()
    }

    /// The `Vec<FieldId>` drag payload: the whole selection when the
    /// dragged field is part of it, otherwise just the dragged field.
    pub fn drag_payload(&self, dragged: FieldId, visible: &[FieldId]) -> Vec<FieldId> {
        if self.selected.contains(&dragged) {
            self.ordered(visible)
        } else {
            vec![dragged]
        }
    }
}

#[derive(Debug, Default)]
pub struct Expanded {
    collapsed_sources: std::collections::HashSet<SourceId>,
    expanded_topics: std::collections::HashSet<TopicId>,
}

impl Expanded {
    pub fn source_open(&self, id: SourceId) -> bool {
        !self.collapsed_sources.contains(&id)
    }

    pub fn topic_open(&self, id: TopicId) -> bool {
        self.expanded_topics.contains(&id)
    }

    pub fn toggle_source(&mut self, id: SourceId) {
        if !self.collapsed_sources.remove(&id) {
            self.collapsed_sources.insert(id);
        }
    }

    pub fn toggle_topic(&mut self, id: TopicId) {
        if !self.expanded_topics.remove(&id) {
            self.expanded_topics.insert(id);
        }
    }
}

/// One row of the flattened, virtualizable tree. Borrows the cached model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Row<'a> {
    Source(&'a SourceNode),
    /// Per-source controls (time range + offset), shown under an open source.
    SourceMeta(&'a SourceNode),
    Topic(&'a TopicNode),
    Field(&'a FieldNode),
}

/// Flatten the (already filtered) model into the visible row sequence, honoring
/// open state. While filtering, every surviving topic is forced open so matches
/// show immediately.
pub fn flatten<'a>(model: &'a BrowserModel, expanded: &Expanded, filtering: bool) -> Vec<Row<'a>> {
    let mut rows = Vec::new();
    for source in &model.sources {
        rows.push(Row::Source(source));
        if !expanded.source_open(source.id) {
            continue;
        }
        rows.push(Row::SourceMeta(source));
        for topic in &source.topics {
            rows.push(Row::Topic(topic));
            if filtering || expanded.topic_open(topic.id) {
                rows.extend(topic.fields.iter().map(Row::Field));
            }
        }
    }
    rows
}

/// Natural order: digit runs compare numerically, text runs case-insensitively
/// (`GPS[2]` before `GPS[10]`).
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut a = a.chars().peekable();
    let mut b = b.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let na = take_number(&mut a);
                    let nb = take_number(&mut b);
                    match na.cmp(&nb) {
                        Ordering::Equal => {}
                        other => return other,
                    }
                } else {
                    let (la, lb) = (ca.to_ascii_lowercase(), cb.to_ascii_lowercase());
                    match la.cmp(&lb) {
                        Ordering::Equal => {
                            a.next();
                            b.next();
                        }
                        other => return other,
                    }
                }
            }
        }
    }
}

/// Consume a digit run as a number (saturating well past any real instance id).
fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u128 {
    let mut value: u128 = 0;
    while let Some(c) = chars.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        value = value
            .saturating_mul(10)
            .saturating_add((c as u8 - b'0') as u128);
        chars.next();
    }
    value
}

/// Whitespace-separated query tokens each match the path case-insensitively
/// (`gps hacc` matches `GPS[0].HAcc`). Blank queries match everything.
pub(crate) fn matches_query(query: &str, path: &str) -> bool {
    let path = path.to_lowercase();
    query
        .split_whitespace()
        .all(|token| path.contains(&token.to_lowercase()))
}

#[derive(Debug, Default)]
pub struct BrowserResponse {
    /// Requested per-source offset change, if any.
    pub offset_change: Option<(SourceId, i64)>,
    /// The user right-clicked a source and asked to remove it.
    pub remove_source: Option<SourceId>,
    /// The user requested source metadata/params/link information.
    pub inspect_source: Option<SourceId>,
    /// The user requested global stats for a field.
    pub inspect_field_stats: Option<FieldId>,
    /// The user asked to generate markers from a discrete field's values.
    pub generate_markers: Option<FieldId>,
    /// The user asked to collapse the data browser panel.
    pub collapse_requested: bool,
}

/// A context-menu action from a field row.
enum FieldRowAction {
    InspectStats(FieldId),
    GenerateMarkers(FieldId),
}

/// Whether a field's dtype label is discrete enough to generate markers from
/// its distinct values (int/uint/bool/string; floats excluded).
fn is_discrete_dtype(label: &str) -> bool {
    matches!(
        label,
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "bool" | "str"
    )
}

fn hover_description(description: Option<&str>) -> Option<&str> {
    description.filter(|description| !description.is_empty())
}

pub fn panel_toggle_button_size(ui: &egui::Ui) -> egui::Vec2 {
    let side = ui.spacing().interact_size.y + ui.spacing().button_padding.x * 2.0;
    egui::Vec2::splat(side)
}

pub fn ui(
    ui: &mut egui::Ui,
    model: &BrowserModel,
    query: &mut String,
    selection: &mut Selection,
    expanded: &mut Expanded,
    offset_dialog: &mut Option<(SourceId, i64)>,
) -> BrowserResponse {
    let mut response = BrowserResponse::default();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let button_size = panel_toggle_button_size(ui);
        let filter_width = (ui.available_width() - button_size.x - ui.spacing().item_spacing.x)
            .max(ui.spacing().interact_size.x);
        ui.add_sized(
            egui::vec2(filter_width, button_size.y),
            egui::TextEdit::singleline(query)
                .hint_text("Filter...")
                .desired_width(filter_width),
        );
        let icon_size = button_size - ui.spacing().button_padding * 2.0;
        let icon = egui::Image::new(crate::icons::panel_left_close())
            .fit_to_exact_size(icon_size)
            .tint(ui.visuals().text_color());
        if ui
            .add_sized(button_size, egui::Button::image(icon))
            .on_hover_text("Hide data browser")
            .clicked()
        {
            response.collapse_requested = true;
        }
    });

    if model.is_empty() {
        ui.allocate_ui_with_layout(
            ui.available_size(),
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
            |ui| {
                ui.weak("No logs loaded.");
            },
        );
        return response;
    }

    let filtering = !query.trim().is_empty();
    let filtered;
    let model = if filtering {
        filtered = model.filtered(query);
        &filtered
    } else {
        model
    };
    if filtering && model.is_empty() {
        ui.add_space(8.0);
        ui.weak("Nothing matches the filter.");
        return response;
    }

    // Field order for shift-range selection and drag payloads — every field in
    // the (filtered) model, independent of collapse state.
    let visible: Vec<FieldId> = model
        .sources
        .iter()
        .flat_map(|s| s.topics.iter())
        .flat_map(|t| t.fields.iter().map(|f| f.id))
        .collect();

    // Flatten to the visible row sequence; render only the on-screen slice.
    // Uniform row height lets `show_rows` virtualize, so cost scales with the
    // viewport — not with how many sources/topics are expanded.
    let rows = flatten(model, expanded, filtering);
    let row_height = ui.spacing().interact_size.y;

    let mut offset_change = None;
    let mut remove_source = None;
    let mut inspect_source = None;
    let mut inspect_field_stats = None;
    let mut generate_markers = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, row_height, rows.len(), |ui, range| {
            ui.set_width(ui.available_width());
            for &row in &rows[range] {
                match row {
                    Row::Source(source) => {
                        match source_header_row(ui, source, expanded, row_height) {
                            Some(SourceAction::Inspect) => inspect_source = Some(source.id),
                            Some(SourceAction::Remove) => remove_source = Some(source.id),
                            None => {}
                        }
                    }
                    Row::SourceMeta(source) => {
                        if let Some(change) = source_meta_row(ui, source, offset_dialog, row_height)
                        {
                            offset_change = Some(change);
                        }
                    }
                    Row::Topic(topic) => topic_header_row(ui, topic, expanded, row_height),
                    Row::Field(field) => {
                        match field_row_band(ui, field, selection, &visible, row_height) {
                            Some(FieldRowAction::InspectStats(f)) => inspect_field_stats = Some(f),
                            Some(FieldRowAction::GenerateMarkers(f)) => generate_markers = Some(f),
                            None => {}
                        }
                    }
                }
            }
        });

    if let Some(change) = offset_dialog_window(ui, model, offset_dialog) {
        offset_change = Some(change);
    }
    response.offset_change = offset_change;
    response.remove_source = remove_source;
    response.inspect_source = inspect_source;
    response.inspect_field_stats = inspect_field_stats;
    response.generate_markers = generate_markers;
    response
}

/// A source row's context-menu choice.
enum SourceAction {
    Inspect,
    Remove,
}

/// Left indent applied per tree depth (≈ one chevron width + spacing).
fn indent_step(ui: &egui::Ui) -> f32 {
    ui.spacing().icon_width + ui.spacing().item_spacing.x
}

/// A right/down chevron drawn as a tinted SVG (no glyphs).
fn chevron(ui: &mut egui::Ui, open: bool) {
    let src = if open {
        crate::icons::chevron_down()
    } else {
        crate::icons::chevron_right()
    };
    ui.add(
        egui::Image::new(src)
            .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
            .tint(ui.visuals().text_color()),
    );
}

/// Reserve one fixed-height row band so `ScrollArea::show_rows` can virtualize,
/// laying content out left-to-right, vertically centered. Returns the band
/// rect; header rows add their click/context interaction *on top* of this rect
/// (via [`egui::Ui::interact`]) so it isn't occluded by the content widgets.
fn row_band(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    height: f32,
    add: impl FnOnce(&mut egui::Ui),
) -> egui::Rect {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id_salt)
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    add(&mut content);
    rect
}

/// Source header: chevron + label, click toggles, right-click for the source
/// context menu.
fn source_header_row(
    ui: &mut egui::Ui,
    source: &SourceNode,
    expanded: &mut Expanded,
    height: f32,
) -> Option<SourceAction> {
    let open = expanded.source_open(source.id);
    let rect = row_band(ui, ("source", source.id.0), height, |ui| {
        chevron(ui, open);
        ui.label(egui::RichText::new(format!("{}  ({} rows)", source.label, source.rows)).strong());
    });
    let response = ui.interact(
        rect,
        egui::Id::new(("source-row", source.id.0)),
        egui::Sense::click(),
    );
    if response.clicked() {
        expanded.toggle_source(source.id);
    }

    let mut action = None;
    response.context_menu(|ui| {
        let info = egui::Image::new(crate::icons::info())
            .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
            .tint(ui.visuals().text_color());
        if ui
            .add(egui::Button::image_and_text(info, "Source metadata"))
            .clicked()
        {
            action = Some(SourceAction::Inspect);
            ui.close();
        }
        let trash = egui::Image::new(crate::icons::trash())
            .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
            .tint(ui.visuals().error_fg_color);
        if ui
            .add(egui::Button::image_and_text(trash, "Remove source"))
            .clicked()
        {
            action = Some(SourceAction::Remove);
            ui.close();
        }
    });
    action
}

/// Per-source controls under an open source: time range + offset editor.
fn source_meta_row(
    ui: &mut egui::Ui,
    source: &SourceNode,
    offset_dialog: &mut Option<(SourceId, i64)>,
    height: f32,
) -> Option<(SourceId, i64)> {
    let mut change = None;
    let step = indent_step(ui);
    row_band(ui, ("meta", source.id.0), height, |ui| {
        ui.add_space(step);
        if let Some(range) = source.range {
            ui.weak(format!(
                "{:.3}–{:.3} s",
                range.min_us as f64 / 1e6,
                range.max_us as f64 / 1e6
            ));
        }
        if let Some(c) = offset_widget(ui, source, offset_dialog) {
            change = Some(c);
        }
    });
    change
}

/// Topic header: indented chevron + label, click toggles.
fn topic_header_row(ui: &mut egui::Ui, topic: &TopicNode, expanded: &mut Expanded, height: f32) {
    let open = expanded.topic_open(topic.id);
    let step = indent_step(ui);
    let rect = row_band(ui, ("topic", topic.id.0), height, |ui| {
        ui.add_space(step);
        chevron(ui, open);
        ui.label(format!("{}  ({})", topic.name, topic.rows));
    });
    let response = ui.interact(
        rect,
        egui::Id::new(("topic-row", topic.id.0)),
        egui::Sense::click(),
    );
    if response.clicked() {
        expanded.toggle_topic(topic.id);
    }
}

/// A field row inside a fixed-height band, indented under its topic.
fn field_row_band(
    ui: &mut egui::Ui,
    field: &FieldNode,
    selection: &mut Selection,
    visible: &[FieldId],
    height: f32,
) -> Option<FieldRowAction> {
    let mut action = None;
    let step = indent_step(ui);
    row_band(ui, ("fieldrow", field.id.0), height, |ui| {
        ui.add_space(step * 2.0);
        action = field_row(ui, field, selection, visible);
    });
    action
}

/// Inline drag-us offset on the source row: dragging
/// shifts the source in ~1 ms steps; the clock button opens the exact-us dialog.
fn offset_widget(
    ui: &mut egui::Ui,
    source: &SourceNode,
    offset_dialog: &mut Option<(SourceId, i64)>,
) -> Option<(SourceId, i64)> {
    let mut change = None;
    ui.weak("offset");
    let mut secs = source.offset_us as f64 * 1e-6;
    let response = ui.add(
        egui::DragValue::new(&mut secs)
            .speed(0.001)
            .fixed_decimals(3)
            .suffix(" s"),
    );
    if response.changed() {
        change = Some((source.id, (secs * 1e6).round() as i64));
    }
    let clock = egui::Image::new(crate::icons::clock())
        .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
        .tint(ui.visuals().text_color());
    if ui
        .add(egui::Button::image(clock))
        .on_hover_text("Set exact offset (us)")
        .clicked()
    {
        *offset_dialog = Some((source.id, source.offset_us));
    }
    change
}

/// Exact-µs offset dialog. The draft lives in app state; Apply emits
/// the change and the window's close button discards it.
fn offset_dialog_window(
    ui: &egui::Ui,
    model: &BrowserModel,
    offset_dialog: &mut Option<(SourceId, i64)>,
) -> Option<(SourceId, i64)> {
    let (source_id, mut draft_us) = (*offset_dialog)?;
    let label = model
        .sources
        .iter()
        .find(|s| s.id == source_id)
        .map_or("(removed source)", |s| s.label.as_str());

    let mut change = None;
    let mut open = true;
    egui::Window::new(format!("Time offset — {label}"))
        .id(egui::Id::new(("source_offset", source_id.0)))
        .open(&mut open)
        .collapsible(false)
        .default_pos(ui.ctx().content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Offset");
                ui.add(egui::DragValue::new(&mut draft_us).speed(100).suffix(" µs"));
            });
            ui.weak(format!("= {:.6} s", draft_us as f64 * 1e-6));
            if ui.button("Apply").clicked() {
                change = Some((source_id, draft_us));
            }
        });

    if change.is_some() || !open {
        *offset_dialog = None;
    } else {
        *offset_dialog = Some((source_id, draft_us));
    }
    change
}

fn field_row(
    ui: &mut egui::Ui,
    field: &FieldNode,
    selection: &mut Selection,
    visible: &[FieldId],
) -> Option<FieldRowAction> {
    let mut action = None;
    // The row is a drag source carrying `Vec<FieldId>` — the multi-selection
    // when the dragged row is part of it; plot panes and tile
    // edges are the drop zones.
    let id = egui::Id::new(("field", field.id.0));
    let dragging_this_field = ui.ctx().is_being_dragged(id);
    if dragging_this_field {
        selection.start_drag(field.id, current_select_modifier(ui), visible);
    }
    let selected = selection.contains(field.id);

    // The drag payload is only consumed by the row actually being dragged, so
    // compute it lazily — otherwise every visible row allocates a `Vec` (and
    // scans the selection) every frame for nothing.
    let response = drag_source_with_click(
        ui,
        id,
        || selection.drag_payload(field.id, visible),
        |ui| {
            let fill = if selected {
                ui.visuals().selection.bg_fill
            } else {
                egui::Color32::TRANSPARENT
            };
            egui::Frame::new()
                .fill(fill)
                .inner_margin(egui::Margin::symmetric(4, 1))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let name_color = if selected {
                            ui.visuals().selection.stroke.color
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.label(egui::RichText::new(&field.name).color(name_color));
                        ui.weak(field.dtype);
                        if let Some(unit) = &field.unit {
                            ui.weak(format!("[{unit}]"));
                        }
                    });
                });
        },
    );
    let response = if let Some(description) = hover_description(field.description.as_deref()) {
        response.on_hover_text(description)
    } else {
        response
    };

    if response.clicked() || response.drag_started() {
        if response.drag_started() {
            selection.start_drag(field.id, current_select_modifier(ui), visible);
        } else {
            selection.click(field.id, current_select_modifier(ui), visible);
        }
    }
    response.context_menu(|ui| {
        let info = egui::Image::new(crate::icons::info())
            .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
            .tint(ui.visuals().text_color());
        if ui
            .add(egui::Button::image_and_text(info, "Field stats"))
            .clicked()
        {
            action = Some(FieldRowAction::InspectStats(field.id));
            ui.close();
        }
        if is_discrete_dtype(field.dtype) {
            let ruler = egui::Image::new(crate::icons::ruler())
                .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
                .tint(ui.visuals().text_color());
            if ui
                .add(egui::Button::image_and_text(ruler, "Generate markers"))
                .clicked()
            {
                action = Some(FieldRowAction::GenerateMarkers(field.id));
                ui.close();
            }
        }
    });
    action
}

fn current_select_modifier(ui: &egui::Ui) -> SelectMod {
    let modifiers = ui.input(|i| i.modifiers);
    if modifiers.shift {
        SelectMod::Range
    } else if modifiers.command {
        SelectMod::Toggle
    } else {
        SelectMod::Replace
    }
}

/// `Ui::dnd_drag_source`, but the overlay senses clicks as well as drags so
/// the whole row both selects (release without movement) and drags. egui's
/// built-in drag source senses drag only, which fights any clickable widget
/// rendered inside it.
fn drag_source_with_click<Payload: std::any::Any + Send + Sync>(
    ui: &mut egui::Ui,
    id: egui::Id,
    make_payload: impl FnOnce() -> Payload,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    if ui.ctx().is_being_dragged(id) {
        egui::DragAndDrop::set_payload(ui.ctx(), make_payload());

        // Paint the row to a floating layer that follows the pointer.
        let layer_id = egui::LayerId::new(egui::Order::Tooltip, id);
        let response = ui
            .scope_builder(egui::UiBuilder::new().layer_id(layer_id), add_contents)
            .response;
        if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
            let delta = pointer_pos - response.rect.center();
            ui.ctx().transform_layer_shapes(
                layer_id,
                egui::emath::TSTransform::from_translation(delta),
            );
        }
        response
    } else {
        let response = ui.scope(add_contents).response;
        ui.interact(response.rect, id, egui::Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::Grab)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Float64Array, Int32Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    use super::*;

    fn snapshot() -> StoreSnapshot {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight_21");
        identity.set_source_offset_us(source, -250);
        let gps = identity.add_topic(source, "GPS").unwrap();
        identity.add_field(gps, "Lat").unwrap();
        identity.add_field(gps, "Alt").unwrap();

        let schema = Arc::new(
            TopicSchema::new(
                "GPS",
                [
                    FieldSchema::new("Lat", DataType::Int32, Some("deg"), 1e-7)
                        .unwrap()
                        .with_description("latitude"),
                    FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Int32Array::from(vec![1, 2, 3])),
            Arc::new(Float64Array::from(vec![10.0, 11.0, 12.0])),
        ];
        let chunk =
            Arc::new(Chunk::try_new(Int64Array::from(vec![100, 200, 300]), cols, &schema).unwrap());
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());

        StoreSnapshot::from_registry(&identity, [(gps, store)], 0).unwrap()
    }

    #[test]
    fn model_mirrors_the_snapshot_tree() {
        let model = BrowserModel::from_snapshot(&snapshot());

        assert_eq!(model.sources.len(), 1);
        let src = &model.sources[0];
        assert_eq!(src.label, "flight_21");
        assert_eq!(src.rows, 3);
        // Effective range: raw 100..300 shifted by the -250 µs source offset.
        assert_eq!(src.offset_us, -250);
        assert_eq!(src.range, TimeRange::new(-150, 50));

        assert_eq!(src.topics.len(), 1);
        let gps = &src.topics[0];
        assert_eq!(gps.name, "GPS");
        assert_eq!(gps.rows, 3);

        // Fields sort naturally: Alt before Lat.
        assert_eq!(gps.fields.len(), 2);
        assert_eq!(gps.fields[0].name, "Alt");
        assert_eq!(gps.fields[0].dtype, "f64");
        assert_eq!(gps.fields[1].name, "Lat");
        assert_eq!(gps.fields[1].dtype, "i32");
        assert_eq!(gps.fields[1].unit.as_deref(), Some("deg"));
        assert_eq!(gps.fields[1].description.as_deref(), Some("latitude"));
        assert_eq!(gps.fields[1].count, 3);
    }

    #[test]
    fn empty_snapshot_yields_an_empty_model() {
        assert!(BrowserModel::from_snapshot(&StoreSnapshot::empty()).is_empty());
    }

    #[test]
    fn plain_click_replaces_selection_and_sets_the_anchor() {
        let visible = [FieldId(1), FieldId(2), FieldId(3), FieldId(4)];
        let mut sel = Selection::default();
        sel.click(FieldId(2), SelectMod::Replace, &visible);
        assert_eq!(sel.ordered(&visible), vec![FieldId(2)]);
        sel.click(FieldId(4), SelectMod::Replace, &visible);
        assert_eq!(sel.ordered(&visible), vec![FieldId(4)]);
    }

    #[test]
    fn ctrl_click_toggles_membership() {
        let visible = [FieldId(1), FieldId(2), FieldId(3)];
        let mut sel = Selection::default();
        sel.click(FieldId(1), SelectMod::Toggle, &visible);
        sel.click(FieldId(3), SelectMod::Toggle, &visible);
        assert_eq!(sel.ordered(&visible), vec![FieldId(1), FieldId(3)]);
        sel.click(FieldId(1), SelectMod::Toggle, &visible);
        assert_eq!(sel.ordered(&visible), vec![FieldId(3)]);
    }

    #[test]
    fn shift_click_selects_the_range_from_the_anchor() {
        let visible = [FieldId(1), FieldId(2), FieldId(3), FieldId(4), FieldId(5)];
        let mut sel = Selection::default();
        sel.click(FieldId(2), SelectMod::Replace, &visible);
        sel.click(FieldId(4), SelectMod::Range, &visible);
        assert_eq!(
            sel.ordered(&visible),
            vec![FieldId(2), FieldId(3), FieldId(4)]
        );
        // Range works upward from the anchor too.
        sel.click(FieldId(1), SelectMod::Range, &visible);
        assert_eq!(sel.ordered(&visible), vec![FieldId(1), FieldId(2)]);
    }

    #[test]
    fn drag_payload_is_the_selection_when_dragging_a_selected_field() {
        let visible = [FieldId(1), FieldId(2), FieldId(3)];
        let mut sel = Selection::default();
        sel.click(FieldId(1), SelectMod::Toggle, &visible);
        sel.click(FieldId(3), SelectMod::Toggle, &visible);
        // Dragging a selected field carries the whole selection.
        assert_eq!(
            sel.drag_payload(FieldId(3), &visible),
            vec![FieldId(1), FieldId(3)]
        );
        // Dragging an unselected field carries just that field.
        assert_eq!(sel.drag_payload(FieldId(2), &visible), vec![FieldId(2)]);
    }

    #[test]
    fn starting_plain_drag_on_unselected_field_replaces_selection() {
        let visible = [FieldId(1), FieldId(2), FieldId(3)];
        let mut sel = Selection::default();
        sel.click(FieldId(1), SelectMod::Replace, &visible);

        sel.start_drag(FieldId(2), SelectMod::Replace, &visible);

        assert_eq!(sel.ordered(&visible), vec![FieldId(2)]);
        assert_eq!(sel.drag_payload(FieldId(2), &visible), vec![FieldId(2)]);
    }

    #[test]
    fn starting_plain_drag_on_selected_field_preserves_multi_selection() {
        let visible = [FieldId(1), FieldId(2), FieldId(3)];
        let mut sel = Selection::default();
        sel.click(FieldId(1), SelectMod::Toggle, &visible);
        sel.click(FieldId(3), SelectMod::Toggle, &visible);

        sel.start_drag(FieldId(3), SelectMod::Replace, &visible);

        assert_eq!(sel.ordered(&visible), vec![FieldId(1), FieldId(3)]);
        assert_eq!(
            sel.drag_payload(FieldId(3), &visible),
            vec![FieldId(1), FieldId(3)]
        );
    }

    #[test]
    fn natural_cmp_orders_embedded_numbers_numerically() {
        use std::cmp::Ordering;
        // GPS[2] before GPS[10].
        assert_eq!(natural_cmp("GPS[2]", "GPS[10]"), Ordering::Less);
        assert_eq!(natural_cmp("GPS[10]", "GPS[2]"), Ordering::Greater);
        assert_eq!(natural_cmp("GPS[2]", "GPS[2]"), Ordering::Equal);
        // Case-insensitive text runs.
        assert_eq!(natural_cmp("baro", "GPS"), Ordering::Less);
        // Plain text still sorts lexically.
        assert_eq!(natural_cmp("AccX", "AccY"), Ordering::Less);
        // Numbers with different digit counts.
        assert_eq!(natural_cmp("M9", "M10"), Ordering::Less);
    }

    #[test]
    fn model_topics_and_fields_sort_naturally() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        // Insert out of order: GPS[10] registered before GPS[2].
        let gps10 = identity.add_topic(source, "GPS[10]").unwrap();
        let gps2 = identity.add_topic(source, "GPS[2]").unwrap();
        identity.add_field(gps10, "Y2").unwrap();
        identity.add_field(gps10, "Y10").unwrap();
        identity.add_field(gps2, "A").unwrap();

        let schema10 = Arc::new(
            TopicSchema::new(
                "GPS[10]",
                [
                    FieldSchema::new("Y2", DataType::Float64, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("Y10", DataType::Float64, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let schema2 = Arc::new(
            TopicSchema::new(
                "GPS[2]",
                [FieldSchema::new("A", DataType::Float64, None::<String>, 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk10 = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![0]),
                vec![
                    Arc::new(Float64Array::from(vec![1.0])) as ArrayRef,
                    Arc::new(Float64Array::from(vec![2.0])) as ArrayRef,
                ],
                &schema10,
            )
            .unwrap(),
        );
        let chunk2 = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![0]),
                vec![Arc::new(Float64Array::from(vec![1.0])) as ArrayRef],
                &schema2,
            )
            .unwrap(),
        );
        let store10 = Arc::new(TopicStore::from_chunks(schema10, [chunk10]).unwrap());
        let store2 = Arc::new(TopicStore::from_chunks(schema2, [chunk2]).unwrap());
        let snapshot =
            StoreSnapshot::from_registry(&identity, [(gps10, store10), (gps2, store2)], 0).unwrap();

        let model = BrowserModel::from_snapshot(&snapshot);
        let topics: Vec<_> = model.sources[0]
            .topics
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(topics, vec!["GPS[2]", "GPS[10]"]);
        let fields: Vec<_> = model.sources[0].topics[1]
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(fields, vec!["Y2", "Y10"]);
    }

    #[test]
    fn query_tokens_match_full_paths_case_insensitively() {
        // "gps hacc" matches GPS[0].HAcc.
        assert!(matches_query("gps hacc", "flight_21/GPS[0].HAcc"));
        assert!(matches_query("GPS", "flight_21/GPS[0].HAcc"));
        assert!(matches_query("flight hacc", "flight_21/GPS[0].HAcc"));
        assert!(!matches_query("baro", "flight_21/GPS[0].HAcc"));
        // Every token must match somewhere in the path.
        assert!(!matches_query("gps baro", "flight_21/GPS[0].HAcc"));
        // Blank queries match everything.
        assert!(matches_query("", "anything"));
        assert!(matches_query("   ", "anything"));
    }

    #[test]
    fn hover_description_rejects_empty_text() {
        assert_eq!(hover_description(Some("latitude")), Some("latitude"));
        assert_eq!(hover_description(Some("")), None);
        assert_eq!(hover_description(None), None);
    }

    #[test]
    fn filtered_model_retains_matching_fields_and_prunes_empty_branches() {
        let model = BrowserModel::from_snapshot(&snapshot());

        let lat = model.filtered("gps lat");
        assert_eq!(lat.sources.len(), 1);
        assert_eq!(lat.sources[0].topics.len(), 1);
        let fields: Vec<_> = lat.sources[0].topics[0]
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(fields, vec!["Lat"]);

        // A topic-level match keeps all its fields.
        let gps = model.filtered("gps");
        assert_eq!(gps.sources[0].topics[0].fields.len(), 2);

        // No match prunes everything.
        assert!(model.filtered("nonexistent").is_empty());

        // Blank query is the identity.
        assert_eq!(model.filtered(""), model);
    }

    /// Render the flattened rows as `S:`/`M:`/`T:`/`F:` labels for assertion.
    fn labels(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Source(s) => format!("S:{}", s.label),
                Row::SourceMeta(s) => format!("M:{}", s.label),
                Row::Topic(t) => format!("T:{}", t.name),
                Row::Field(f) => format!("F:{}", f.name),
            })
            .collect()
    }

    #[test]
    fn expanded_defaults_open_sources_and_closed_topics() {
        let expanded = Expanded::default();
        assert!(expanded.source_open(SourceId(1)));
        assert!(!expanded.topic_open(TopicId(1)));
    }

    #[test]
    fn toggling_a_source_closes_then_reopens_it() {
        let mut expanded = Expanded::default();
        expanded.toggle_source(SourceId(7));
        assert!(!expanded.source_open(SourceId(7)));
        expanded.toggle_source(SourceId(7));
        assert!(expanded.source_open(SourceId(7)));
    }

    #[test]
    fn toggling_a_topic_opens_then_closes_it() {
        let mut expanded = Expanded::default();
        expanded.toggle_topic(TopicId(3));
        assert!(expanded.topic_open(TopicId(3)));
        expanded.toggle_topic(TopicId(3));
        assert!(!expanded.topic_open(TopicId(3)));
    }

    #[test]
    fn flatten_default_shows_source_meta_and_topics_but_not_fields() {
        let model = BrowserModel::from_snapshot(&snapshot());
        let rows = flatten(&model, &Expanded::default(), false);
        assert_eq!(labels(&rows), vec!["S:flight_21", "M:flight_21", "T:GPS"]);
    }

    #[test]
    fn flatten_reveals_fields_under_an_expanded_topic() {
        let model = BrowserModel::from_snapshot(&snapshot());
        let topic_id = model.sources[0].topics[0].id;
        let mut expanded = Expanded::default();
        expanded.toggle_topic(topic_id);
        let rows = flatten(&model, &expanded, false);
        // Fields sort naturally: Alt before Lat.
        assert_eq!(
            labels(&rows),
            vec!["S:flight_21", "M:flight_21", "T:GPS", "F:Alt", "F:Lat"]
        );
    }

    #[test]
    fn flatten_hides_topics_and_meta_under_a_collapsed_source() {
        let model = BrowserModel::from_snapshot(&snapshot());
        let source_id = model.sources[0].id;
        let mut expanded = Expanded::default();
        expanded.toggle_source(source_id);
        let rows = flatten(&model, &expanded, false);
        assert_eq!(labels(&rows), vec!["S:flight_21"]);
    }

    #[test]
    fn flatten_while_filtering_forces_every_topic_open() {
        let model = BrowserModel::from_snapshot(&snapshot());
        // No topic toggled, but filtering forces fields visible.
        let rows = flatten(&model, &Expanded::default(), true);
        assert_eq!(
            labels(&rows),
            vec!["S:flight_21", "M:flight_21", "T:GPS", "F:Alt", "F:Lat"]
        );
    }
}

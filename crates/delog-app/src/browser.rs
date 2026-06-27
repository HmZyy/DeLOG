//! Data browser tree: Source → Topic → Field.
//!
//! The tree model is built purely from a [`StoreSnapshot`] so it is testable
//! without a GUI; [`ui`] renders it.

use delog_core::identity::{FieldId, SourceId, TopicId};
use delog_core::snapshot::StoreSnapshot;
use delog_core::time::TimeRange;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct BrowserModel {
    pub sources: Vec<SourceNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceNode {
    pub id: SourceId,
    pub label: String,
    pub rows: u64,
    /// Source offset already applied.
    pub range: Option<TimeRange>,
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

    /// Filter over full `source/topic.field` paths: a match at topic or source
    /// level keeps the whole branch, empty branches are pruned, blank is identity.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMod {
    Replace,
    Toggle,
    Range,
}

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

    /// Dragging an already-selected field preserves the multi-field payload;
    /// dragging an unselected field makes it the selection.
    pub fn start_drag(&mut self, field: FieldId, modifier: SelectMod, visible: &[FieldId]) {
        if modifier == SelectMod::Replace && self.selected.contains(&field) {
            return;
        }
        self.click(field, modifier, visible);
    }

    pub fn contains(&self, field: FieldId) -> bool {
        self.selected.contains(&field)
    }

    pub fn ordered(&self, visible: &[FieldId]) -> Vec<FieldId> {
        visible
            .iter()
            .copied()
            .filter(|f| self.selected.contains(f))
            .collect()
    }

    pub fn drag_payload(&self, dragged: FieldId, visible: &[FieldId]) -> Vec<FieldId> {
        if self.selected.contains(&dragged) {
            self.ordered(visible)
        } else {
            vec![dragged]
        }
    }
}

/// Digit runs compare numerically, text runs case-insensitively
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

/// Every whitespace-separated token must match the path case-insensitively;
/// a blank query matches everything.
pub(crate) fn matches_query(query: &str, path: &str) -> bool {
    let path = path.to_lowercase();
    query
        .split_whitespace()
        .all(|token| path.contains(&token.to_lowercase()))
}

#[derive(Debug, Default)]
pub struct BrowserResponse {
    pub offset_change: Option<(SourceId, i64)>,
    pub remove_source: Option<SourceId>,
    pub inspect_source: Option<SourceId>,
    pub inspect_field_metadata: Option<FieldId>,
    pub inspect_field_stats: Option<FieldId>,
    pub generate_markers: Option<FieldId>,
    pub collapse_requested: bool,
}

enum FieldRowAction {
    InspectMetadata(FieldId),
    InspectStats(FieldId),
    GenerateMarkers(FieldId),
}

/// Discrete dtypes markers can be generated from (floats excluded).
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

    let visible: Vec<FieldId> = model
        .sources
        .iter()
        .flat_map(|s| s.topics.iter())
        .flat_map(|t| t.fields.iter().map(|f| f.id))
        .collect();

    let mut offset_change = None;
    let mut remove_source = None;
    let mut inspect_source = None;
    let mut inspect_field_metadata = None;
    let mut inspect_field_stats = None;
    let mut generate_markers = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for source in &model.sources {
                let header = format!("{}  ({} rows)", source.label, source.rows);
                let collapsing = egui::CollapsingHeader::new(header)
                    .id_salt(("source", source.id.0))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(range) = source.range {
                                ui.weak(format!(
                                    "{:.3}–{:.3} s",
                                    range.min_us as f64 / 1e6,
                                    range.max_us as f64 / 1e6
                                ));
                            }
                            if let Some(change) = offset_widget(ui, source, offset_dialog) {
                                offset_change = Some(change);
                            }
                        });
                        for topic in &source.topics {
                            egui::CollapsingHeader::new(format!(
                                "{}  ({})",
                                topic.name, topic.rows
                            ))
                            .id_salt(("topic", topic.id.0))
                            .default_open(false)
                            .open(filtering.then_some(true))
                            .show(ui, |ui| {
                                for field in &topic.fields {
                                    match field_row(ui, field, selection, &visible) {
                                        Some(FieldRowAction::InspectMetadata(f)) => {
                                            inspect_field_metadata = Some(f);
                                        }
                                        Some(FieldRowAction::InspectStats(f)) => {
                                            inspect_field_stats = Some(f);
                                        }
                                        Some(FieldRowAction::GenerateMarkers(f)) => {
                                            generate_markers = Some(f);
                                        }
                                        None => {}
                                    }
                                }
                            });
                        }
                    });
                collapsing.header_response.context_menu(|ui| {
                    let info = egui::Image::new(crate::icons::info())
                        .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
                        .tint(ui.visuals().text_color());
                    if ui
                        .add(egui::Button::image_and_text(info, "Source metadata"))
                        .clicked()
                    {
                        inspect_source = Some(source.id);
                        ui.close();
                    }
                    let trash = egui::Image::new(crate::icons::trash())
                        .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
                        .tint(ui.visuals().error_fg_color);
                    if ui
                        .add(egui::Button::image_and_text(trash, "Remove source"))
                        .clicked()
                    {
                        remove_source = Some(source.id);
                        ui.close();
                    }
                });
            }
        });

    if let Some(change) = offset_dialog_window(ui, model, offset_dialog) {
        offset_change = Some(change);
    }
    response.offset_change = offset_change;
    response.remove_source = remove_source;
    response.inspect_source = inspect_source;
    response.inspect_field_metadata = inspect_field_metadata;
    response.inspect_field_stats = inspect_field_stats;
    response.generate_markers = generate_markers;
    response
}

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
    let id = egui::Id::new(("field", field.id.0));
    let dragging_this_field = ui.ctx().is_being_dragged(id);
    if dragging_this_field {
        selection.start_drag(field.id, current_select_modifier(ui), visible);
    }
    let payload = selection.drag_payload(field.id, visible);
    let selected = selection.contains(field.id);

    let response = drag_source_with_click(ui, id, payload, |ui| {
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
    });
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
        let metadata_info = egui::Image::new(crate::icons::info())
            .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
            .tint(ui.visuals().text_color());
        if ui
            .add(egui::Button::image_and_text(
                metadata_info,
                "Field metadata",
            ))
            .clicked()
        {
            action = Some(FieldRowAction::InspectMetadata(field.id));
            ui.close();
        }
        let stats_info = egui::Image::new(crate::icons::info())
            .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
            .tint(ui.visuals().text_color());
        if ui
            .add(egui::Button::image_and_text(stats_info, "Field stats"))
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

/// Like `Ui::dnd_drag_source`, but senses clicks too: egui's built-in drag
/// source senses drag only, which fights any clickable widget inside it.
fn drag_source_with_click<Payload: std::any::Any + Send + Sync>(
    ui: &mut egui::Ui,
    id: egui::Id,
    payload: Payload,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    if ui.ctx().is_being_dragged(id) {
        egui::DragAndDrop::set_payload(ui.ctx(), payload);

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
        assert_eq!(src.offset_us, -250);
        assert_eq!(src.range, TimeRange::new(-150, 50));

        assert_eq!(src.topics.len(), 1);
        let gps = &src.topics[0];
        assert_eq!(gps.name, "GPS");
        assert_eq!(gps.rows, 3);

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
        sel.click(FieldId(1), SelectMod::Range, &visible);
        assert_eq!(sel.ordered(&visible), vec![FieldId(1), FieldId(2)]);
    }

    #[test]
    fn drag_payload_is_the_selection_when_dragging_a_selected_field() {
        let visible = [FieldId(1), FieldId(2), FieldId(3)];
        let mut sel = Selection::default();
        sel.click(FieldId(1), SelectMod::Toggle, &visible);
        sel.click(FieldId(3), SelectMod::Toggle, &visible);
        assert_eq!(
            sel.drag_payload(FieldId(3), &visible),
            vec![FieldId(1), FieldId(3)]
        );
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
        assert_eq!(natural_cmp("GPS[2]", "GPS[10]"), Ordering::Less);
        assert_eq!(natural_cmp("GPS[10]", "GPS[2]"), Ordering::Greater);
        assert_eq!(natural_cmp("GPS[2]", "GPS[2]"), Ordering::Equal);
        assert_eq!(natural_cmp("baro", "GPS"), Ordering::Less);
        assert_eq!(natural_cmp("AccX", "AccY"), Ordering::Less);
        assert_eq!(natural_cmp("M9", "M10"), Ordering::Less);
    }

    #[test]
    fn model_topics_and_fields_sort_naturally() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
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
        assert!(matches_query("gps hacc", "flight_21/GPS[0].HAcc"));
        assert!(matches_query("GPS", "flight_21/GPS[0].HAcc"));
        assert!(matches_query("flight hacc", "flight_21/GPS[0].HAcc"));
        assert!(!matches_query("baro", "flight_21/GPS[0].HAcc"));
        assert!(!matches_query("gps baro", "flight_21/GPS[0].HAcc"));
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

        let gps = model.filtered("gps");
        assert_eq!(gps.sources[0].topics[0].fields.len(), 2);

        assert!(model.filtered("nonexistent").is_empty());

        assert_eq!(model.filtered(""), model);
    }
}

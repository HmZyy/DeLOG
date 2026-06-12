//! Data browser tree: Source → Topic → Field (PLAN.md §13, BRW-01).
//!
//! BRW-01 is the read-only tree with dtype/count/unit chips; fuzzy search,
//! natural sort, highlighting, drag and context menus are later BRW items. The
//! tree model is built purely from a [`StoreSnapshot`] so it is testable without
//! a GUI; [`ui`] renders it.

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
    /// Effective time range (source offset applied, §4.2).
    pub range: Option<TimeRange>,
    /// Per-source time offset (BRW-07).
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

    /// Filter the tree by a search query over full `source/topic.field` paths
    /// (§13, BRW-02). A field is kept when the query matches its full path; a
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

/// How a click modifies the selection (BRW-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMod {
    /// Plain click: the field becomes the whole selection.
    Replace,
    /// Ctrl-click: toggle the field in/out.
    Toggle,
    /// Shift-click: select the visible range from the anchor to the field.
    Range,
}

/// Multi-select state for the browser tree (§13/§10.7, BRW-05). Pure data —
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

    /// The `Vec<FieldId>` drag payload (§10.7): the whole selection when the
    /// dragged field is part of it, otherwise just the dragged field.
    pub fn drag_payload(&self, dragged: FieldId, visible: &[FieldId]) -> Vec<FieldId> {
        if self.selected.contains(&dragged) {
            self.ordered(visible)
        } else {
            vec![dragged]
        }
    }
}

/// Natural order: digit runs compare numerically, text runs case-insensitively
/// (`GPS[2]` before `GPS[10]`, §13 BRW-03).
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
/// (`gps hacc` matches `GPS[0].HAcc`, §13). Blank queries match everything.
fn matches_query(query: &str, path: &str) -> bool {
    let path = path.to_lowercase();
    query
        .split_whitespace()
        .all(|token| path.contains(&token.to_lowercase()))
}

/// Render the browser tree with its search box (BRW-01/02). `query`,
/// `selection` and the offset dialog draft persist in app state across
/// frames. Returns a requested per-source offset change, if any (BRW-07).
pub fn ui(
    ui: &mut egui::Ui,
    model: &BrowserModel,
    query: &mut String,
    selection: &mut Selection,
    offset_dialog: &mut Option<(SourceId, i64)>,
) -> Option<(SourceId, i64)> {
    if model.is_empty() {
        ui.add_space(8.0);
        ui.weak("No logs loaded.");
        return None;
    }

    // Fuzzy filter over full paths (§13, BRW-02).
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(query)
                .hint_text("Filter...")
                .desired_width(f32::INFINITY),
        );
    });
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
        return None;
    }

    // Visible field order, for shift-range selection and drag payloads.
    let visible: Vec<FieldId> = model
        .sources
        .iter()
        .flat_map(|s| s.topics.iter())
        .flat_map(|t| t.fields.iter().map(|f| f.id))
        .collect();

    let mut offset_change = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for source in &model.sources {
                let header = format!("{}  ({} rows)", source.label, source.rows);
                egui::CollapsingHeader::new(header)
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
                            // While filtering, surviving topics open so the
                            // matched fields are visible immediately.
                            egui::CollapsingHeader::new(format!(
                                "{}  ({})",
                                topic.name, topic.rows
                            ))
                            .id_salt(("topic", topic.id.0))
                            .default_open(false)
                            .open(filtering.then_some(true))
                            .show(ui, |ui| {
                                for field in &topic.fields {
                                    field_row(ui, field, selection, &visible);
                                }
                            });
                        }
                    });
            }
        });

    if let Some(change) = offset_dialog_window(ui, model, offset_dialog) {
        offset_change = Some(change);
    }
    offset_change
}

/// Inline drag-µs offset on the source row (§13/§4.2, BRW-07): dragging
/// shifts the source in ~1 ms steps; the ⏱ button opens the exact-µs dialog.
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
    if ui
        .small_button("⏱")
        .on_hover_text("Set exact offset (µs)…")
        .clicked()
    {
        *offset_dialog = Some((source.id, source.offset_us));
    }
    change
}

/// Exact-µs offset dialog (BRW-07). The draft lives in app state; Apply emits
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

fn field_row(ui: &mut egui::Ui, field: &FieldNode, selection: &mut Selection, visible: &[FieldId]) {
    // The row is a drag source carrying `Vec<FieldId>` — the multi-selection
    // when the dragged row is part of it (§10.7, BRW-05); plot panes and tile
    // edges are the drop zones (PLT-13).
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

    if response.clicked() || response.drag_started() {
        if response.drag_started() {
            selection.start_drag(field.id, current_select_modifier(ui), visible);
        } else {
            selection.click(field.id, current_select_modifier(ui), visible);
        }
    }
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
    payload: Payload,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    if ui.ctx().is_being_dragged(id) {
        egui::DragAndDrop::set_payload(ui.ctx(), payload);

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
                    FieldSchema::new("Lat", DataType::Int32, Some("deg"), 1e-7).unwrap(),
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

        // Fields sort naturally (BRW-03): Alt before Lat.
        assert_eq!(gps.fields.len(), 2);
        assert_eq!(gps.fields[0].name, "Alt");
        assert_eq!(gps.fields[0].dtype, "f64");
        assert_eq!(gps.fields[1].name, "Lat");
        assert_eq!(gps.fields[1].dtype, "i32");
        assert_eq!(gps.fields[1].unit.as_deref(), Some("deg"));
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
        // Dragging a selected field carries the whole selection (§10.7).
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
        // The §13 example: GPS[2] before GPS[10].
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
        // The §13 example: "gps hacc" matches GPS[0].HAcc.
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
}

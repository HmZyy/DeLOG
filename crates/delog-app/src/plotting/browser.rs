//! Data browser tree: Source → Topic → Field.
//!
//! The tree model is built purely from a [`StoreSnapshot`] so it is testable
//! without a GUI; [`ui`] renders it.

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array,
    Int64Array, LargeStringArray, StringArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::DataType;
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
    search_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopicNode {
    pub id: TopicId,
    pub name: String,
    pub rows: u64,
    pub fields: Vec<FieldNode>,
    search_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldNode {
    pub id: FieldId,
    pub name: String,
    pub dtype: &'static str,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub count: u64,
    pub first_raw: Option<String>,
    pub last_raw: Option<String>,
    search_path: String,
}

const fn default_openness(node: BrowserNode) -> Option<bool> {
    match node {
        BrowserNode::Source(_) => Some(true),
        BrowserNode::Topic(_) => Some(false),
        BrowserNode::SourceMeta(_) | BrowserNode::TopicHeader(_) | BrowserNode::Field(_) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BrowserNode {
    Source(u32),
    SourceMeta(u32),
    Topic(u32),
    TopicHeader(u32),
    Field(u32),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BrowserFilter {
    pub sources: Vec<VisibleSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleSource {
    pub source: usize,
    pub topics: Vec<VisibleTopic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleTopic {
    pub topic: usize,
    pub fields: Vec<usize>,
}

#[derive(Debug, Default)]
pub struct BrowserFilterCache {
    epoch: u64,
    query: String,
    view: BrowserFilter,
    valid: bool,
}

impl BrowserFilter {
    pub fn build(model: &BrowserModel, query: &str) -> Self {
        let tokens = lowercase_query_tokens(query);
        if tokens.is_empty() {
            return Self::all(model);
        }

        let mut sources = Vec::new();
        for (source_idx, source) in model.sources.iter().enumerate() {
            if matches_lowercase_tokens(&tokens, &source.search_path) {
                sources.push(VisibleSource {
                    source: source_idx,
                    topics: source
                        .topics
                        .iter()
                        .enumerate()
                        .map(|(topic_idx, topic)| VisibleTopic {
                            topic: topic_idx,
                            fields: (0..topic.fields.len()).collect(),
                        })
                        .collect(),
                });
                continue;
            }

            let mut topics = Vec::new();
            for (topic_idx, topic) in source.topics.iter().enumerate() {
                if matches_lowercase_tokens(&tokens, &topic.search_path) {
                    topics.push(VisibleTopic {
                        topic: topic_idx,
                        fields: (0..topic.fields.len()).collect(),
                    });
                    continue;
                }

                let fields: Vec<usize> = topic
                    .fields
                    .iter()
                    .enumerate()
                    .filter_map(|(field_idx, field)| {
                        matches_lowercase_tokens(&tokens, &field.search_path).then_some(field_idx)
                    })
                    .collect();
                if !fields.is_empty() {
                    topics.push(VisibleTopic {
                        topic: topic_idx,
                        fields,
                    });
                }
            }

            if !topics.is_empty() {
                sources.push(VisibleSource {
                    source: source_idx,
                    topics,
                });
            }
        }
        Self { sources }
    }

    pub fn all(model: &BrowserModel) -> Self {
        Self {
            sources: model
                .sources
                .iter()
                .enumerate()
                .map(|(source_idx, source)| VisibleSource {
                    source: source_idx,
                    topics: source
                        .topics
                        .iter()
                        .enumerate()
                        .map(|(topic_idx, topic)| VisibleTopic {
                            topic: topic_idx,
                            fields: (0..topic.fields.len()).collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

impl BrowserFilterCache {
    pub fn view(&mut self, model_epoch: u64, model: &BrowserModel, query: &str) -> &BrowserFilter {
        if !self.valid || self.epoch != model_epoch || self.query != query {
            self.epoch = model_epoch;
            self.query.clear();
            self.query.push_str(query);
            self.view = BrowserFilter::build(model, query);
            self.valid = true;
        }
        &self.view
    }

    pub fn reset(&mut self) {
        self.epoch = 0;
        self.query.clear();
        self.view = BrowserFilter::default();
        self.valid = false;
    }
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
                let topic_search_path =
                    format!("{}/{}", source.entry.label, topic.entry.name).to_lowercase();

                let mut fields: Vec<FieldNode> = snapshot
                    .fields
                    .iter()
                    .filter(|f| f.topic == topic_id && !f.removed)
                    .map(|f| {
                        let schema = store.schema.field_by_name(&f.name);
                        let (first_raw, last_raw) = schema
                            .and_then(|schema| raw_endpoints(store, schema.name.as_str()))
                            .unwrap_or((None, None));
                        FieldNode {
                            id: f.id,
                            name: f.name.clone(),
                            dtype: schema.map(|s| s.dtype_label()).unwrap_or("?"),
                            unit: schema.and_then(|s| s.unit.clone()),
                            description: schema.and_then(|s| s.description.clone()),
                            count: rows,
                            first_raw,
                            last_raw,
                            search_path: format!("{topic_search_path}.{}", f.name).to_lowercase(),
                        }
                    })
                    .collect();
                fields.sort_by(|a, b| natural_cmp(&a.name, &b.name));

                topics.push(TopicNode {
                    id: topic_id,
                    name: topic.entry.name.clone(),
                    rows,
                    fields,
                    search_path: topic_search_path,
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
                search_path: source.entry.label.to_lowercase(),
            });
        }
        Self { sources }
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
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
    matches_lowercase_tokens(&lowercase_query_tokens(query), &path)
}

fn lowercase_query_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|token| token.to_lowercase())
        .collect()
}

fn matches_lowercase_tokens(tokens: &[String], lowercase_path: &str) -> bool {
    tokens
        .iter()
        .all(|token| lowercase_path.contains(token.as_str()))
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

fn raw_endpoints(
    store: &delog_core::store::TopicStore,
    field_name: &str,
) -> Option<(Option<String>, Option<String>)> {
    let field_index = store.schema.field_index(field_name)?;
    let first_chunk = store.chunks.iter().find(|chunk| !chunk.is_empty())?;
    let last_chunk = store.chunks.iter().rev().find(|chunk| !chunk.is_empty())?;
    let first = first_chunk
        .cols
        .get(field_index)
        .and_then(|array| raw_value_string(array, 0));
    let last_row = last_chunk.len().checked_sub(1)?;
    let last = last_chunk
        .cols
        .get(field_index)
        .and_then(|array| raw_value_string(array, last_row));
    Some((first, last))
}

fn raw_value_string(array: &ArrayRef, row: usize) -> Option<String> {
    if row >= array.len() || array.is_null(row) {
        return None;
    }

    let any = array.as_any();
    match array.data_type() {
        DataType::Int8 => Some(any.downcast_ref::<Int8Array>()?.value(row).to_string()),
        DataType::Int16 => Some(any.downcast_ref::<Int16Array>()?.value(row).to_string()),
        DataType::Int32 => Some(any.downcast_ref::<Int32Array>()?.value(row).to_string()),
        DataType::Int64 => Some(any.downcast_ref::<Int64Array>()?.value(row).to_string()),
        DataType::UInt8 => Some(any.downcast_ref::<UInt8Array>()?.value(row).to_string()),
        DataType::UInt16 => Some(any.downcast_ref::<UInt16Array>()?.value(row).to_string()),
        DataType::UInt32 => Some(any.downcast_ref::<UInt32Array>()?.value(row).to_string()),
        DataType::UInt64 => Some(any.downcast_ref::<UInt64Array>()?.value(row).to_string()),
        DataType::Float32 => Some(any.downcast_ref::<Float32Array>()?.value(row).to_string()),
        DataType::Float64 => Some(any.downcast_ref::<Float64Array>()?.value(row).to_string()),
        DataType::Boolean => Some(any.downcast_ref::<BooleanArray>()?.value(row).to_string()),
        DataType::Utf8 => Some(any.downcast_ref::<StringArray>()?.value(row).to_owned()),
        DataType::LargeUtf8 => Some(
            any.downcast_ref::<LargeStringArray>()?
                .value(row)
                .to_owned(),
        ),
        _ => None,
    }
}

fn display_endpoint(value: Option<&str>) -> &str {
    value.unwrap_or("-")
}

pub fn data_browser_toggle_button_size(ui: &egui::Ui) -> egui::Vec2 {
    let tokens = crate::ui::design_tokens::DesignTokens::from_style(ui.style());
    egui::Vec2::splat(tokens.control_height)
}

pub fn data_browser_toggle_button(
    ui: &mut egui::Ui,
    icon: egui::ImageSource<'static>,
    tooltip: &str,
) -> egui::Response {
    let tokens = crate::ui::design_tokens::DesignTokens::from_style(ui.style());
    let button_size = egui::Vec2::splat(tokens.control_height);
    let icon_size = egui::Vec2::splat(tokens.icon_size);
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = (button_size - icon_size) * 0.5;
        crate::ui::components::icon_button_sized(ui, icon, tooltip, false, button_size, icon_size)
    })
    .inner
}

pub fn ui(
    ui: &mut egui::Ui,
    model_epoch: u64,
    model: &BrowserModel,
    query: &mut String,
    filter_cache: &mut BrowserFilterCache,
    selection: &mut Selection,
    offset_dialog: &mut Option<(SourceId, i64)>,
) -> BrowserResponse {
    let mut response = BrowserResponse::default();
    let tokens = crate::ui::design_tokens::DesignTokens::from_style(ui.style());
    ui.add_space(5.0 + tokens.space_sm);
    ui.horizontal(|ui| {
        let button_size = data_browser_toggle_button_size(ui);
        let filter_height = button_size.y;
        let filter_width = (ui.available_width() - button_size.x - ui.spacing().item_spacing.x)
            .max(ui.spacing().interact_size.x);
        ui.add_sized(
            egui::vec2(filter_width, filter_height),
            egui::TextEdit::singleline(query)
                .hint_text("Filter...")
                .desired_width(filter_width),
        );
        if data_browser_toggle_button(
            ui,
            crate::ui::icons::panel_left_close(),
            "Hide data browser",
        )
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
    let view = filter_cache.view(model_epoch, model, query);
    if filtering && view.is_empty() {
        ui.add_space(8.0);
        ui.weak("Nothing matches the filter.");
        return response;
    }

    let visible: Vec<FieldId> = view
        .sources
        .iter()
        .flat_map(|visible_source| {
            let source = &model.sources[visible_source.source];
            visible_source.topics.iter().flat_map(move |visible_topic| {
                let topic = &source.topics[visible_topic.topic];
                visible_topic
                    .fields
                    .iter()
                    .map(move |&field_idx| topic.fields[field_idx].id)
            })
        })
        .collect();

    let mut offset_change = None;
    let mut remove_source = None;
    let mut inspect_source = None;
    let mut inspect_field_metadata = None;
    let mut inspect_field_stats = None;
    let mut generate_markers = None;
    let tree_id = if filtering {
        egui::Id::new("browser_tree_filtered")
    } else {
        egui::Id::new("browser_tree")
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let tokens = crate::ui::design_tokens::DesignTokens::from_style(ui.style());
            ui.spacing_mut().interact_size.y = tokens.dense_row_height;
            ui.spacing_mut().item_spacing.y = tokens.dense_row_gap;
            ui.set_width(ui.available_width());

            let mut open_nodes: std::collections::HashMap<BrowserNode, bool> =
                ui.data(|data| data.get_temp(tree_id)).unwrap_or_default();
            let mut state = egui_ltreeview::TreeViewState::<BrowserNode>::default();
            for (node, open) in &open_nodes {
                state.set_openness(*node, *open);
            }
            if filtering {
                for visible_source in &view.sources {
                    let source = &model.sources[visible_source.source];
                    state.set_openness(BrowserNode::Source(source.id.0), true);
                    for visible_topic in &visible_source.topics {
                        let topic = &source.topics[visible_topic.topic];
                        state.set_openness(BrowserNode::Topic(topic.id.0), true);
                    }
                }
            }

            let (_, tree_actions) = crate::ui::components::clamp_to_available_width(ui, |ui| {
                egui_ltreeview::TreeView::new(tree_id)
                .allow_multi_selection(false)
                .allow_drag_and_drop(false)
                .show_state(ui, &mut state, |builder| {
                    for visible_source in &view.sources {
                        let source = &model.sources[visible_source.source];
                        let header = format!("{}  ({} rows)", source.label, source.rows);
                        let source_open = builder.node(
                            egui_ltreeview::NodeBuilder::dir(BrowserNode::Source(source.id.0))
                                .default_open(true)
                                .label_ui(|ui| {
                                    ui.add(egui::Label::new(&header).selectable(false));
                                })
                                .context_menu(|ui| {
                                    crate::ui::components::dense_rows(ui);
                                    let info = egui::Image::new(crate::ui::icons::info())
                                        .fit_to_exact_size(egui::Vec2::splat(
                                            ui.spacing().icon_width,
                                        ))
                                        .tint(ui.visuals().text_color());
                                    if ui
                                        .add(egui::Button::image_and_text(info, "Source metadata"))
                                        .clicked()
                                    {
                                        inspect_source = Some(source.id);
                                        ui.close();
                                    }
                                    let trash = egui::Image::new(crate::ui::icons::trash())
                                        .fit_to_exact_size(egui::Vec2::splat(
                                            ui.spacing().icon_width,
                                        ))
                                        .tint(ui.visuals().error_fg_color);
                                    if ui
                                        .add(egui::Button::image_and_text(trash, "Remove source"))
                                        .clicked()
                                    {
                                        remove_source = Some(source.id);
                                        ui.close();
                                    }
                                }),
                        );
                        if !source_open {
                            builder.close_dir();
                            continue;
                        }

                        builder.node(
                            egui_ltreeview::NodeBuilder::leaf(BrowserNode::SourceMeta(source.id.0))
                                .label_ui(|ui| {
                                    ui.horizontal(|ui| {
                                        if let Some(range) = source.range {
                                            ui.weak(format!(
                                                "{:.3}–{:.3} s",
                                                range.min_us as f64 / 1e6,
                                                range.max_us as f64 / 1e6
                                            ));
                                        }
                                        if let Some(change) = offset_widget(ui, source, offset_dialog)
                                        {
                                            offset_change = Some(change);
                                        }
                                    });
                                }),
                        );

                        for visible_topic in &visible_source.topics {
                            let topic = &source.topics[visible_topic.topic];
                            let topic_open = builder.node(
                                egui_ltreeview::NodeBuilder::dir(BrowserNode::Topic(topic.id.0))
                                    .default_open(false)
                                    .label_ui(|ui| {
                                        ui.add(
                                            egui::Label::new(&topic.name).selectable(false),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(format!(
                                                            "({})",
                                                            topic.rows
                                                        ))
                                                        .weak(),
                                                    )
                                                    .selectable(false),
                                                );
                                            },
                                        );
                                    }),
                            );
                            if !topic_open {
                                builder.close_dir();
                                continue;
                            }

                            builder.node(
                                egui_ltreeview::NodeBuilder::leaf(BrowserNode::TopicHeader(
                                    topic.id.0,
                                ))
                                .label_ui(|ui| {
                                    field_table_header(ui);
                                }),
                            );
                            for &field_idx in &visible_topic.fields {
                                let field = &topic.fields[field_idx];
                                builder.node(
                                    egui_ltreeview::NodeBuilder::leaf(BrowserNode::Field(
                                        field.id.0,
                                    ))
                                    .label_ui(|ui| {
                                        match field_table_row(ui, field, selection, &visible) {
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
                                    }),
                                );
                            }
                            builder.close_dir();
                        }
                        builder.close_dir();
                    }
                })
            });
            for action in tree_actions {
                let egui_ltreeview::Action::SetSelected(clicked) = action else {
                    continue;
                };
                for node in clicked {
                    let Some(default_open) = default_openness(node) else {
                        continue;
                    };
                    let open = open_nodes.get(&node).copied().unwrap_or(default_open);
                    open_nodes.insert(node, !open);
                }
            }
            ui.data_mut(|data| data.insert_temp(tree_id, open_nodes));
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
    let clock = egui::Image::new(crate::ui::icons::clock())
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
    egui::Window::new(format!("Time offset - {label}"))
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

const FIELD_COL: f32 = 0.34;
const FIRST_COL: f32 = 0.22;
const LAST_COL: f32 = 0.22;
const UNIT_COL: f32 = 0.11;
const TYPE_COL: f32 = 0.11;

fn field_table_header(ui: &mut egui::Ui) {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(4, 0))
        .show(ui, |ui| {
            let width = (ui.available_width() - ui.spacing().item_spacing.x * 4.0).max(0.0);
            ui.horizontal(|ui| {
                field_table_cell(ui, width * FIELD_COL, egui::RichText::new(""), None);
                field_table_cell(ui, width * FIRST_COL, egui::RichText::new("first"), None);
                field_table_cell(ui, width * LAST_COL, egui::RichText::new("last"), None);
                field_table_cell(ui, width * UNIT_COL, egui::RichText::new("unit"), None);
                field_table_cell(ui, width * TYPE_COL, egui::RichText::new("type"), None);
            });
        });
}

fn field_table_row(
    ui: &mut egui::Ui,
    field: &FieldNode,
    selection: &mut Selection,
    visible: &[FieldId],
) -> Option<FieldRowAction> {
    field_row(ui, field, selection, visible, |ui, field, selected| {
        let width = (ui.available_width() - ui.spacing().item_spacing.x * 4.0).max(0.0);
        let name_color = if selected {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().text_color()
        };
        let first = display_endpoint(field.first_raw.as_deref());
        let last = display_endpoint(field.last_raw.as_deref());
        let unit = field.unit.as_deref().unwrap_or("-");
        ui.horizontal(|ui| {
            field_table_cell(
                ui,
                width * FIELD_COL,
                egui::RichText::new(&field.name).color(name_color),
                cell_hover_text(&field.name),
            );
            field_table_cell(
                ui,
                width * FIRST_COL,
                egui::RichText::new(first).weak(),
                cell_hover_text(first),
            );
            field_table_cell(
                ui,
                width * LAST_COL,
                egui::RichText::new(last).weak(),
                cell_hover_text(last),
            );
            field_table_cell(
                ui,
                width * UNIT_COL,
                egui::RichText::new(unit).weak(),
                cell_hover_text(unit),
            );
            field_table_cell(
                ui,
                width * TYPE_COL,
                egui::RichText::new(field.dtype).weak(),
                cell_hover_text(field.dtype),
            );
        });
    })
}

fn field_table_cell(
    ui: &mut egui::Ui,
    width: f32,
    text: impl Into<egui::WidgetText>,
    hover_text: Option<&str>,
) -> egui::InnerResponse<()> {
    let row_height =
        crate::ui::design_tokens::DesignTokens::from_style(ui.style()).dense_row_height;
    ui.allocate_ui_with_layout(
        egui::vec2(width, row_height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            let response = ui.add(egui::Label::new(text).truncate());
            if let Some(hover_text) = hover_text {
                response.on_hover_text(hover_text);
            }
        },
    )
}

fn cell_hover_text(value: &str) -> Option<&str> {
    (value != "-").then_some(value)
}

fn field_row(
    ui: &mut egui::Ui,
    field: &FieldNode,
    selection: &mut Selection,
    visible: &[FieldId],
    add_contents: impl FnOnce(&mut egui::Ui, &FieldNode, bool),
) -> Option<FieldRowAction> {
    let mut action = None;
    let id = egui::Id::new(("field", field.id.0));
    let dragging_this_field = ui.ctx().is_being_dragged(id);
    if dragging_this_field {
        selection.start_drag(field.id, current_select_modifier(ui), visible);
    }
    let payload = selection.drag_payload(field.id, visible);
    let drag_label = if payload.len() > 1 {
        format!("{} fields", payload.len())
    } else {
        field.name.clone()
    };
    let selected = selection.contains(field.id);

    let response = drag_source_with_click(ui, id, payload, &drag_label, |ui| {
        let fill = if selected {
            ui.visuals().selection.bg_fill
        } else {
            egui::Color32::TRANSPARENT
        };
        egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(4, 0))
            .show(ui, |ui| {
                add_contents(ui, field, selected);
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
        crate::ui::components::dense_rows(ui);
        let metadata_info = egui::Image::new(crate::ui::icons::info())
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
        let stats_info = egui::Image::new(crate::ui::icons::info())
            .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
            .tint(ui.visuals().text_color());
        if ui
            .add(egui::Button::image_and_text(stats_info, "Field stats"))
            .on_hover_text("Open field statistics")
            .clicked()
        {
            action = Some(FieldRowAction::InspectStats(field.id));
            ui.close();
        }
        if is_discrete_dtype(field.dtype) {
            let ruler = egui::Image::new(crate::ui::icons::ruler())
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
    drag_label: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    // Keep the row in place so the list stays stable during a drag.
    let inner = ui.scope(add_contents).response;
    let response = ui.interact(inner.rect, id, egui::Sense::click_and_drag());

    if ui.ctx().is_being_dragged(id) {
        egui::DragAndDrop::set_payload(ui.ctx(), payload);
        // A badge follows the cursor instead of lifting the rows out of the
        // list, so the selection stays visible in the browser.
        if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
            egui::Area::new(id.with("drag_ghost"))
                .order(egui::Order::Tooltip)
                .fixed_pos(pointer_pos + egui::vec2(12.0, 8.0))
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::new()
                        .fill(ui.visuals().selection.bg_fill)
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .corner_radius(4)
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(drag_label)
                                    .color(ui.visuals().selection.stroke.color),
                            );
                        });
                });
        }
        response.on_hover_cursor(egui::CursorIcon::Grabbing)
    } else {
        response.on_hover_cursor(egui::CursorIcon::Grab)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, BooleanArray, Float64Array, Int32Array, Int64Array, StringArray};
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::IdentityRegistry;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    use super::*;

    fn find_text_rect(shape: &egui::epaint::Shape, expected: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == expected => {
                Some(text.visual_bounding_rect())
            }
            egui::epaint::Shape::Vec(shapes) => shapes
                .iter()
                .find_map(|shape| find_text_rect(shape, expected)),
            _ => None,
        }
    }


    fn synth_model(sources: usize, topics: usize, fields: usize) -> BrowserModel {
        let mut model = BrowserModel::default();
        let mut fid = 0u32;
        for s in 0..sources {
            let mut source = SourceNode {
                id: SourceId(s as u32),
                label: format!("flight_{s}.bin"),
                rows: 1_000_000,
                range: None,
                offset_us: 0,
                topics: Vec::new(),
                search_path: format!("flight_{s}.bin"),
            };
            for t in 0..topics {
                let mut topic = TopicNode {
                    id: TopicId(((s * topics) + t) as u32),
                    name: format!("TOPIC{t:03}"),
                    rows: 10_000,
                    fields: Vec::new(),
                    search_path: format!("flight_{s}.bin.topic{t:03}"),
                };
                for f in 0..fields {
                    fid += 1;
                    topic.fields.push(FieldNode {
                        id: FieldId(fid),
                        name: format!("field_{f:02}"),
                        dtype: "f64",
                        unit: Some("m/s".into()),
                        description: None,
                        count: 10_000,
                        first_raw: Some("0.000".into()),
                        last_raw: Some("1.000".into()),
                        search_path: format!("flight_{s}.bin.topic{t:03}.field_{f:02}"),
                    });
                }
                source.topics.push(topic);
            }
            model.sources.push(source);
        }
        model
    }

    fn painted_shape_count(model: &BrowserModel, query: &str) -> usize {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let mut query = query.to_owned();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_600.0, 1_000.0),
            )),
            ..Default::default()
        };
        let mut run = || {
            ctx.run_ui(input(), |ui| {
                egui::Panel::left("culling-test")
                    .default_size(320.0)
                    .show_inside(ui, |ui| {
                        super::ui(
                            ui,
                            0,
                            model,
                            &mut query,
                            &mut filter_cache,
                            &mut selection,
                            &mut offset_dialog,
                        );
                    });
            })
        };
        let _ = run();
        let _ = run();
        run().shapes.len()
    }

    #[test]
    fn dragging_a_field_row_delivers_the_payload_to_a_drop_zone() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let model = synth_model(1, 1, 3);
        let dragged = model.sources[0].topics[0].fields[0].id;
        let mut query = "field".to_owned();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;
        let mut dropped: Option<Vec<FieldId>> = None;
        let mut row_pos = None;
        let mut zone_rect = egui::Rect::NOTHING;

        let mut frame = |events: Vec<egui::Event>,
                         pointer: Option<egui::Pos2>,
                         row_pos: &mut Option<egui::Pos2>,
                         zone_rect: &mut egui::Rect,
                         dropped: &mut Option<Vec<FieldId>>| {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_200.0, 800.0),
                )),
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.push(egui::Event::PointerMoved(pos));
            }
            input.events.extend(events);

            let output = ctx.run_ui(input, |ui| {
                egui::Panel::left("drag-browser")
                    .exact_size(420.0)
                    .show_inside(ui, |ui| {
                        super::ui(
                            ui,
                            0,
                            &model,
                            &mut query,
                            &mut filter_cache,
                            &mut selection,
                            &mut offset_dialog,
                        );
                    });
                egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                    let (inner, payload) =
                        ui.dnd_drop_zone::<Vec<FieldId>, ()>(egui::Frame::default(), |ui| {
                            ui.allocate_space(ui.available_size());
                        });
                    *zone_rect = inner.response.rect;
                    if let Some(payload) = payload {
                        *dropped = Some((*payload).clone());
                    }
                });
            });

            if row_pos.is_none() {
                *row_pos = find_text_rect_in(&output, "field_00").map(|rect| rect.center());
            }
        };

        frame(vec![], None, &mut row_pos, &mut zone_rect, &mut dropped);
        frame(vec![], None, &mut row_pos, &mut zone_rect, &mut dropped);
        let row = row_pos.expect("the field row should be painted");
        let target = zone_rect.center();

        frame(
            vec![egui::Event::PointerButton {
                pos: row,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
            Some(row),
            &mut row_pos,
            &mut zone_rect,
            &mut dropped,
        );
        frame(
            vec![],
            Some(row + egui::vec2(0.0, 20.0)),
            &mut row_pos,
            &mut zone_rect,
            &mut dropped,
        );
        for step in 0..4 {
            frame(
                vec![],
                Some(target + egui::vec2(step as f32 * 2.0, 0.0)),
                &mut row_pos,
                &mut zone_rect,
                &mut dropped,
            );
        }
        let release_at = target + egui::vec2(8.0, 0.0);
        frame(
            vec![egui::Event::PointerButton {
                pos: release_at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
            Some(release_at),
            &mut row_pos,
            &mut zone_rect,
            &mut dropped,
        );

        assert_eq!(
            dropped.as_deref(),
            Some(&[dragged][..]),
            "dragging a browser field row must hand a Vec<FieldId> to the plot drop zone"
        );
    }

    fn find_text_rect_in(output: &egui::FullOutput, expected: &str) -> Option<egui::Rect> {
        output
            .shapes
            .iter()
            .find_map(|clipped| find_text_rect(&clipped.shape, expected))
    }

    #[test]
    fn clicking_a_topic_label_expands_it() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let model = synth_model(1, 1, 3);
        let mut query = String::new();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;
        let mut topic_pos = None;
        let mut painted = Vec::new();

        let mut frame = |events: Vec<egui::Event>,
                         pointer: Option<egui::Pos2>,
                         topic_pos: &mut Option<egui::Pos2>,
                         painted: &mut Vec<String>| {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_200.0, 800.0),
                )),
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.push(egui::Event::PointerMoved(pos));
            }
            input.events.extend(events);
            let output = ctx.run_ui(input, |ui| {
                egui::Panel::left("topic-click")
                    .exact_size(420.0)
                    .show_inside(ui, |ui| {
                        super::ui(
                            ui,
                            0,
                            &model,
                            &mut query,
                            &mut filter_cache,
                            &mut selection,
                            &mut offset_dialog,
                        );
                    });
            });
            if topic_pos.is_none() {
                *topic_pos = output
                    .shapes
                    .iter()
                    .find_map(|clipped| find_text_rect(&clipped.shape, "TOPIC000"))
                    .map(|rect| rect.center());
            }
            painted.clear();
            fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
                match shape {
                    egui::epaint::Shape::Text(text) => out.push(text.galley.job.text.clone()),
                    egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            for clipped in &output.shapes {
                walk(&clipped.shape, painted);
            }
        };

        frame(vec![], None, &mut topic_pos, &mut painted);
        frame(vec![], None, &mut topic_pos, &mut painted);
        let target = topic_pos.expect("the topic row should be painted");
        assert!(
            !painted.iter().any(|text| text == "field_00"),
            "topics start collapsed"
        );

        frame(
            vec![egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
            Some(target),
            &mut topic_pos,
            &mut painted,
        );
        frame(
            vec![egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
            Some(target),
            &mut topic_pos,
            &mut painted,
        );
        frame(vec![], Some(target), &mut topic_pos, &mut painted);

        assert!(
            painted.iter().any(|text| text == "field_00"),
            "clicking the topic label should expand it, got {painted:?}"
        );

        for pressed in [true, false] {
            frame(
                vec![egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                }],
                Some(target),
                &mut topic_pos,
                &mut painted,
            );
        }
        frame(vec![], Some(target), &mut topic_pos, &mut painted);
        assert!(
            !painted.iter().any(|text| text == "field_00"),
            "clicking the topic label again should collapse it, got {painted:?}"
        );
    }

    #[test]
    fn clicking_a_topic_row_count_expands_it() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let model = synth_model(1, 1, 3);
        let mut query = String::new();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;
        let mut count_pos = None;
        let mut painted = Vec::new();

        let mut frame = |events: Vec<egui::Event>,
                         pointer: Option<egui::Pos2>,
                         count_pos: &mut Option<egui::Pos2>,
                         painted: &mut Vec<String>| {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_200.0, 800.0),
                )),
                ..Default::default()
            };
            if let Some(pos) = pointer {
                input.events.push(egui::Event::PointerMoved(pos));
            }
            input.events.extend(events);
            let output = ctx.run_ui(input, |ui| {
                egui::Panel::left("topic-count-click")
                    .exact_size(420.0)
                    .show_inside(ui, |ui| {
                        super::ui(
                            ui,
                            0,
                            &model,
                            &mut query,
                            &mut filter_cache,
                            &mut selection,
                            &mut offset_dialog,
                        );
                    });
            });
            if count_pos.is_none() {
                *count_pos = output
                    .shapes
                    .iter()
                    .find_map(|clipped| find_text_rect(&clipped.shape, "(10000)"))
                    .map(|rect| rect.center());
            }
            painted.clear();
            fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
                match shape {
                    egui::epaint::Shape::Text(text) => out.push(text.galley.job.text.clone()),
                    egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            for clipped in &output.shapes {
                walk(&clipped.shape, painted);
            }
        };

        frame(vec![], None, &mut count_pos, &mut painted);
        frame(vec![], None, &mut count_pos, &mut painted);
        let target = count_pos.expect("the topic row count should be painted");
        for pressed in [true, false] {
            frame(
                vec![egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                }],
                Some(target),
                &mut count_pos,
                &mut painted,
            );
        }
        frame(vec![], Some(target), &mut count_pos, &mut painted);

        assert!(
            painted.iter().any(|text| text == "field_00"),
            "clicking the row count should expand the topic too, got {painted:?}"
        );
    }

    #[test]
    fn browser_does_not_pin_the_panel_to_its_widest_layout() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let model = synth_model(1, 2, 6);
        let mut query = "field".to_owned();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;

        let measure = |panel_width: f32,
                           query: &mut String,
                           filter_cache: &mut BrowserFilterCache,
                           selection: &mut Selection,
                           offset_dialog: &mut Option<(SourceId, i64)>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_400.0, 800.0),
                )),
                ..Default::default()
            };
            let mut used = 0.0;
            let _ = ctx.run_ui(input, |ui| {
                egui::Panel::left("browser-shrink")
                    .resizable(false)
                    .exact_size(panel_width)
                    .show_inside(ui, |ui| {
                        super::ui(
                            ui,
                            0,
                            &model,
                            query,
                            filter_cache,
                            selection,
                            offset_dialog,
                        );
                        used = ui.min_rect().width();
                    });
            });
            used
        };

        measure(
            900.0,
            &mut query,
            &mut filter_cache,
            &mut selection,
            &mut offset_dialog,
        );
        measure(
            900.0,
            &mut query,
            &mut filter_cache,
            &mut selection,
            &mut offset_dialog,
        );
        let narrow = measure(
            380.0,
            &mut query,
            &mut filter_cache,
            &mut selection,
            &mut offset_dialog,
        );

        assert!(
            narrow <= 400.0,
            "after being shown wide the browser still demands {narrow} points, \
             which blocks resizing the panel back down"
        );
    }

    #[test]
    fn browser_columns_stay_visible_after_shrinking_the_panel() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let model = synth_model(1, 2, 4);
        let mut query = "field".to_owned();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;

        let render = |panel_width: f32,
                          query: &mut String,
                          filter_cache: &mut BrowserFilterCache,
                          selection: &mut Selection,
                          offset_dialog: &mut Option<(SourceId, i64)>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_400.0, 800.0),
                )),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                egui::Panel::left("browser-column-visibility")
                    .resizable(false)
                    .exact_size(panel_width)
                    .show_inside(ui, |ui| {
                        super::ui(
                            ui,
                            0,
                            &model,
                            query,
                            filter_cache,
                            selection,
                            offset_dialog,
                        );
                    });
            })
        };

        for width in [900.0, 900.0] {
            render(
                width,
                &mut query,
                &mut filter_cache,
                &mut selection,
                &mut offset_dialog,
            );
        }
        let narrow = render(
            380.0,
            &mut query,
            &mut filter_cache,
            &mut selection,
            &mut offset_dialog,
        );

        for label in ["type", "unit", "(10000)"] {
            let rect = narrow
                .shapes
                .iter()
                .find_map(|clipped| find_text_rect(&clipped.shape, label))
                .unwrap_or_else(|| panic!("{label} should still be painted in a 380 point panel"));
            assert!(
                rect.right() <= 380.0,
                "{label} is drawn out to x={} which is outside a 380 point panel",
                rect.right()
            );
        }
    }

    #[test]
    fn browser_still_paints_rows_after_scrolling_down() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let model = synth_model(1, 12, 10);
        let mut query = "field".to_owned();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;

        let frame = |events: Vec<egui::Event>,
                     query: &mut String,
                     filter_cache: &mut BrowserFilterCache,
                     selection: &mut Selection,
                     offset_dialog: &mut Option<(SourceId, i64)>| {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_000.0, 400.0),
                )),
                ..Default::default()
            };
            input
                .events
                .push(egui::Event::PointerMoved(egui::pos2(200.0, 200.0)));
            input.events.extend(events);
            let output = ctx.run_ui(input, |ui| {
                egui::Panel::left("browser-scroll")
                    .exact_size(420.0)
                    .show_inside(ui, |ui| {
                        super::ui(
                            ui,
                            0,
                            &model,
                            query,
                            filter_cache,
                            selection,
                            offset_dialog,
                        );
                    });
            });
            let mut texts = Vec::new();
            fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
                match shape {
                    egui::epaint::Shape::Text(text) => out.push(text.galley.job.text.clone()),
                    egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut texts);
            }
            texts
        };

        let before = frame(
            vec![],
            &mut query,
            &mut filter_cache,
            &mut selection,
            &mut offset_dialog,
        );
        assert!(
            before.iter().any(|text| text.starts_with("field_")),
            "rows should be painted before scrolling, got {before:?}"
        );

        let mut after = Vec::new();
        for _ in 0..6 {
            after = frame(
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -400.0),
                    modifiers: Default::default(),
                    phase: egui::TouchPhase::Move,
                }],
                &mut query,
                &mut filter_cache,
                &mut selection,
                &mut offset_dialog,
            );
        }

        assert!(
            after.iter().any(|text| text.starts_with("field_")),
            "rows should still be painted after scrolling down, got {after:?}"
        );
        assert_ne!(
            before
                .iter()
                .filter(|t| t.starts_with("field_"))
                .count(),
            0,
            "sanity"
        );
    }

    #[test]
    fn browser_culls_rows_outside_the_viewport() {
        let small = synth_model(1, 10, 8);
        let huge = synth_model(4, 250, 12);

        let small_shapes = painted_shape_count(&small, "field");
        let huge_shapes = painted_shape_count(&huge, "field");

        assert!(
            huge_shapes < small_shapes * 2,
            "a 150x larger tree must not paint proportionally more shapes \
             (small={small_shapes}, huge={huge_shapes}) - viewport culling regressed"
        );
    }

    #[test]
    fn data_browser_toggle_matches_global_toolbar_metrics_and_alignment() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        ctx.enable_accesskit();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_200.0, 800.0),
            )),
            ..Default::default()
        };
        let mut query = String::new();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;
        let output = ctx.run_ui(input, |ui| {
            egui::Panel::left("browser-alignment-test")
                .default_size(280.0)
                .show_inside(ui, |ui| {
                    super::ui(
                        ui,
                        0,
                        &BrowserModel::default(),
                        &mut query,
                        &mut filter_cache,
                        &mut selection,
                        &mut offset_dialog,
                    );
                });
            let button_size = super::data_browser_toggle_button_size(ui);
            let collapsed_left_margin = ui.spacing().item_spacing.x;
            let collapsed_frame =
                egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::ZERO);
            egui::Panel::left("collapsed-browser-alignment-test")
                .resizable(false)
                .show_separator_line(false)
                .frame(collapsed_frame)
                .exact_size(collapsed_left_margin + button_size.x)
                .show_inside(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(15.0);
                        ui.horizontal(|ui| {
                            ui.add_space(collapsed_left_margin);
                            super::data_browser_toggle_button(
                                ui,
                                crate::ui::icons::panel_left_open(),
                                "Show data browser",
                            );
                        });
                    });
                });
            egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    crate::ui::components::icon_button(
                        ui,
                        crate::ui::icons::magnet(),
                        "Toolbar control",
                        false,
                    );
                });
            });
        });
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree should be emitted");
        let bounds = |label: &str| {
            update
                .nodes
                .iter()
                .map(|(_, node)| node)
                .find(|node| node.label() == Some(label))
                .and_then(|node| node.bounds())
                .unwrap_or_else(|| panic!("{label} button should have bounds"))
        };
        let browser_bounds = bounds("Hide data browser");
        let collapsed_browser_bounds = bounds("Show data browser");
        let toolbar_bounds = bounds("Toolbar control");

        assert_eq!(browser_bounds.size(), toolbar_bounds.size());
        assert_eq!(collapsed_browser_bounds.size(), toolbar_bounds.size());
        let center_y = |rect: egui::accesskit::Rect| (rect.y0 + rect.y1) * 0.5;
        assert_eq!(center_y(browser_bounds), center_y(toolbar_bounds));
        assert_eq!(center_y(collapsed_browser_bounds), center_y(toolbar_bounds));
        assert_eq!(browser_bounds.width(), 30.0);
        assert_eq!(browser_bounds.height(), 30.0);
    }

    #[test]
    fn data_browser_toggle_response_is_exactly_thirty_points() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let mut response_rect = egui::Rect::NOTHING;

        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            response_rect = super::data_browser_toggle_button(
                ui,
                crate::ui::icons::panel_left_close(),
                "Hide data browser",
            )
            .rect;
        });

        assert_eq!(response_rect.size(), egui::Vec2::splat(30.0));
    }

    #[test]
    fn data_browser_filter_matches_toggle_height_and_center() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        ctx.enable_accesskit();
        let mut query = String::new();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;

        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            super::ui(
                ui,
                0,
                &BrowserModel::default(),
                &mut query,
                &mut filter_cache,
                &mut selection,
                &mut offset_dialog,
            );
        });
        let update = output
            .platform_output
            .accesskit_update
            .expect("accessibility tree should be emitted");
        let filter_bounds = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.role() == egui::accesskit::Role::TextInput)
            .and_then(|node| node.bounds())
            .expect("filter input should have bounds");
        let button_bounds = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.label() == Some("Hide data browser"))
            .and_then(|node| node.bounds())
            .expect("browser toggle should have bounds");
        let center_y = |rect: egui::accesskit::Rect| (rect.y0 + rect.y1) * 0.5;

        assert_eq!(filter_bounds.height(), 30.0);
        assert_eq!(button_bounds.height(), 30.0);
        assert_eq!(center_y(filter_bounds), center_y(button_bounds));
    }

    #[test]
    fn field_table_cells_use_the_dense_row_height_token() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let mut cell_height = 0.0;

        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            cell_height = super::field_table_cell(ui, 120.0, "ATT.Roll", None)
                .response
                .rect
                .height();
        });

        let tokens = crate::ui::design_tokens::DesignTokens::from_style(&ctx.global_style());
        assert_eq!(cell_height, tokens.dense_row_height);
    }

    #[test]
    fn rendered_browser_field_rows_use_dense_vertical_metrics() {
        let ctx = egui::Context::default();
        crate::ui::theme::ThemeChoice::CatppuccinMocha.apply(&ctx);
        let model = BrowserModel::from_snapshot(&snapshot());
        let mut query = "a".to_owned();
        let mut filter_cache = BrowserFilterCache::default();
        let mut selection = Selection::default();
        let mut offset_dialog = None;

        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| {
                super::ui(
                    ui,
                    0,
                    &model,
                    &mut query,
                    &mut filter_cache,
                    &mut selection,
                    &mut offset_dialog,
                );
            },
        );
        let text_rect = |expected| {
            output
                .shapes
                .iter()
                .find_map(|shape| find_text_rect(&shape.shape, expected))
                .unwrap_or_else(|| panic!("{expected} should be painted"))
        };
        let alt = text_rect("Alt");
        let lat = text_rect("Lat");
        let tokens = crate::ui::design_tokens::DesignTokens::from_style(&ctx.global_style());
        let expected_stride = tokens.dense_row_height + tokens.dense_row_gap;

        let actual_stride = (lat.center().y - alt.center().y).round();
        assert!((actual_stride - expected_stride).abs() <= 1.0);
    }

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
    fn model_includes_raw_first_and_last_values() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "STAT").unwrap();
        identity.add_field(topic, "Mode").unwrap();
        identity.add_field(topic, "Armed").unwrap();
        identity.add_field(topic, "Alt").unwrap();

        let schema = Arc::new(
            TopicSchema::new(
                "STAT",
                [
                    FieldSchema::new("Mode", DataType::Utf8, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("Armed", DataType::Boolean, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("Alt", DataType::Float64, Some("m"), 0.01).unwrap(),
                ],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![100, 200, 300]),
                vec![
                    Arc::new(StringArray::from(vec!["idle", "climb", "land"])) as ArrayRef,
                    Arc::new(BooleanArray::from(vec![false, true, false])) as ArrayRef,
                    Arc::new(Float64Array::from(vec![1200.0, 1234.5, 1300.0])) as ArrayRef,
                ],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();

        let model = BrowserModel::from_snapshot(&snapshot);
        let fields = &model.sources[0].topics[0].fields;

        let alt = fields.iter().find(|f| f.name == "Alt").unwrap();
        assert_eq!(alt.first_raw.as_deref(), Some("1200"));
        assert_eq!(alt.last_raw.as_deref(), Some("1300"));

        let armed = fields.iter().find(|f| f.name == "Armed").unwrap();
        assert_eq!(armed.first_raw.as_deref(), Some("false"));
        assert_eq!(armed.last_raw.as_deref(), Some("false"));

        let mode = fields.iter().find(|f| f.name == "Mode").unwrap();
        assert_eq!(mode.first_raw.as_deref(), Some("idle"));
        assert_eq!(mode.last_raw.as_deref(), Some("land"));
    }

    #[test]
    fn raw_endpoint_values_are_none_for_nulls() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source("flight");
        let topic = identity.add_topic(source, "STAT").unwrap();
        identity.add_field(topic, "Alt").unwrap();

        let schema = Arc::new(
            TopicSchema::new(
                "STAT",
                [FieldSchema::new("Alt", DataType::Float64, Some("m"), 1.0).unwrap()],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![100, 200]),
                vec![Arc::new(Float64Array::from(vec![None, Some(42.0)])) as ArrayRef],
                &schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(Arc::clone(&schema), [chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&identity, [(topic, store)], 0).unwrap();

        let model = BrowserModel::from_snapshot(&snapshot);
        let alt = &model.sources[0].topics[0].fields[0];

        assert_eq!(alt.first_raw, None);
        assert_eq!(alt.last_raw.as_deref(), Some("42"));
        assert_eq!(display_endpoint(None), "-");
        assert_eq!(display_endpoint(Some("42")), "42");
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
    fn filter_view_retains_matching_fields_and_prunes_empty_branches() {
        let model = BrowserModel::from_snapshot(&snapshot());
        let view = BrowserFilter::build(&model, "gps lat");

        assert!(!view.is_empty());
        assert_eq!(view.sources.len(), 1);
        assert_eq!(view.sources[0].source, 0);
        assert_eq!(view.sources[0].topics.len(), 1);
        assert_eq!(view.sources[0].topics[0].topic, 0);

        let field_names: Vec<_> = view.sources[0].topics[0]
            .fields
            .iter()
            .map(|&field_idx| model.sources[0].topics[0].fields[field_idx].name.as_str())
            .collect();
        assert_eq!(field_names, vec!["Lat"]);
    }

    #[test]
    fn filter_view_preserves_branch_match_semantics() {
        let model = BrowserModel::from_snapshot(&snapshot());

        let source = BrowserFilter::build(&model, "flight");
        assert_eq!(source.sources.len(), 1);
        assert_eq!(source.sources[0].topics.len(), 1);
        assert_eq!(source.sources[0].topics[0].fields.len(), 2);

        let topic = BrowserFilter::build(&model, "gps");
        assert_eq!(topic.sources.len(), 1);
        assert_eq!(topic.sources[0].topics.len(), 1);
        assert_eq!(topic.sources[0].topics[0].fields.len(), 2);

        let field = BrowserFilter::build(&model, "lat");
        let field_names: Vec<_> = field.sources[0].topics[0]
            .fields
            .iter()
            .map(|&field_idx| model.sources[0].topics[0].fields[field_idx].name.as_str())
            .collect();
        assert_eq!(field_names, vec!["Lat"]);

        assert!(BrowserFilter::build(&model, "nonexistent").is_empty());
        assert_eq!(BrowserFilter::build(&model, ""), BrowserFilter::all(&model));
    }

    #[test]
    fn filter_cache_reuses_results_until_query_or_epoch_changes() {
        let model = BrowserModel::from_snapshot(&snapshot());
        let mut changed = model.clone();
        changed.sources[0].topics[0]
            .fields
            .retain(|field| field.name != "Lat");
        let mut cache = BrowserFilterCache::default();

        let blank = cache.view(1, &model, "");
        assert_eq!(blank.sources[0].topics[0].fields.len(), 2);

        let lat = cache.view(1, &model, "lat");
        assert_eq!(lat.sources[0].topics[0].fields.len(), 1);

        let lat_after_epoch_change = cache.view(2, &changed, "lat");
        assert!(lat_after_epoch_change.is_empty());

        let blank_again = cache.view(2, &changed, "");
        assert_eq!(blank_again.sources[0].topics[0].fields.len(), 1);
    }
}

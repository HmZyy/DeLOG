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
    pub range: Option<TimeRange>,
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

            sources.push(SourceNode {
                id: source.entry.id,
                label: source.entry.label.clone(),
                rows: source_rows,
                range: source_range,
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

/// Render the browser tree with its search box (BRW-01/02). `query` persists
/// in app state across frames.
pub fn ui(ui: &mut egui::Ui, model: &BrowserModel, query: &mut String) {
    ui.heading("Data");
    ui.separator();

    if model.is_empty() {
        ui.add_space(8.0);
        ui.weak("No logs loaded.");
        ui.weak("Drop a .BIN file here, or use File ▸ Open.");
        return;
    }

    // Fuzzy filter over full paths (§13, BRW-02).
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(query)
                .hint_text("Filter (e.g. gps hacc)")
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
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for source in &model.sources {
            let header = format!("{}  ({} rows)", source.label, source.rows);
            egui::CollapsingHeader::new(header)
                .id_salt(("source", source.id.0))
                .default_open(true)
                .show(ui, |ui| {
                    if let Some(range) = source.range {
                        ui.weak(format!(
                            "{:.3}–{:.3} s",
                            range.min_us as f64 / 1e6,
                            range.max_us as f64 / 1e6
                        ));
                    }
                    for topic in &source.topics {
                        // While filtering, surviving topics open so the
                        // matched fields are visible immediately.
                        egui::CollapsingHeader::new(format!("{}  ({})", topic.name, topic.rows))
                            .id_salt(("topic", topic.id.0))
                            .default_open(false)
                            .open(filtering.then_some(true))
                            .show(ui, |ui| {
                                for field in &topic.fields {
                                    field_row(ui, field);
                                }
                            });
                    }
                });
        }
    });
}

fn field_row(ui: &mut egui::Ui, field: &FieldNode) {
    // The row is a drag source carrying its FieldId; the plot pane is the drop
    // zone (PLT-13).
    let id = egui::Id::new(("field", field.id.0));
    ui.dnd_drag_source(id, field.id, |ui| {
        ui.horizontal(|ui| {
            ui.label(&field.name);
            ui.weak(field.dtype);
            if let Some(unit) = &field.unit {
                ui.weak(format!("[{unit}]"));
            }
        });
    });
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
        assert_eq!(src.range, TimeRange::new(100, 300));

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

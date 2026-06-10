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

                let fields = snapshot
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

                topics.push(TopicNode {
                    id: topic_id,
                    name: topic.entry.name.clone(),
                    rows,
                    fields,
                });
            }

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
}

/// Render the browser tree. Pure display — selection/drag/context come later.
pub fn ui(ui: &mut egui::Ui, model: &BrowserModel) {
    ui.heading("Data");
    ui.separator();

    if model.is_empty() {
        ui.add_space(8.0);
        ui.weak("No logs loaded.");
        ui.weak("Drop a .BIN file here, or use File ▸ Open.");
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
                        egui::CollapsingHeader::new(format!("{}  ({})", topic.name, topic.rows))
                            .id_salt(("topic", topic.id.0))
                            .default_open(false)
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
    ui.horizontal(|ui| {
        ui.label(&field.name);
        ui.weak(field.dtype);
        if let Some(unit) = &field.unit {
            ui.weak(format!("[{unit}]"));
        }
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

        assert_eq!(gps.fields.len(), 2);
        assert_eq!(gps.fields[0].name, "Lat");
        assert_eq!(gps.fields[0].dtype, "i32");
        assert_eq!(gps.fields[0].unit.as_deref(), Some("deg"));
        assert_eq!(gps.fields[0].count, 3);
        assert_eq!(gps.fields[1].dtype, "f64");
    }

    #[test]
    fn empty_snapshot_yields_an_empty_model() {
        assert!(BrowserModel::from_snapshot(&StoreSnapshot::empty()).is_empty());
    }
}

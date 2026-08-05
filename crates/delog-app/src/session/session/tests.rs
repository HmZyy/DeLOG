use std::io::Write;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float32Array, Float64Array, Int64Array, LargeStringArray,
    StringArray, TimestampMillisecondArray, UInt16Array,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use delog_core::chunk::Chunk;
use delog_core::identity::IdentityRegistry;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::snapshot::StoreSnapshot;
use delog_core::store::TopicStore;
use delog_parsers::{LogParser, ParseError, ReadSeek, Sniff};
use parquet::arrow::ArrowWriter;

use super::*;
use crate::plotting::browser::BrowserModel;
use crate::export::data_export::{
    ExportCtl, ExportField, ExportFormat, available_fields, write_export_file,
};
use delog_core::export::ResampleMode;

#[cfg(feature = "scripting")]
#[test]
fn app_observer_mirrors_only_live_batches_without_blocking() {
    use std::sync::mpsc::channel;

    use arrow::array::{ArrayRef, Float64Array, Int64Array};
    use arrow::datatypes::DataType;
    use delog_core::ingest::{ParsedBatch, SourceKind};
    use delog_core::schema::{FieldSchema, TopicSchema};

    let (tx, rx) = channel();
    let live_scripts = Arc::new(Mutex::new(Some(tx)));
    let mut observer = AppObserver {
        loads: Arc::default(),
        load_terminals: Arc::default(),
        diagnostics: Arc::default(),
        ctx: egui::Context::default(),
        live_scripts,
    };
    let schema = Arc::new(
        TopicSchema::new(
            "A",
            [FieldSchema::new("v", DataType::Float64, None::<String>, 1.0).unwrap()],
        )
        .unwrap(),
    );
    let batch = ParsedBatch::new(
        SourceId(0),
        schema,
        Int64Array::from(vec![1]),
        vec![Arc::new(Float64Array::from(vec![2.0])) as ArrayRef],
    );

    observer.on_batch(SourceKind::File, "flight", &batch);
    assert!(rx.try_recv().is_err(), "file batches are not mirrored");

    observer.on_batch(SourceKind::Live, "live", &batch);
    let mirrored = rx.try_recv().expect("live batches are mirrored");
    assert_eq!(mirrored.source_label, "live");
    assert_eq!(mirrored.batch.source, batch.source);
}

fn tiny_bin() -> Vec<u8> {
    const HEAD1: u8 = 0xA3;
    const HEAD2: u8 = 0x95;
    const FMT: u8 = 0x80;
    let mut buf = Vec::new();
    // FMT for TEST(200): format "Qf" (TimeUS, A), length = 3 + 8 + 4 = 15.
    buf.extend([HEAD1, HEAD2, FMT, 200, 15]);
    let field = |s: &str, w: usize, b: &mut Vec<u8>| {
        b.extend(s.as_bytes());
        b.extend(std::iter::repeat_n(0u8, w - s.len()));
    };
    field("TEST", 4, &mut buf);
    field("Qf", 16, &mut buf);
    field("TimeUS,A", 64, &mut buf);
    for (t, a) in [(1_000u64, 1.5f32), (2_000, 2.5)] {
        buf.extend([HEAD1, HEAD2, 200]);
        buf.extend(t.to_le_bytes());
        buf.extend(a.to_le_bytes());
    }
    buf
}

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("delog-session-test-{name}.BIN"));
    p
}

fn temp_parquet_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("delog-session-test-{name}.parquet"));
    path
}

fn write_generic_parquet(path: &Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new(
            "time",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
        Field::new("value", DataType::Float32, false),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(TimestampMillisecondArray::from(vec![1, 2])) as ArrayRef,
            Arc::new(Float32Array::from(vec![1.5, 2.5])) as ArrayRef,
        ],
    )
    .unwrap();
    let file = File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn write_unsorted_generic_parquet(path: &Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new(
            "time",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
        Field::new("value", DataType::Float32, false),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(TimestampMillisecondArray::from(vec![4, 1, 3, 2])) as ArrayRef,
            Arc::new(Float32Array::from(vec![40.0, 10.0, 30.0, 20.0])) as ArrayRef,
        ],
    )
    .unwrap();
    let file = File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn write_all_invalid_parquet(path: &Path) {
    let rows = delog_parsers::parquet::PARQUET_BATCH_ROWS * 32;
    let schema = Arc::new(Schema::new(vec![
        Field::new("time", DataType::Timestamp(TimeUnit::Millisecond, None), true),
        Field::new("value", DataType::Float32, true),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(TimestampMillisecondArray::from(vec![None; rows])) as ArrayRef,
            Arc::new(Float32Array::from(vec![Some(1.0); rows])) as ArrayRef,
        ],
    )
    .unwrap();
    let file = File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn structured_export_fixture() -> (StoreSnapshot, Vec<ExportField>) {
    let mut identity = IdentityRegistry::new();
    let flight_a = identity.add_source("flight-a");
    identity.set_source_offset_us(flight_a, 100).unwrap();
    let att_a = identity.add_topic(flight_a, "ATT").unwrap();
    for name in ["Roll", "Seq", "Pitch"] {
        identity.add_field(att_a, name).unwrap();
    }
    let status = identity.add_topic(flight_a, "STATUS").unwrap();
    for name in ["armed", "mode", "message", "ignored"] {
        identity.add_field(status, name).unwrap();
    }

    let flight_b = identity.add_source("flight-b");
    identity.set_source_offset_us(flight_b, -200).unwrap();
    let att_b = identity.add_topic(flight_b, "ATT").unwrap();
    for name in ["Roll", "Yaw"] {
        identity.add_field(att_b, name).unwrap();
    }

    let att_a_schema = Arc::new(
        TopicSchema::new(
            "ATT",
            [
                FieldSchema::new("Roll", DataType::Float32, Some("deg"), 0.01)
                    .unwrap()
                    .with_description("roll angle"),
                FieldSchema::new("Seq", DataType::UInt16, Some("count"), 2.0)
                    .unwrap()
                    .with_description("sample sequence"),
                FieldSchema::new("Pitch", DataType::Float64, Some("deg"), 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    let att_a_chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![1_000, 2_000]),
            vec![
                Arc::new(Float32Array::from(vec![Some(1.25), None])) as ArrayRef,
                Arc::new(UInt16Array::from(vec![Some(10), Some(11)])) as ArrayRef,
                Arc::new(Float64Array::from(vec![Some(2.5), Some(3.5)])) as ArrayRef,
            ],
            &att_a_schema,
        )
        .unwrap(),
    );
    let att_a_store = Arc::new(TopicStore::from_chunks(att_a_schema, [att_a_chunk]).unwrap());

    let status_schema = Arc::new(
        TopicSchema::new(
            "STATUS",
            [
                FieldSchema::new("armed", DataType::Boolean, None::<String>, 1.0)
                    .unwrap()
                    .with_description("arming state"),
                FieldSchema::new("mode", DataType::Utf8, None::<String>, 1.0)
                    .unwrap()
                    .with_description("flight mode"),
                FieldSchema::new("message", DataType::LargeUtf8, None::<String>, 1.0)
                    .unwrap()
                    .with_description("status message"),
                FieldSchema::new("ignored", DataType::Int64, None::<String>, 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    let status_chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![1_200, 3_200]),
            vec![
                Arc::new(BooleanArray::from(vec![Some(true), None])) as ArrayRef,
                Arc::new(StringArray::from(vec![Some("AUTO"), None])) as ArrayRef,
                Arc::new(LargeStringArray::from(vec![None, Some("ready")])) as ArrayRef,
                Arc::new(Int64Array::from(vec![99, 100])) as ArrayRef,
            ],
            &status_schema,
        )
        .unwrap(),
    );
    let status_store =
        Arc::new(TopicStore::from_chunks(status_schema, [status_chunk]).unwrap());

    let att_b_schema = Arc::new(
        TopicSchema::new(
            "ATT",
            [
                FieldSchema::new("Roll", DataType::Float32, Some("rad"), 1.0)
                    .unwrap()
                    .with_description("secondary roll"),
                FieldSchema::new("Yaw", DataType::Float64, Some("rad"), 1.0).unwrap(),
            ],
        )
        .unwrap(),
    );
    let att_b_chunk = Arc::new(
        Chunk::try_new(
            Int64Array::from(vec![1_500, 2_500, 3_500]),
            vec![
                Arc::new(Float32Array::from(vec![Some(-2.0), Some(-1.0), Some(0.0)]))
                    as ArrayRef,
                Arc::new(Float64Array::from(vec![0.5, 0.75, 1.0])) as ArrayRef,
            ],
            &att_b_schema,
        )
        .unwrap(),
    );
    let att_b_store = Arc::new(TopicStore::from_chunks(att_b_schema, [att_b_chunk]).unwrap());

    let snapshot = StoreSnapshot::from_registry(
        &identity,
        [
            (att_a, att_a_store),
            (status, status_store),
            (att_b, att_b_store),
        ],
        1,
    )
    .unwrap();
    let model = BrowserModel::from_snapshot(&snapshot);
    let fields = available_fields(&snapshot, &model)
        .into_iter()
        .filter(|field| {
            matches!(
                (
                    field.source.as_str(),
                    field.topic.as_str(),
                    field.name.as_str()
                ),
                ("flight-a", "ATT", "Roll" | "Seq")
                    | ("flight-a", "STATUS", "armed" | "mode" | "message")
                    | ("flight-b", "ATT", "Roll")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), 6);
    (snapshot, fields)
}

struct LoadedStructuredExport {
    snapshot: Arc<StoreSnapshot>,
    exported_rows: u64,
    terminal: LoadTerminal,
    diagnostics: Vec<DiagRecord>,
}

fn open_parquet_path(
    path: PathBuf,
    exported_rows: u64,
    expected_diagnostic: Option<&str>,
) -> LoadedStructuredExport {
    let mut session = Session::new(egui::Context::default());
    session.open_path(path, None);
    session.join_workers();
    session.wait_until(|session| {
        !session.load_terminals.lock().unwrap().is_empty()
            && expected_diagnostic.is_none_or(|code| {
                session
                    .diagnostic_records()
                    .iter()
                    .any(|record| record.diag.code == code)
            })
    });
    let terminal = session
        .load_terminals
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .expect("one parser terminal event");
    LoadedStructuredExport {
        snapshot: session.snapshot(),
        exported_rows,
        terminal,
        diagnostics: session.diagnostic_records(),
    }
}

fn load_structured_export(window: (i64, i64), file_stem: &str) -> LoadedStructuredExport {
    let (source_snapshot, fields) = structured_export_fixture();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(format!("{file_stem}.parquet"));
    let exported_rows = write_export_file(
        &path,
        ExportFormat::Parquet,
        &source_snapshot,
        &fields,
        window,
        ResampleMode::None,
        window.0,
        &ExportCtl::default(),
    )
    .unwrap();
    open_parquet_path(path, exported_rows, None)
}

pub(crate) fn structured_round_trip_snapshot() -> Arc<StoreSnapshot> {
    let loaded = load_structured_export((1_100, 3_300), "structured-metadata");
    assert_eq!(loaded.exported_rows, 7);
    assert!(matches!(loaded.terminal, LoadTerminal::Closed(_)));
    loaded.snapshot
}

fn store_for<'a>(snapshot: &'a StoreSnapshot, topic_name: &str) -> &'a TopicStore {
    snapshot
        .topics
        .iter()
        .find(|topic| topic.entry.name == topic_name)
        .and_then(|topic| topic.store.as_deref())
        .unwrap_or_else(|| panic!("missing topic store {topic_name}"))
}

fn topic_times(store: &TopicStore) -> Vec<i64> {
    store
        .chunks
        .iter()
        .flat_map(|chunk| chunk.t.values().iter().copied())
        .collect()
}

fn float32_values(store: &TopicStore, field_name: &str) -> Vec<Option<f32>> {
    let field_index = store.schema.field_index(field_name).unwrap();
    store
        .chunks
        .iter()
        .flat_map(|chunk| {
            chunk.cols[field_index]
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap()
                .iter()
        })
        .collect()
}

#[test]
fn structured_parquet_round_trip_preserves_topics_and_fields() {
    let loaded = load_structured_export((1_100, 3_300), "structured-round-trip");
    assert_eq!(loaded.exported_rows, 7);
    assert_eq!(
        loaded.terminal,
        LoadTerminal::Closed(ParseSummary {
            topic_count: 3,
            row_count: 7,
            time_range: delog_core::time::TimeRange::new(1_100, 3_300),
            diagnostics: 0,
            source_meta: Default::default(),
        })
    );
    let snapshot = loaded.snapshot;
    assert_eq!(
        snapshot
            .topics
            .iter()
            .filter(|topic| topic.store.is_some())
            .count(),
        3
    );

    let att_a = store_for(&snapshot, "ATT[0]");
    let att_b = store_for(&snapshot, "ATT[1]");
    let status = store_for(&snapshot, "STATUS");
    assert_eq!(att_a.rows, 2);
    assert_eq!(att_b.rows, 3);
    assert_eq!(status.rows, 2);
    assert_eq!(topic_times(att_a), [1_100, 2_100]);
    assert_eq!(topic_times(att_b), [1_300, 2_300, 3_300]);
    assert_eq!(topic_times(status), [1_300, 3_300]);

    assert_eq!(
        att_a.schema.provenance().unwrap().original_source(),
        "flight-a"
    );
    assert_eq!(att_a.schema.provenance().unwrap().original_topic(), "ATT");
    assert_eq!(
        att_b.schema.provenance().unwrap().original_source(),
        "flight-b"
    );
    assert_eq!(
        status.schema.provenance().unwrap().original_source(),
        "flight-a"
    );
    assert_eq!(
        status.schema.provenance().unwrap().original_topic(),
        "STATUS"
    );

    let roll = att_a.schema.field_by_name("Roll").unwrap();
    assert_eq!(roll.dtype, DataType::Float32);
    assert_eq!(roll.unit.as_deref(), Some("deg"));
    assert_eq!(roll.multiplier, 0.01);
    assert_eq!(roll.description.as_deref(), Some("roll angle"));
    assert_eq!(float32_values(att_a, "Roll"), [Some(1.25), None]);
    assert_eq!(
        att_a.schema.field_by_name("Seq").unwrap().dtype,
        DataType::UInt16
    );
    assert!(att_a.schema.field_by_name("Pitch").is_none());
    assert!(att_b.schema.field_by_name("Yaw").is_none());

    assert_eq!(
        status.schema.field_by_name("armed").unwrap().dtype,
        DataType::Boolean
    );
    assert_eq!(
        status.schema.field_by_name("mode").unwrap().dtype,
        DataType::Utf8
    );
    assert_eq!(
        status.schema.field_by_name("message").unwrap().dtype,
        DataType::LargeUtf8
    );
    assert_eq!(
        status
            .schema
            .field_by_name("message")
            .unwrap()
            .description
            .as_deref(),
        Some("status message")
    );
    assert!(status.schema.field_by_name("ignored").is_none());

    let armed = status.chunks[0].cols[0]
        .as_any()
        .downcast_ref::<BooleanArray>()
        .unwrap();
    let mode = status.chunks[0].cols[1]
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let message = status.chunks[0].cols[2]
        .as_any()
        .downcast_ref::<LargeStringArray>()
        .unwrap();
    assert_eq!(armed.iter().collect::<Vec<_>>(), [Some(true), None]);
    assert_eq!(mode.iter().collect::<Vec<_>>(), [Some("AUTO"), None]);
    assert_eq!(message.iter().collect::<Vec<_>>(), [None, Some("ready")]);
}

#[test]
fn structured_parquet_empty_window_opens_without_topics_or_picker() {
    let loaded = load_structured_export((9_000, 10_000), "structured-empty-window");

    assert_eq!(loaded.exported_rows, 0);
    assert_eq!(
        loaded.terminal,
        LoadTerminal::Closed(ParseSummary::default())
    );
    assert!(
        loaded.snapshot.topics.is_empty(),
        "manifest-only topics must not be registered without rows"
    );
}

#[test]
fn invalid_marked_parquet_records_removal_instead_of_successful_close() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("structured-invalid-version.parquet");
    let schema = Arc::new(Schema::new_with_metadata(
        vec![Field::new("time", DataType::Int64, true)],
        [
            ("delog.format".to_owned(), "multi-topic".to_owned()),
            ("delog.version".to_owned(), "99".to_owned()),
            (
                "delog.manifest".to_owned(),
                r#"{"version":99,"topics":[]}"#.to_owned(),
            ),
        ]
        .into(),
    ));
    let writer = ArrowWriter::try_new(File::create(&path).unwrap(), schema, None).unwrap();
    writer.close().unwrap();

    let loaded = open_parquet_path(path, 0, Some("parse-setup"));

    assert_eq!(loaded.terminal, LoadTerminal::Removed);
    assert!(loaded.snapshot.topics.is_empty());
    assert!(
        loaded
            .diagnostics
            .iter()
            .any(|record| record.diag.code == "parse-setup")
    );
}

#[test]
fn open_path_loads_a_bin_into_the_store() {
    let path = temp_path("load");
    File::create(&path).unwrap().write_all(&tiny_bin()).unwrap();

    let mut session = Session::new(egui::Context::default());
    session.open_path(path.clone(), None);
    session.join_workers();
    session.wait_until(|s| {
        let snap = s.snapshot();
        snap.topics
            .iter()
            .find(|t| t.entry.name == "TEST")
            .and_then(|t| snap.topic_store(t.entry.id))
            .is_some_and(|store| store.rows == 2)
    });

    let snap = session.snapshot();
    assert!(snap.sources.iter().any(|s| s.entry.label.contains("load")));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_forced_parser_bypasses_sniffing() {
    let path = temp_path("forced-wrong");
    File::create(&path).unwrap().write_all(&tiny_bin()).unwrap();

    let mut session = Session::new(egui::Context::default());
    session.open_path(path.clone(), Some("ulog".to_owned()));
    session.join_workers();
    session.wait_until(|s| !s.diagnostic_records().is_empty());

    let snap = session.snapshot();
    assert!(
        !snap.topics.iter().any(|t| t.entry.name == "TEST"),
        "forcing `ulog` must not fall back to the sniffed ArduPilot parser"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_unknown_forced_parser_name_is_reported() {
    let path = temp_path("forced-missing");
    File::create(&path).unwrap().write_all(&tiny_bin()).unwrap();

    let mut session = Session::new(egui::Context::default());
    session.open_path(path.clone(), Some("nope".to_owned()));
    session.join_workers();
    session.wait_until(|s| {
        s.diagnostic_records()
            .iter()
            .any(|r| r.diag.code == "parser-unknown")
    });

    let _ = std::fs::remove_file(&path);
}

#[test]
fn parser_names_lists_every_registered_parser() {
    let session = Session::new(egui::Context::default());
    assert_eq!(
        session.parser_names(),
        ["ardupilot-bin", "ulog", "tlog", "parquet"]
    );
}

#[test]
fn open_path_loads_a_generic_parquet_into_the_store() {
    let path = temp_parquet_path("generic-load");
    write_generic_parquet(&path);

    let mut session = Session::new(egui::Context::default());
    session.open_path(path.clone(), None);
    session.join_workers();
    session.wait_until(|session| {
        session
            .snapshot()
            .topics
            .iter()
            .filter_map(|topic| topic.store.as_ref())
            .any(|store| store.rows == 2)
    });

    let snapshot = session.snapshot();
    let store = snapshot
        .topics
        .iter()
        .find_map(|topic| topic.store.as_ref())
        .expect("generic Parquet creates a topic store");
    assert_eq!(store.rows, 2);
    assert_eq!(store.schema.name(), source_label(path.as_path()));
    assert_eq!(store.chunks[0].t.values(), &[1_000, 2_000]);
    assert_eq!(store.schema.fields()[0].dtype, DataType::Float32);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_generic_parquet_load_never_reports_an_unsorted_batch() {
    let path = temp_parquet_path("unsorted");
    write_unsorted_generic_parquet(&path);

    let mut session = Session::new(egui::Context::default());
    session.open_path(path.clone(), None);
    session.join_workers();
    session.wait_until(|session| {
        session
            .snapshot()
            .topics
            .iter()
            .filter_map(|topic| topic.store.as_ref())
            .any(|store| store.rows == 4)
    });

    assert!(
        session
            .diagnostic_records()
            .iter()
            .all(|record| record.diag.code != "unsorted-batch"),
        "the parser sorts each batch, so the ingestor must not have to"
    );

    let snapshot = session.snapshot();
    let store = snapshot
        .topics
        .iter()
        .find_map(|topic| topic.store.as_ref())
        .expect("generic Parquet creates a topic store");
    let times = topic_times(store);
    let mut sorted_times = times.clone();
    sorted_times.sort_unstable();
    assert_eq!(
        times, sorted_times,
        "the parser must sort each batch by timestamp before it reaches the store"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn pre_submit_cancellation_after_progress_does_not_contaminate_the_next_load() {
    let invalid_path = temp_parquet_path("invalid-progress-cancel");
    write_all_invalid_parquet(&invalid_path);
    let next_path = temp_path("after-invalid-cancel");
    File::create(&next_path)
        .unwrap()
        .write_all(&tiny_bin())
        .unwrap();
    let mut session = Session::new(egui::Context::default());

    session.open_path(invalid_path.clone(), None);
    let cancel = session.active[0].cancel.clone();
    let loads = Arc::clone(&session.loads);
    let cancel_after_progress = std::thread::spawn(move || {
        for _ in 0..2_000 {
            if !loads.lock().unwrap().is_empty() {
                cancel.cancel();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("all-invalid Parquet parse did not report progress");
    });
    session.join_workers();
    cancel_after_progress.join().unwrap();
    session.wait_until(|session| {
        session.snapshot().sources.iter().any(|source| {
            source.entry.label == source_label(invalid_path.as_path()) && source.entry.removed
        })
    });

    let invalid_source = session
        .snapshot()
        .sources
        .iter()
        .find(|source| source.entry.label == source_label(invalid_path.as_path()))
        .expect("all-invalid Parquet opens a provisional source")
        .entry
        .id;

    let mut late_sink = session.sender.file_sink();
    late_sink.progress(invalid_source, 0.25);
    late_sink.diagnostic(Diag::info(
        "late-progress-sentinel",
        "late progress was processed",
    ));
    session.wait_until(|session| {
        session
            .diagnostic_records()
            .iter()
            .any(|record| record.diag.code == "late-progress-sentinel")
    });

    session.open_path(next_path.clone(), None);
    session.join_workers();
    session.wait_until(|session| {
        let snapshot = session.snapshot();
        let Some(source) = snapshot
            .sources
            .iter()
            .find(|source| source.entry.label == source_label(next_path.as_path()))
        else {
            return false;
        };
        session
            .loads
            .lock()
            .unwrap()
            .get(&source.entry.id)
            .is_some_and(|state| state.done)
    });
    let next_snapshot = session.snapshot();
    assert!(
        next_snapshot
            .topics
            .iter()
            .find(|topic| topic.entry.name == "TEST")
            .and_then(|topic| next_snapshot.topic_store(topic.entry.id))
            .is_some_and(|store| store.rows == 2)
    );

    let invalid_load = session.loads.lock().unwrap()[&invalid_source];
    assert!(invalid_load.done);
    assert_eq!(invalid_load.progress, 1.0);
    assert_eq!(
        session.overall_progress(),
        None,
        "removed-source progress must not contaminate later imports"
    );
    assert!(session.diagnostic_records().iter().all(|record| {
        record.diag.code != "parse-setup" && record.diag.code != "parse-ended"
    }));

    let _ = std::fs::remove_file(&invalid_path);
    let _ = std::fs::remove_file(&next_path);
}

#[test]
fn unknown_format_reports_a_diagnostic_and_no_topics() {
    let path = temp_path("garbage");
    File::create(&path)
        .unwrap()
        .write_all(b"this is not a flight log")
        .unwrap();

    let mut session = Session::new(egui::Context::default());
    session.open_path(path.clone(), None);
    session.join_workers();
    session.wait_until(|s| {
        s.diagnostic_records()
            .iter()
            .any(|record| record.diag.code == "format-unknown")
    });

    let snap = session.snapshot();
    assert!(snap.topics.iter().all(|t| t.store.is_none()));

    let _ = std::fs::remove_file(&path);
}

struct StubSetupParser;

impl LogParser for StubSetupParser {
    fn name(&self) -> &'static str {
        "setup-stub"
    }

    fn sniff(&self, _head: &[u8]) -> Sniff {
        Sniff::new(100, "test")
    }

    fn parse(
        &self,
        _src: Box<dyn ReadSeek>,
        _sink: &mut dyn IngestSink,
        _ctl: &ParseCtl,
    ) -> Result<ParseSummary, ParseError> {
        Err(ParseError::Setup {
            detail: "bad schema".into(),
        })
    }
}

#[test]
fn setup_failure_removes_the_provisional_source() {
    let path = temp_path("setup-failure");
    File::create(&path).unwrap().write_all(b"setup").unwrap();

    let session = Session::new(egui::Context::default());
    let mut registry = ParserRegistry::new();
    registry.register(Arc::new(StubSetupParser));
    run_parse(
        &path,
        "setup-failure",
        &registry,
        &session.sender,
        CancelToken::new(),
        None,
    );
    session.wait_until(|s| {
        let snapshot = s.snapshot();
        snapshot
            .sources
            .iter()
            .any(|source| source.entry.label == "setup-failure" && source.entry.removed)
            && s.diagnostic_records()
                .iter()
                .any(|record| record.diag.code == "parse-setup")
    });

    let snapshot = session.snapshot();
    let source = snapshot
        .sources
        .iter()
        .find(|source| source.entry.label == "setup-failure")
        .expect("setup parser opens a provisional source");
    assert!(source.entry.removed);
    assert!(session.diagnostic_records().iter().any(|record| {
        record.diag.code == "parse-setup" && record.diag.message == "setup-failure: bad schema"
    }));

    let _ = std::fs::remove_file(&path);
}

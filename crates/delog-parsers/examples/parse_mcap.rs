//! Usage: `cargo run -p delog-parsers --example parse_mcap -- path/to/log.mcap`

use std::collections::BTreeMap;
use std::fs::File;
use std::sync::{Arc, Mutex};
use std::thread;

use delog_core::diagnostics::Diag;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, SourceKind, ingest_channel};
use delog_core::ingestor::{IngestObserver, Ingestor};
use delog_core::parse_ctl::{CancelToken, ParseCtl};
use delog_parsers::{LogParser, McapParser, ReadSeek};

/// Collects diagnostics so the run can report them grouped by code.
#[derive(Clone, Default)]
struct SharedObserver {
    diagnostics: Arc<Mutex<Vec<Diag>>>,
}

impl IngestObserver for SharedObserver {
    fn on_diagnostic(&mut self, diag: Diag) {
        self.diagnostics.lock().unwrap().push(diag);
    }

    fn on_close(&mut self, _source: SourceId, _summary: ParseSummary) {}
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: parse_mcap <file.mcap>");
    let file = File::open(&path).expect("open log");
    let total = file.metadata().expect("metadata").len();

    let observer = SharedObserver::default();
    let diagnostics = Arc::clone(&observer.diagnostics);
    let ingestor = Ingestor::new(observer);
    let store = ingestor.store();
    let (tx, rx) = ingest_channel();
    let handle = thread::spawn(move || ingestor.run(rx));

    let mut sink = tx.file_sink();
    let source = sink.open_source(&path, SourceKind::File);
    let ctl = ParseCtl::new(CancelToken::new(), source, total);

    let parser = McapParser;
    let boxed: Box<dyn ReadSeek> = Box::new(file);
    let summary = parser.parse(boxed, &mut sink, &ctl).expect("parse");
    drop(sink);
    drop(tx);
    handle.join().unwrap();

    println!(
        "summary: {} topics, {} rows, {} diagnostics, range {:?}",
        summary.topic_count, summary.row_count, summary.diagnostics, summary.time_range
    );

    let snap = store.load();
    let mut topics: Vec<_> = snap
        .topics
        .iter()
        .filter_map(|t| {
            snap.topic_store(t.entry.id)
                .map(|store| (t.entry.name.clone(), store))
        })
        .collect();
    topics.sort_by(|a, b| a.0.cmp(&b.0));

    println!("\n{} topics (first 40):", topics.len());
    for (name, store) in topics.iter().take(40) {
        let fields: Vec<String> = store
            .schema
            .fields()
            .iter()
            .take(8)
            .map(|f| format!("{}:{:?}", f.name, f.dtype))
            .collect();
        let more = if store.schema.fields().len() > fields.len() {
            format!(", ... +{}", store.schema.fields().len() - fields.len())
        } else {
            String::new()
        };
        println!(
            "  {name:44} {:>8} rows  [{}{}]",
            store.rows,
            fields.join(", "),
            more
        );
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for diag in diagnostics.lock().unwrap().iter() {
        *counts.entry(diag.code).or_default() += 1;
    }
    println!("\ndiagnostics:");
    for (code, count) in counts {
        println!("  {code}: {count}");
    }
}

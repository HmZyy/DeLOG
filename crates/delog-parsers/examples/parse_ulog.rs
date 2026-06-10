//! Dev tool: parse a PX4 `.ulg` and print the resulting topic tree.
//!
//! Usage: `cargo run -p delog-parsers --example parse_ulog -- path/to/log.ulg`

use std::fs::File;
use std::thread;

use delog_core::diagnostics::Diag;
use delog_core::identity::SourceId;
use delog_core::ingest::{IngestSink, ParseSummary, SourceKind, ingest_channel};
use delog_core::ingestor::{IngestObserver, Ingestor};
use delog_core::parse_ctl::{CancelToken, ParseCtl};
use delog_parsers::{LogParser, ReadSeek, ULogParser};

#[derive(Default)]
struct PrintObserver {
    diagnostics: Vec<Diag>,
}

impl IngestObserver for PrintObserver {
    fn on_diagnostic(&mut self, diag: Diag) {
        self.diagnostics.push(diag);
    }

    fn on_close(&mut self, _source: SourceId, _summary: ParseSummary) {}
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: parse_ulog <file.ulg>");
    let file = File::open(&path).expect("open log");
    let total = file.metadata().expect("metadata").len();

    let ingestor = Ingestor::new(PrintObserver::default());
    let store = ingestor.store();
    let (tx, rx) = ingest_channel();
    let handle = thread::spawn(move || ingestor.run(rx));

    let mut sink = tx.file_sink();
    let source = sink.open_source(&path, SourceKind::File);
    let ctl = ParseCtl::new(CancelToken::new(), source, total);

    let parser = ULogParser;
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

    println!("\n{} topics:", topics.len());
    for (name, store) in &topics {
        let range = store.time_range();
        let suspicious = range.is_some_and(|r| r.min_us < 0 || r.max_us > 24 * 60 * 60 * 1_000_000);
        let fields: Vec<String> = store
            .schema
            .fields()
            .iter()
            .take(10)
            .map(|f| format!("{}:{:?}", f.name, f.dtype))
            .collect();
        let more = if store.schema.fields().len() > fields.len() {
            format!(", ... +{}", store.schema.fields().len() - fields.len())
        } else {
            String::new()
        };
        println!(
            "  {name:40} {:>8} rows  {range:?}{}  [{}{}]",
            store.rows,
            if suspicious { "  SUSPICIOUS_TIME" } else { "" },
            fields.join(", "),
            more
        );
    }
}

//! Usage: `cargo run -p delog-parsers --example parse_bin -- path/to/log.BIN`

use std::fs::File;
use std::thread;

use delog_core::ingest::{IngestSink, SourceKind, ingest_channel};
use delog_core::ingestor::{Ingestor, NullObserver};
use delog_core::parse_ctl::{CancelToken, ParseCtl};
use delog_parsers::{ArduPilotParser, LogParser, ReadSeek};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: parse_bin <file.BIN>");
    let file = File::open(&path).expect("open log");
    let total = file.metadata().expect("metadata").len();

    let ingestor = Ingestor::new(NullObserver);
    let store = ingestor.store();
    let (tx, rx) = ingest_channel();
    let handle = thread::spawn(move || ingestor.run(rx));

    let mut sink = tx.file_sink();
    let source = sink.open_source(&path, SourceKind::File);
    let ctl = ParseCtl::new(CancelToken::new(), source, total);

    let parser = ArduPilotParser;
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
        let fields: Vec<String> = store
            .schema
            .fields()
            .iter()
            .map(|f| {
                let unit = f.unit.as_deref().unwrap_or("");
                let mult = if f.multiplier == 1.0 {
                    String::new()
                } else {
                    format!(" x{}", f.multiplier)
                };
                format!(
                    "{}:{:?}{}{}",
                    f.name,
                    f.dtype,
                    if unit.is_empty() { "" } else { "/" },
                    unit
                ) + &mult
            })
            .collect();
        println!(
            "  {name:20} {:>8} rows  [{}]",
            store.rows,
            fields.join(", ")
        );
    }
}

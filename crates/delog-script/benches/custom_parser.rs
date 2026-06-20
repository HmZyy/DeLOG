//! Custom Python parser stage benchmarks (SCR-11).

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use delog_script::custom_parser::{parse_python_result, read_float32_file};
use numpy::IntoPyArray;
use pyo3::prelude::*;
use pyo3::types::PyDict;

const READ_VALUES: usize = 1_048_576;
const PYTHON_ROWS: usize = 16_384;
const PYTHON_FIELDS: u64 = 64;

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TemporaryFile(PathBuf);

impl TemporaryFile {
    fn new() -> Self {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "delog-custom-parser-bench-{}-{id}.bin",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn bench_custom_parser(c: &mut Criterion) {
    let input = TemporaryFile::new();
    let mut bytes = Vec::with_capacity(READ_VALUES * size_of::<f32>());
    for value in 0..READ_VALUES {
        bytes.extend_from_slice(&(value as f32).to_ne_bytes());
    }
    fs::write(input.path(), &bytes).expect("write synthetic parser input");

    let mut group = c.benchmark_group("python_parser");
    group.throughput(Throughput::Bytes(bytes.len() as u64));
    group.bench_function("read_float32", |b| {
        b.iter(|| {
            let raw = read_float32_file(black_box(input.path())).expect("read benchmark input");
            black_box(raw);
        });
    });

    let (parse, raw_data) = Python::attach(|py| {
        let namespace = PyDict::new(py);
        let source = c"import numpy as np
def Parse(raw_data):
    t = np.arange(raw_data.size, dtype=np.float64) * 0.01
    fields = [(\"DATA.rtc\", t, \"time in seconds\")]
    fields.extend(
        (f\"DATA.value_{i}\", raw_data + np.float32(i), \"synthetic value\")
        for i in range(64)
    )
    return fields
";
        py.run(source, Some(&namespace), None)
            .expect("compile benchmark parser");
        let parse = namespace
            .get_item("Parse")
            .expect("look up Parse")
            .expect("benchmark parser defines Parse")
            .unbind();
        let raw_data = vec![1.0_f32; PYTHON_ROWS].into_pyarray(py).unbind();
        (parse, raw_data)
    });

    group.throughput(Throughput::Elements(PYTHON_ROWS as u64 * PYTHON_FIELDS));
    group.bench_function("python_and_convert", |b| {
        Python::attach(|py| {
            b.iter_batched(
                || raw_data.clone_ref(py),
                |raw_data| {
                    let result = parse
                        .bind(py)
                        .call1((raw_data.bind(py),))
                        .expect("call benchmark parser");
                    let output = parse_python_result(py, &result).expect("convert parser result");
                    black_box(output);
                },
                BatchSize::SmallInput,
            );
        });
    });
    group.finish();
}

criterion_group!(benches, bench_custom_parser);
criterion_main!(benches);

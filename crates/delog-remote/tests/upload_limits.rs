use std::sync::Arc;

use arrow::array::{ArrayRef, Date64Array, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use arrow_ipc::writer::StreamWriter;
use axum::body::{Body, HttpBody};
use delog_remote::upload::{UploadLimits, decode_staged_upload, stage_body};
use futures_util::stream;
use tokio_util::sync::CancellationToken;

fn limits() -> UploadLimits {
    UploadLimits {
        max_bytes: 1024 * 1024,
        max_rows: 8,
        max_fields: 4,
    }
}

fn encode(schema: Arc<Schema>, batches: &[RecordBatch]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
        for batch in batches {
            writer.write(batch).unwrap();
        }
        writer.finish().unwrap();
    }
    bytes
}

fn f64_batch(times: Vec<Option<i64>>, values: Vec<f64>) -> (Arc<Schema>, RecordBatch) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Int64, true),
        Field::new("value", DataType::Float64, false),
    ]));
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(times)),
        Arc::new(Float64Array::from(values)),
    ];
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).unwrap();
    (schema, batch)
}

async fn decode(bytes: Vec<u8>, limits: UploadLimits) -> Result<usize, delog_remote::ApiError> {
    let staging = tempfile::tempdir().unwrap();
    let staged = stage_body(
        Body::from(bytes),
        limits,
        staging.path(),
        CancellationToken::new(),
    )
    .await?;
    let prepared = decode_staged_upload(staged, "derived", limits).await?;
    Ok(prepared.topics()[0].rows as usize)
}

#[tokio::test]
async fn valid_upload_preserves_rows_and_deletes_the_staged_file() {
    let (schema, batch) = f64_batch(vec![Some(1_000), Some(2_000)], vec![1.0, 2.0]);
    let staging = tempfile::tempdir().unwrap();
    let staged = stage_body(
        Body::from(encode(schema, &[batch])),
        limits(),
        staging.path(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 1);

    let source = decode_staged_upload(staged, "derived", limits())
        .await
        .unwrap();

    assert_eq!(source.topics()[0].rows, 2);
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn timestamp_column_is_required_int64_non_null_exact_microseconds_and_monotonic() {
    let missing_schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Float64,
        false,
    )]));
    let missing = RecordBatch::try_new(
        Arc::clone(&missing_schema),
        vec![Arc::new(Float64Array::from(vec![1.0]))],
    )
    .unwrap();
    let error = decode(encode(missing_schema, &[missing]), limits())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_input");

    let wrong_schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Float64, false),
        Field::new("value", DataType::Float64, false),
    ]));
    let wrong = RecordBatch::try_new(
        Arc::clone(&wrong_schema),
        vec![
            Arc::new(Float64Array::from(vec![1_000.0])),
            Arc::new(Float64Array::from(vec![1.0])),
        ],
    )
    .unwrap();
    assert_eq!(
        decode(encode(wrong_schema, &[wrong]), limits())
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );

    for times in [vec![None], vec![Some(1_001)]] {
        let (schema, batch) = f64_batch(times, vec![1.0]);
        assert_eq!(
            decode(encode(schema, &[batch]), limits())
                .await
                .unwrap_err()
                .code(),
            "invalid_input"
        );
    }

    let (schema, first) = f64_batch(vec![Some(2_000)], vec![2.0]);
    let (_, second) = f64_batch(vec![Some(1_000)], vec![1.0]);
    assert_eq!(
        decode(encode(schema, &[first, second]), limits())
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );

    let source_schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Int64, false),
        Field::new("__source_time_ns", DataType::Int64, false),
        Field::new("value", DataType::Float64, false),
    ]));
    let mismatched_source = RecordBatch::try_new(
        Arc::clone(&source_schema),
        vec![
            Arc::new(Int64Array::from(vec![1_000])),
            Arc::new(Int64Array::from(vec![2_000])),
            Arc::new(Float64Array::from(vec![1.0])),
        ],
    )
    .unwrap();
    assert_eq!(
        decode(encode(source_schema, &[mismatched_source]), limits())
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );

    let exact_minimum = i64::MIN / 1_000 * 1_000;
    let exact_maximum = i64::MAX / 1_000 * 1_000;
    let (schema, boundary_batch) = f64_batch(
        vec![Some(exact_minimum), Some(exact_maximum)],
        vec![1.0, 2.0],
    );
    assert_eq!(
        decode(encode(schema, &[boundary_batch]), limits())
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn a_second_stream_with_a_changed_schema_is_rejected() {
    let (first_schema, first_batch) = f64_batch(vec![Some(1_000)], vec![1.0]);
    let second_schema = Arc::new(Schema::new(vec![
        Field::new("__delog_time_ns", DataType::Int64, false),
        Field::new("changed", DataType::Int64, false),
    ]));
    let second_batch = RecordBatch::try_new(
        Arc::clone(&second_schema),
        vec![
            Arc::new(Int64Array::from(vec![2_000])),
            Arc::new(Int64Array::from(vec![2])),
        ],
    )
    .unwrap();
    let mut bytes = encode(first_schema, &[first_batch]);
    bytes.extend(encode(second_schema, &[second_batch]));

    let error = decode(bytes, limits()).await.unwrap_err();

    assert_eq!(error.code(), "invalid_input");
}

#[tokio::test]
async fn reserved_columns_unsupported_types_duplicate_fields_and_empty_data_are_rejected() {
    let cases = [
        Arc::new(Schema::new(vec![
            Field::new("__delog_time_ns", DataType::Int64, false),
            Field::new("__delog_private", DataType::Float64, false),
        ])),
        Arc::new(Schema::new(vec![
            Field::new("__delog_time_ns", DataType::Int64, false),
            Field::new("when", DataType::Date64, false),
        ])),
        Arc::new(Schema::new(vec![
            Field::new("__delog_time_ns", DataType::Int64, false),
            Field::new("value", DataType::Float64, false),
            Field::new("value", DataType::Float64, false),
        ])),
    ];
    let columns: [Vec<ArrayRef>; 3] = [
        vec![
            Arc::new(Int64Array::from(vec![1_000])),
            Arc::new(Float64Array::from(vec![1.0])),
        ],
        vec![
            Arc::new(Int64Array::from(vec![1_000])),
            Arc::new(Date64Array::from(vec![1])),
        ],
        vec![
            Arc::new(Int64Array::from(vec![1_000])),
            Arc::new(Float64Array::from(vec![1.0])),
            Arc::new(Float64Array::from(vec![2.0])),
        ],
    ];
    for (schema, columns) in cases.into_iter().zip(columns) {
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns).unwrap();
        assert_eq!(
            decode(encode(schema, &[batch]), limits())
                .await
                .unwrap_err()
                .code(),
            "invalid_input"
        );
    }

    let (schema, empty) = f64_batch(Vec::new(), Vec::new());
    assert_eq!(
        decode(encode(schema, &[empty]), limits())
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );
}

#[tokio::test]
async fn byte_row_and_field_limits_fail_and_staging_files_are_cleaned() {
    let (schema, batch) = f64_batch(vec![Some(1_000), Some(2_000)], vec![1.0, 2.0]);
    let bytes = encode(schema, &[batch]);
    let staging = tempfile::tempdir().unwrap();
    let error = stage_body(
        Body::from(bytes.clone()),
        UploadLimits {
            max_bytes: 8,
            ..limits()
        },
        staging.path(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "invalid_input");
    assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);

    assert_eq!(
        decode(
            bytes,
            UploadLimits {
                max_rows: 1,
                ..limits()
            }
        )
        .await
        .unwrap_err()
        .code(),
        "invalid_input"
    );

    let (schema, batch) = f64_batch(vec![Some(1_000)], vec![1.0]);
    assert_eq!(
        decode(
            encode(schema, &[batch]),
            UploadLimits {
                max_fields: 0,
                ..limits()
            }
        )
        .await
        .unwrap_err()
        .code(),
        "invalid_input"
    );
}

#[tokio::test]
async fn body_error_or_cancellation_cleans_the_partial_staging_file() {
    for body in [
        Body::from_stream(stream::iter([
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"partial")),
            Err(std::io::Error::other("disconnect")),
        ])),
        Body::from_stream(stream::pending::<Result<bytes::Bytes, std::io::Error>>()),
    ] {
        let staging = tempfile::tempdir().unwrap();
        let cancellation = CancellationToken::new();
        if body.size_hint().upper().is_none() {
            cancellation.cancel();
        }
        assert!(
            stage_body(body, limits(), staging.path(), cancellation)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    }
}

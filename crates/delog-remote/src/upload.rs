use std::collections::HashSet;
use std::fs::File as StdFile;
use std::io::Seek;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Int64Array};
use arrow::datatypes::DataType;
use arrow_ipc::reader::StreamReader;
use axum::body::Body;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tempfile::TempPath;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::ApiError;
use crate::arrow_stream::{DELOG_TIME_COLUMN, SOURCE_TIME_COLUMN};
use delog_core::chunk::Chunk;
use delog_core::derived::PreparedDerivedSource;
use delog_core::schema::{FieldSchema, TopicSchema};
use delog_core::store::TopicStore;

const UNIT_META: &str = "delog.unit";
const DESCRIPTION_META: &str = "delog.description";
const MULTIPLIER_META: &str = "delog.multiplier";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadLimits {
    pub max_bytes: u64,
    pub max_rows: u64,
    pub max_fields: usize,
}

impl Default for UploadLimits {
    fn default() -> Self {
        Self {
            max_bytes: 512 * 1024 * 1024,
            max_rows: 10_000_000,
            max_fields: 1024,
        }
    }
}

#[derive(Debug)]
pub struct StagedUpload {
    path: TempPath,
    bytes: u64,
    digest: [u8; 32],
}

impl StagedUpload {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

pub async fn stage_body(
    body: Body,
    limits: UploadLimits,
    staging_dir: &Path,
    cancellation: CancellationToken,
) -> Result<StagedUpload, ApiError> {
    let directory = staging_dir.to_owned();
    let (file, path) = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&directory)?;
        let temporary = tempfile::NamedTempFile::new_in(directory)?;
        Ok::<_, std::io::Error>(temporary.into_parts())
    })
    .await
    .map_err(|_| ApiError::internal("upload staging task failed"))?
    .map_err(|error| {
        ApiError::internal(format!("could not create upload staging file: {error}"))
    })?;

    let mut file = tokio::fs::File::from_std(file);
    let mut stream = body.into_data_stream();
    let mut bytes = 0_u64;
    let mut hasher = Sha256::new();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                return Err(ApiError::unavailable("upload was cancelled"));
            }
            next = stream.next() => next,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|_| ApiError::invalid_input("upload body could not be read"))?;
        bytes = bytes
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| ApiError::invalid_input("upload byte count overflow"))?;
        if bytes > limits.max_bytes {
            return Err(ApiError::invalid_input("upload exceeds the byte limit"));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|error| ApiError::internal(format!("could not stage upload: {error}")))?;
    }
    file.flush()
        .await
        .map_err(|error| ApiError::internal(format!("could not flush staged upload: {error}")))?;
    drop(file);

    Ok(StagedUpload {
        path,
        bytes,
        digest: hasher.finalize().into(),
    })
}

pub async fn decode_staged_upload(
    staged: StagedUpload,
    topic_name: impl Into<String>,
    limits: UploadLimits,
) -> Result<PreparedDerivedSource, ApiError> {
    let topic_name = topic_name.into();
    tokio::task::spawn_blocking(move || decode_staged(staged, topic_name, limits))
        .await
        .map_err(|_| ApiError::internal("upload decoding task failed"))?
}

fn decode_staged(
    staged: StagedUpload,
    topic_name: String,
    limits: UploadLimits,
) -> Result<PreparedDerivedSource, ApiError> {
    let file = StdFile::open(&staged.path)
        .map_err(|error| ApiError::internal(format!("could not open staged upload: {error}")))?;
    let mut position = file
        .try_clone()
        .map_err(|error| ApiError::internal(format!("could not inspect staged upload: {error}")))?;
    let stream_length = file
        .metadata()
        .map_err(|error| ApiError::internal(format!("could not inspect staged upload: {error}")))?
        .len();
    let mut reader = StreamReader::try_new(file, None)
        .map_err(|_| ApiError::invalid_input("upload is not a valid Arrow IPC stream"))?;
    let arrow_schema = reader.schema();
    let delog_time = arrow_schema
        .index_of(DELOG_TIME_COLUMN)
        .map_err(|_| ApiError::invalid_input("__delog_time_ns is required"))?;
    if arrow_schema.field(delog_time).data_type() != &DataType::Int64 {
        return Err(ApiError::invalid_input(
            "__delog_time_ns must have Arrow Int64 type",
        ));
    }
    let source_time = arrow_schema.index_of(SOURCE_TIME_COLUMN).ok();
    if source_time.is_some_and(|index| arrow_schema.field(index).data_type() != &DataType::Int64) {
        return Err(ApiError::invalid_input(
            "__source_time_ns must have Arrow Int64 type",
        ));
    }

    let mut selected = Vec::new();
    let mut fields = Vec::new();
    let mut names = HashSet::new();
    for (index, field) in arrow_schema.fields().iter().enumerate() {
        if index == delog_time || source_time == Some(index) {
            continue;
        }
        if field.name().starts_with("__delog_") {
            return Err(ApiError::invalid_input(
                "only __delog_time_ns is allowed in the reserved __delog_ namespace",
            ));
        }
        if !names.insert(field.name().to_owned()) {
            return Err(ApiError::invalid_input(
                "upload contains duplicate field names",
            ));
        }
        if selected.len() >= limits.max_fields {
            return Err(ApiError::invalid_input("upload exceeds the field limit"));
        }
        let metadata = field.metadata();
        let multiplier = metadata
            .get(MULTIPLIER_META)
            .map(|value| {
                value
                    .parse::<f64>()
                    .map_err(|_| ApiError::invalid_input("invalid delog.multiplier metadata"))
            })
            .transpose()?
            .unwrap_or(1.0);
        let mut schema = FieldSchema::new(
            field.name(),
            field.data_type().clone(),
            metadata.get(UNIT_META).cloned(),
            multiplier,
        )
        .map_err(|error| ApiError::invalid_input(error.to_string()))?;
        if let Some(description) = metadata.get(DESCRIPTION_META) {
            schema = schema.with_description(description);
        }
        selected.push(index);
        fields.push(schema);
    }
    if selected.is_empty() {
        return Err(ApiError::invalid_input(
            "upload must contain at least one data field",
        ));
    }
    let topic_schema = Arc::new(
        TopicSchema::new(topic_name, fields)
            .map_err(|error| ApiError::invalid_input(error.to_string()))?,
    );

    let mut chunks = Vec::new();
    let mut rows = 0_u64;
    let mut previous_time = None;
    for batch in &mut reader {
        let batch = batch.map_err(|_| ApiError::invalid_input("invalid Arrow record batch"))?;
        if batch.schema().as_ref() != arrow_schema.as_ref() {
            return Err(ApiError::invalid_input(
                "Arrow schema changed between record batches",
            ));
        }
        rows = rows
            .checked_add(batch.num_rows() as u64)
            .ok_or_else(|| ApiError::invalid_input("upload row count overflow"))?;
        if rows > limits.max_rows {
            return Err(ApiError::invalid_input("upload exceeds the row limit"));
        }
        if batch.num_rows() == 0 {
            continue;
        }

        let delog_ns = batch
            .column(delog_time)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("schema type was validated");
        let source_ns = source_time.map(|index| {
            batch
                .column(index)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("schema type was validated")
        });
        let mut times_us = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            if delog_ns.is_null(row) || source_ns.is_some_and(|source| source.is_null(row)) {
                return Err(ApiError::invalid_input(
                    "upload timestamps must not be null",
                ));
            }
            let delog = delog_ns.value(row);
            if delog % 1_000 != 0 {
                return Err(ApiError::invalid_input(
                    "upload timestamps must be exact microseconds",
                ));
            }
            if let Some(source) = source_ns
                && source.value(row) != delog
            {
                return Err(ApiError::invalid_input(
                    "__source_time_ns must equal __delog_time_ns for derived uploads",
                ));
            }
            let current = delog
                .checked_div(1_000)
                .ok_or_else(|| ApiError::invalid_input("upload timestamp conversion overflow"))?;
            if previous_time.is_some_and(|previous| current < previous) {
                return Err(ApiError::invalid_input(
                    "upload timestamps must be nondecreasing across batches",
                ));
            }
            previous_time = Some(current);
            times_us.push(current);
        }
        let columns: Vec<ArrayRef> = selected
            .iter()
            .map(|&index| Arc::clone(batch.column(index)))
            .collect();
        let chunk = Chunk::try_new(Int64Array::from(times_us), columns, &topic_schema)
            .map_err(|error| ApiError::invalid_input(error.to_string()))?;
        chunks.push(Arc::new(chunk));
    }
    if rows == 0 {
        return Err(ApiError::invalid_input(
            "upload must contain at least one row",
        ));
    }
    drop(reader);
    let consumed = position
        .stream_position()
        .map_err(|error| ApiError::internal(format!("could not inspect staged upload: {error}")))?;
    if consumed != stream_length {
        return Err(ApiError::invalid_input(
            "upload contains trailing data or a second Arrow schema",
        ));
    }

    let store = TopicStore::from_chunks(topic_schema, chunks)
        .map_err(|error| ApiError::invalid_input(error.to_string()))?;
    PreparedDerivedSource::try_new([Arc::new(store)])
        .map_err(|error| ApiError::invalid_input(error.to_string()))
}

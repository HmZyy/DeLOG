use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use arrow::array::{ArrayBuilder, ArrayRef, Float64Builder, Int64Array, Int64Builder};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::error::ArrowError;
use arrow::record_batch::RecordBatch;
use delog_core::export::{Cell, ExportError, ResampleMode, RowCursor};
use delog_core::identity::{FieldId, SourceId, TopicId};
use delog_core::parse_ctl::CancelToken;
use delog_core::snapshot::StoreSnapshot;

use crate::plotting::browser::BrowserModel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    #[default]
    Csv,
    Parquet,
}

impl ExportFormat {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Parquet => "Parquet",
        }
    }

    pub const fn dialog_title(self) -> &'static str {
        match self {
            Self::Csv => "Export CSV",
            Self::Parquet => "Export Parquet",
        }
    }

    pub const fn default_file_name(self) -> &'static str {
        match self {
            Self::Csv => "export.csv",
            Self::Parquet => "export.parquet",
        }
    }

    pub const fn filter_name(self) -> &'static str {
        self.label()
    }

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Parquet => "parquet",
        }
    }

    pub const fn action_label(self) -> &'static str {
        match self {
            Self::Csv => "Export CSV…",
            Self::Parquet => "Export Parquet…",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportField {
    pub id: FieldId,
    pub source_id: SourceId,
    pub topic_id: TopicId,
    pub source: String,
    pub topic: String,
    pub name: String,
    pub label: String,
    pub dtype: DataType,
    pub unit: Option<String>,
    pub multiplier: f64,
    pub description: Option<String>,
}

impl ExportField {
    pub fn csv_compatible(&self) -> bool {
        matches!(
            self.dtype,
            DataType::Int8
                | DataType::Int16
                | DataType::Int32
                | DataType::Int64
                | DataType::UInt8
                | DataType::UInt16
                | DataType::UInt32
                | DataType::UInt64
                | DataType::Float32
                | DataType::Float64
                | DataType::Boolean
        )
    }

    pub fn parquet_compatible(&self) -> bool {
        self.csv_compatible() || matches!(self.dtype, DataType::Utf8 | DataType::LargeUtf8)
    }
}

pub const EXPORT_BATCH_ROWS: usize = 8_192;

const PROGRESS_REFRESH: std::time::Duration = std::time::Duration::from_millis(100);

/// Export progress in per mille, shared between the writer and the UI.
#[derive(Clone, Default)]
pub struct ExportProgress(Arc<AtomicU32>);

impl ExportProgress {
    pub fn set(&self, fraction: f32) {
        let per_mille = (fraction.clamp(0.0, 1.0) * 1_000.0).round() as u32;
        self.0.store(per_mille, Ordering::Relaxed);
    }

    fn per_mille(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Cancellation plus a progress callback for one export run.
pub struct ExportCtl {
    cancel: CancelToken,
    progress: Box<dyn Fn(f32) + Send + Sync>,
}

impl ExportCtl {
    pub fn new(cancel: CancelToken, progress: impl Fn(f32) + Send + Sync + 'static) -> Self {
        Self {
            cancel,
            progress: Box::new(progress),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) fn report_fraction(&self, fraction: f32) {
        (self.progress)(fraction);
    }
}

impl Default for ExportCtl {
    fn default() -> Self {
        Self::new(CancelToken::new(), |_| {})
    }
}

/// How far `t_us` sits through the exported window. Rows leave the store in
/// timestamp order, so this is the only progress measure both formats share
/// without counting the output rows up front.
pub(crate) fn window_fraction(t_us: i64, window: (i64, i64)) -> f32 {
    let span = i128::from(window.1) - i128::from(window.0);
    if span <= 0 {
        return 1.0;
    }
    let done = i128::from(t_us) - i128::from(window.0);
    (done as f64 / span as f64).clamp(0.0, 1.0) as f32
}

/// An export that is writing right now, as shown by [`progress_ui`].
pub struct ActiveExport {
    pub id: u64,
    label: String,
    progress: ExportProgress,
    cancel: CancelToken,
}

impl ActiveExport {
    pub fn new(id: u64, path: &Path, progress: ExportProgress, cancel: CancelToken) -> Self {
        Self {
            id,
            label: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            progress,
            cancel,
        }
    }

    pub fn fraction(&self) -> f32 {
        self.progress.per_mille() as f32 / 1_000.0
    }

    pub fn status(&self) -> String {
        if self.cancel.is_cancelled() {
            format!("{} - cancelling…", self.label)
        } else {
            format!("{} - {}%", self.label, self.progress.per_mille() / 10)
        }
    }

    pub fn request_cancel(&self) {
        self.cancel.cancel();
    }
}

#[derive(Debug)]
pub enum DataExportError {
    Export(ExportError),
    Arrow(ArrowError),
    Parquet(parquet::errors::ParquetError),
    ParquetFormat(delog_parquet_format::FormatError),
    InvalidSelection(String),
    TimestampOverflow {
        source: String,
        timestamp_us: i64,
        offset_us: i64,
    },
    Cancelled,
    Io(std::io::Error),
}

impl std::fmt::Display for DataExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Export(error) => write!(f, "{error}"),
            Self::Arrow(error) => write!(f, "{error}"),
            Self::Parquet(error) => write!(f, "{error}"),
            Self::ParquetFormat(error) => write!(f, "{error}"),
            Self::InvalidSelection(message) => write!(f, "invalid export selection: {message}"),
            Self::TimestampOverflow {
                source,
                timestamp_us,
                offset_us,
            } => write!(
                f,
                "timestamp overflow for source {source}: {timestamp_us} + {offset_us}"
            ),
            Self::Cancelled => write!(f, "export cancelled"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DataExportError {}

impl From<ExportError> for DataExportError {
    fn from(error: ExportError) -> Self {
        Self::Export(error)
    }
}

impl From<ArrowError> for DataExportError {
    fn from(error: ArrowError) -> Self {
        Self::Arrow(error)
    }
}

impl From<parquet::errors::ParquetError> for DataExportError {
    fn from(error: parquet::errors::ParquetError) -> Self {
        Self::Parquet(error)
    }
}

impl From<delog_parquet_format::FormatError> for DataExportError {
    fn from(error: delog_parquet_format::FormatError) -> Self {
        Self::ParquetFormat(error)
    }
}

impl From<std::io::Error> for DataExportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn column_name(field: &ExportField) -> String {
    match field.unit.as_deref().filter(|unit| !unit.is_empty()) {
        Some(unit) => format!("{} [{unit}]", field.label),
        None => field.label.clone(),
    }
}

pub fn export_schema(fields: &[ExportField]) -> SchemaRef {
    let mut schema_fields = vec![
        Field::new("t_us", DataType::Int64, false),
        Field::new("t_s", DataType::Float64, false),
    ];
    schema_fields.extend(fields.iter().map(|field| {
        let mut metadata = HashMap::new();
        if let Some(unit) = field.unit.as_deref().filter(|unit| !unit.is_empty()) {
            metadata.insert("unit".to_owned(), unit.to_owned());
        }
        Field::new(column_name(field), DataType::Float64, true).with_metadata(metadata)
    }));
    Arc::new(Schema::new(schema_fields))
}

pub struct ExportBatchReader<'a> {
    cursor: RowCursor<'a>,
    schema: SchemaRef,
    origin_us: i64,
    field_count: usize,
    finished: bool,
}

impl<'a> ExportBatchReader<'a> {
    pub fn try_new(
        snapshot: &'a StoreSnapshot,
        fields: &[ExportField],
        window: (i64, i64),
        mode: ResampleMode,
        origin_us: i64,
    ) -> Result<Self, DataExportError> {
        let ids = fields.iter().map(|field| field.id).collect::<Vec<_>>();
        Ok(Self {
            cursor: RowCursor::new(snapshot, &ids, window.0, window.1, mode)?,
            schema: export_schema(fields),
            origin_us,
            field_count: fields.len(),
            finished: false,
        })
    }

    pub fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }
}

impl Iterator for ExportBatchReader<'_> {
    type Item = Result<RecordBatch, DataExportError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let mut times = Int64Builder::with_capacity(EXPORT_BATCH_ROWS);
        let mut seconds = Float64Builder::with_capacity(EXPORT_BATCH_ROWS);
        let mut values = (0..self.field_count)
            .map(|_| Float64Builder::with_capacity(EXPORT_BATCH_ROWS))
            .collect::<Vec<_>>();
        let mut row = Vec::with_capacity(self.field_count);

        for _ in 0..EXPORT_BATCH_ROWS {
            let Some(t_us) = self.cursor.next_row(&mut row) else {
                self.finished = true;
                break;
            };
            times.append_value(t_us);
            seconds.append_value((t_us - self.origin_us) as f64 * 1e-6);
            for (builder, cell) in values.iter_mut().zip(&row) {
                match cell {
                    Cell::Num(value) => builder.append_value(*value),
                    Cell::Empty => builder.append_null(),
                }
            }
        }

        if times.len() == 0 {
            return None;
        }
        let mut columns: Vec<ArrayRef> = vec![Arc::new(times.finish()), Arc::new(seconds.finish())];
        columns.extend(
            values
                .into_iter()
                .map(|mut builder| Arc::new(builder.finish()) as ArrayRef),
        );
        Some(RecordBatch::try_new(Arc::clone(&self.schema), columns).map_err(Into::into))
    }
}

pub struct DataExportRequest {
    pub format: ExportFormat,
    pub fields: Vec<FieldId>,
    pub window: (i64, i64),
    pub mode: ResampleMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingExportField {
    id: FieldId,
}

impl std::fmt::Display for MissingExportField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "selected export field {} is no longer available",
            self.id.0
        )
    }
}

pub fn resolve_export_fields(
    selected: &[FieldId],
    available: &[ExportField],
) -> Result<Vec<ExportField>, MissingExportField> {
    resolve_export_field_refs(selected, available)
        .map(|fields| fields.into_iter().cloned().collect())
}

fn resolve_export_field_refs<'a>(
    selected: &[FieldId],
    available: &'a [ExportField],
) -> Result<Vec<&'a ExportField>, MissingExportField> {
    selected
        .iter()
        .map(|id| {
            available
                .iter()
                .find(|field| field.id == *id)
                .ok_or(MissingExportField { id: *id })
        })
        .collect()
}

type FieldPickerRects = (egui::Rect, egui::Rect);

fn field_picker_ui(
    ui: &mut egui::Ui,
    state: &mut DataExportState,
    available: &[ExportField],
) -> FieldPickerRects {
    let mut add_one = None;
    let mut add_source = None;
    let mut remove_one = None;
    let mut add_filtered = false;
    let mut clear = false;
    let picker_width = (ui.available_width() - ui.spacing().item_spacing.x * 3.0) * 0.5;
    let pane_size = egui::vec2(picker_width, ui.available_height());

    let rects = ui
        .horizontal_top(|ui| {
            let available_rect = ui
                .allocate_ui_with_layout(
                    pane_size,
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.heading("Available fields");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut state.search)
                                    .hint_text("Search fields…"),
                            );
                            add_filtered = ui.button("Add filtered").clicked();
                        });
                        egui::ScrollArea::vertical()
                            .id_salt("data_export_available_fields")
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .auto_shrink([false, false])
                            .max_height(ui.available_height())
                            .show(ui, |ui| {
                                let mut previous_source = None::<&str>;
                                let mut previous_topic = None::<&str>;
                                for field in available.iter().filter(|field| {
                                    state.format_compatible(field) && state.matches(field)
                                }) {
                                    if previous_source != Some(field.source.as_str()) {
                                        let source_fully_selected = available
                                            .iter()
                                            .filter(|candidate| {
                                                candidate.source == field.source
                                                    && state.format_compatible(candidate)
                                            })
                                            .all(|candidate| {
                                                state.selected.contains(&candidate.id)
                                            });
                                        ui.horizontal(|ui| {
                                            ui.strong(&field.source);
                                            if ui
                                                .add_enabled(
                                                    !source_fully_selected,
                                                    egui::Button::new("Add all"),
                                                )
                                                .clicked()
                                            {
                                                add_source = Some(field.source.clone());
                                            }
                                        });
                                        previous_source = Some(&field.source);
                                        previous_topic = None;
                                    }
                                    if previous_topic != Some(field.topic.as_str()) {
                                        ui.label(egui::RichText::new(&field.topic).strong());
                                        previous_topic = Some(&field.topic);
                                    }
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        ui.label(&field.name);
                                        if let Some(unit) =
                                            field.unit.as_deref().filter(|unit| !unit.is_empty())
                                        {
                                            ui.weak(format!("[{unit}]"));
                                        }
                                        let already_selected = state.selected.contains(&field.id);
                                        if ui
                                            .add_enabled_ui(!already_selected, |ui| {
                                                ui.add_sized([24.0, 24.0], egui::Button::new("+"))
                                            })
                                            .inner
                                            .clicked()
                                        {
                                            add_one = Some(field.id);
                                        }
                                    });
                                }
                            })
                            .inner_rect
                    },
                )
                .inner;

            ui.separator();

            let selected_rect = ui
                .allocate_ui_with_layout(
                    pane_size,
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.heading(format!("Selected fields ({})", state.selected.len()));
                            if ui.button("Clear").clicked() {
                                clear = true;
                            }
                        });
                        egui::ScrollArea::vertical()
                            .id_salt("data_export_selected_fields")
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .auto_shrink([false, false])
                            .max_height(ui.available_height())
                            .show(ui, |ui| match state.selected_fields(available) {
                                Ok(fields) => {
                                    for field in fields {
                                        ui.horizontal(|ui| {
                                            ui.label(column_name(field));
                                            if ui.small_button("×").clicked() {
                                                remove_one = Some(field.id);
                                            }
                                        });
                                    }
                                }
                                Err(error) => {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        error.to_string(),
                                    );
                                }
                            })
                            .inner_rect
                    },
                )
                .inner;

            (available_rect, selected_rect)
        })
        .inner;

    if add_filtered {
        state.add_filtered(available);
    }
    if let Some(id) = add_one {
        state.add(id);
    }
    if let Some(source) = add_source {
        state.add_source_fields(&source, available);
    }
    if let Some(id) = remove_one {
        state.remove(id);
    }
    if clear {
        state.clear();
    }

    rects
}

/// `visible` is the current ViewX (min,max); `full` is the global range.
pub fn dialog_ui(
    ctx: &egui::Context,
    state: &mut DataExportState,
    available: &[ExportField],
    visible: (i64, i64),
    full: (i64, i64),
) -> Option<DataExportRequest> {
    let mut request = None;
    let mut open = state.open;
    egui::Window::new("Export Data")
        .open(&mut open)
        .collapsible(false)
        .resizable([true, true])
        .default_width(760.0)
        .default_height(440.0)
        .min_height(300.0)
        .show(ctx, |ui| {
            egui::Panel::top("data_export_controls")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Format:");
                        let mut format = state.format;
                        ui.radio_value(&mut format, ExportFormat::Csv, "CSV");
                        ui.radio_value(&mut format, ExportFormat::Parquet, "Parquet");
                        if format != state.format {
                            state.set_format(format, available);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Range:");
                        ui.radio_value(&mut state.visible_range, true, "Visible window");
                        ui.radio_value(&mut state.visible_range, false, "Full");
                    });
                    ui.horizontal(|ui| {
                        ui.label("Resample:");
                        ui.add_enabled_ui(state.format == ExportFormat::Csv, |ui| {
                            egui::ComboBox::from_id_salt("data_export_mode")
                                .selected_text(MODES[state.mode_ix])
                                .show_ui(ui, |ui| {
                                    for (index, mode) in MODES.iter().enumerate() {
                                        ui.selectable_value(&mut state.mode_ix, index, *mode);
                                    }
                                });
                            if state.mode_ix == 2 {
                                ui.label("dt (s):");
                                ui.add(
                                    egui::DragValue::new(&mut state.dt_s)
                                        .speed(0.001)
                                        .range(1e-4..=3600.0),
                                );
                            }
                        });
                        if state.format == ExportFormat::Parquet {
                            ui.label("Native samples per topic");
                        }
                    });
                    ui.separator();
                });

            egui::Panel::bottom("data_export_actions")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            state.open = false;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(
                                    !state.selected.is_empty(),
                                    egui::Button::new(state.format.action_label()),
                                )
                                .clicked()
                            {
                                request = state.request(visible, full);
                            }
                        });
                    });
                });

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| {
                    let _ = field_picker_ui(ui, state, available);
                });
        });
    state.open = open && state.open;
    request
}

pub fn progress_ui(ctx: &egui::Context, active: &[ActiveExport]) {
    if active.is_empty() {
        return;
    }

    egui::Window::new("Exporting data")
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center())
        .pivot(egui::Align2::CENTER_CENTER)
        .show(ctx, |ui| {
            for active in active {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::ProgressBar::new(active.fraction())
                            .desired_width(260.0)
                            .text(active.status()),
                    );
                    if ui.button("Cancel").clicked() {
                        active.request_cancel();
                    }
                });
            }
        });
    ctx.request_repaint_after(PROGRESS_REFRESH);
}

pub const MODES: [&str; 3] = ["None (union)", "Previous-fill", "Linear @ dt"];

pub struct DataExportState {
    pub open: bool,
    pub search: String,
    pub selected: Vec<FieldId>,
    pub format: ExportFormat,
    pub visible_range: bool,
    pub mode_ix: usize,
    pub dt_s: f64,
}

impl Default for DataExportState {
    fn default() -> Self {
        Self {
            open: false,
            search: String::new(),
            selected: Vec::new(),
            format: ExportFormat::Csv,
            visible_range: true,
            mode_ix: 0,
            dt_s: 0.0,
        }
    }
}

impl DataExportState {
    pub fn open(&mut self) {
        *self = Self {
            open: true,
            ..Self::default()
        };
    }

    pub fn add(&mut self, id: FieldId) {
        if !self.selected.contains(&id) {
            self.selected.push(id);
        }
    }

    pub fn add_filtered(&mut self, available: &[ExportField]) {
        let ids = available
            .iter()
            .filter(|field| self.format_compatible(field) && self.matches(field))
            .map(|field| field.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.add(id);
        }
    }

    pub fn add_source_fields(&mut self, source: &str, available: &[ExportField]) {
        let ids = available
            .iter()
            .filter(|field| field.source == source && self.format_compatible(field))
            .map(|field| field.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.add(id);
        }
    }

    pub fn remove(&mut self, id: FieldId) {
        self.selected.retain(|selected| *selected != id);
    }

    pub fn clear(&mut self) {
        self.selected.clear();
    }

    pub fn set_format(&mut self, format: ExportFormat, available: &[ExportField]) {
        self.format = format;
        self.selected.retain(|id| {
            available
                .iter()
                .find(|field| field.id == *id)
                .is_some_and(|field| match format {
                    ExportFormat::Csv => field.csv_compatible(),
                    ExportFormat::Parquet => field.parquet_compatible(),
                })
        });
    }

    pub fn request(&self, visible: (i64, i64), full: (i64, i64)) -> Option<DataExportRequest> {
        if self.selected.is_empty() {
            return None;
        }
        Some(DataExportRequest {
            format: self.format,
            fields: self.selected.clone(),
            window: if self.visible_range { visible } else { full },
            mode: match self.format {
                ExportFormat::Csv => self.mode(),
                ExportFormat::Parquet => ResampleMode::None,
            },
        })
    }

    pub fn selected_fields<'a>(
        &self,
        available: &'a [ExportField],
    ) -> Result<Vec<&'a ExportField>, MissingExportField> {
        resolve_export_field_refs(&self.selected, available)
    }

    fn matches(&self, field: &ExportField) -> bool {
        let needle = self.search.trim().to_lowercase();
        needle.is_empty() || field.label.to_lowercase().contains(&needle)
    }

    fn format_compatible(&self, field: &ExportField) -> bool {
        match self.format {
            ExportFormat::Csv => field.csv_compatible(),
            ExportFormat::Parquet => field.parquet_compatible(),
        }
    }

    pub fn mode(&self) -> ResampleMode {
        match self.mode_ix {
            1 => ResampleMode::PrevFill,
            2 => ResampleMode::Linear {
                dt_us: ((self.dt_s.max(1e-6)) * 1e6) as i64,
            },
            _ => ResampleMode::None,
        }
    }
}

pub fn available_fields(snapshot: &StoreSnapshot, model: &BrowserModel) -> Vec<ExportField> {
    let mut out = Vec::new();
    for src in &model.sources {
        for topic in &src.topics {
            let Some(store) = snapshot.topic_store(topic.id) else {
                continue;
            };
            for sf in store.schema.fields() {
                let Some(field) = topic.fields.iter().find(|field| field.name == sf.name) else {
                    continue;
                };
                let export_field = ExportField {
                    id: field.id,
                    source_id: src.id,
                    topic_id: topic.id,
                    source: src.label.clone(),
                    topic: topic.name.clone(),
                    name: field.name.clone(),
                    label: format!("{} / {}.{}", src.label, topic.name, field.name),
                    dtype: sf.dtype.clone(),
                    unit: sf.unit.clone(),
                    multiplier: sf.multiplier,
                    description: sf.description.clone(),
                };
                if export_field.parquet_compatible() {
                    out.push(export_field);
                }
            }
        }
    }
    out
}

fn write_csv<W: Write + Send>(
    mut writer: W,
    mut batches: ExportBatchReader<'_>,
    window: (i64, i64),
    ctl: &ExportCtl,
) -> Result<u64, DataExportError> {
    let schema = batches.schema();
    let mut rows = 0_u64;
    let mut csv = arrow::csv::WriterBuilder::new()
        .with_header(true)
        .build(&mut writer);
    let mut wrote_batch = false;
    for batch in &mut batches {
        if ctl.is_cancelled() {
            return Err(DataExportError::Cancelled);
        }
        let batch = batch?;
        rows += batch.num_rows() as u64;
        let times = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("the export schema starts with an Int64 t_us column");
        let last_t_us = times.value(times.len() - 1);
        csv.write(&batch)?;
        wrote_batch = true;
        ctl.report_fraction(window_fraction(last_t_us, window));
    }
    if !wrote_batch {
        csv.write(&RecordBatch::new_empty(schema))?;
    }
    drop(csv);
    writer.flush()?;
    ctl.report_fraction(1.0);
    Ok(rows)
}

fn write_atomic<T>(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> Result<T, DataExportError>,
) -> Result<T, DataExportError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let value = write(temporary.as_file_mut())?;
    temporary
        .persist(path)
        .map_err(|error| DataExportError::Io(error.error))?;
    Ok(value)
}

#[allow(clippy::too_many_arguments)]
pub fn write_export_file(
    path: &Path,
    format: ExportFormat,
    snapshot: &StoreSnapshot,
    fields: &[ExportField],
    window: (i64, i64),
    mode: ResampleMode,
    origin_us: i64,
    ctl: &ExportCtl,
) -> Result<u64, DataExportError> {
    if ctl.is_cancelled() {
        return Err(DataExportError::Cancelled);
    }
    write_atomic(path, |temporary| match format {
        ExportFormat::Csv => {
            let batches = ExportBatchReader::try_new(snapshot, fields, window, mode, origin_us)?;
            write_csv(BufWriter::new(temporary), batches, window, ctl)
        }
        ExportFormat::Parquet => crate::parquet_export::write_structured_parquet(
            BufWriter::new(temporary),
            snapshot,
            fields,
            window,
            ctl,
        ),
    })
}

#[cfg(test)]
mod tests;

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

use crate::browser::BrowserModel;

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
            format!("{} — cancelling…", self.label)
        } else {
            format!("{} — {}%", self.label, self.progress.per_mille() / 10)
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
mod tests {
    use std::sync::Arc;

    use super::*;
    use arrow::array::{
        Array, BooleanArray, Float32Array, Float64Array, Int64Array, LargeStringArray, StringArray,
    };
    use arrow::datatypes::DataType;
    use delog_core::chunk::Chunk;
    use delog_core::identity::{IdentityRegistry, SourceId};
    use delog_core::parse_ctl::CancelToken;
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::store::TopicStore;

    fn snapshot_with_values(
        timestamps: Vec<i64>,
        values: Vec<Option<f64>>,
    ) -> (StoreSnapshot, ExportField) {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("flight");
        let topic = registry.add_topic(source, "ATT").unwrap();
        let id = registry.add_field(topic, "Roll").unwrap();
        let topic_schema = Arc::new(
            TopicSchema::new(
                "ATT",
                vec![FieldSchema {
                    name: "Roll".into(),
                    dtype: DataType::Float64,
                    unit: Some("rad".into()),
                    multiplier: 2.0,
                    description: None,
                }],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(timestamps),
                vec![Arc::new(Float64Array::from(values))],
                &topic_schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(topic_schema, vec![chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&registry, [(topic, store)], 1).unwrap();
        (
            snapshot,
            ExportField {
                id,
                source_id: source,
                topic_id: topic,
                source: "flight".into(),
                topic: "ATT".into(),
                name: "Roll".into(),
                label: "flight / ATT.Roll".into(),
                dtype: DataType::Float64,
                unit: Some("rad".into()),
                multiplier: 2.0,
                description: None,
            },
        )
    }

    fn snapshot_with_nonmonotonic_chunks() -> (StoreSnapshot, ExportField) {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("flight");
        let topic = registry.add_topic(source, "ATT").unwrap();
        let id = registry.add_field(topic, "Roll").unwrap();
        let topic_schema = Arc::new(
            TopicSchema::new(
                "ATT",
                [FieldSchema::new("Roll", DataType::Float64, Some("rad"), 2.0).unwrap()],
            )
            .unwrap(),
        );
        let chunks = [
            (vec![100, 200], vec![Some(1.0), Some(2.0)]),
            (vec![50, 300], vec![Some(3.0), Some(4.0)]),
        ]
        .map(|(timestamps, values)| {
            Arc::new(
                Chunk::try_new(
                    Int64Array::from(timestamps),
                    vec![Arc::new(Float64Array::from(values))],
                    &topic_schema,
                )
                .unwrap(),
            )
        });
        let store = Arc::new(TopicStore::from_chunks(topic_schema, chunks).unwrap());
        assert!(!store.is_monotonic());
        let snapshot = StoreSnapshot::from_registry(&registry, [(topic, store)], 1).unwrap();
        (
            snapshot,
            ExportField {
                id,
                source_id: source,
                topic_id: topic,
                source: "flight".into(),
                topic: "ATT".into(),
                name: "Roll".into(),
                label: "flight / ATT.Roll".into(),
                dtype: DataType::Float64,
                unit: Some("rad".into()),
                multiplier: 2.0,
                description: None,
            },
        )
    }

    fn snapshot_with_staggered_fields() -> (StoreSnapshot, Vec<ExportField>) {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("flight");
        let topic = registry.add_topic(source, "ATT").unwrap();
        let roll_id = registry.add_field(topic, "Roll").unwrap();
        let pitch_id = registry.add_field(topic, "Pitch").unwrap();
        let topic_schema = Arc::new(
            TopicSchema::new(
                "ATT",
                vec![
                    FieldSchema {
                        name: "Roll".into(),
                        dtype: DataType::Float64,
                        unit: Some("rad".into()),
                        multiplier: 1.0,
                        description: None,
                    },
                    FieldSchema {
                        name: "Pitch".into(),
                        dtype: DataType::Float64,
                        unit: Some("rad".into()),
                        multiplier: 1.0,
                        description: None,
                    },
                ],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10, 20]),
                vec![
                    Arc::new(Float64Array::from(vec![Some(1.0), None])),
                    Arc::new(Float64Array::from(vec![None, Some(2.0)])),
                ],
                &topic_schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(topic_schema, vec![chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&registry, [(topic, store)], 1).unwrap();
        let field = |id, name: &str| ExportField {
            id,
            source_id: source,
            topic_id: topic,
            source: "flight".into(),
            topic: "ATT".into(),
            name: name.into(),
            label: format!("flight / ATT.{name}"),
            dtype: DataType::Float64,
            unit: Some("rad".into()),
            multiplier: 1.0,
            description: None,
        };
        (
            snapshot,
            vec![field(roll_id, "Roll"), field(pitch_id, "Pitch")],
        )
    }

    fn snapshot_with_supported_fields() -> (StoreSnapshot, BrowserModel) {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("flight");
        let topic = registry.add_topic(source, "STATUS").unwrap();
        for name in ["value", "armed", "mode", "message"] {
            registry.add_field(topic, name).unwrap();
        }
        let topic_schema = Arc::new(
            TopicSchema::new(
                "STATUS",
                vec![
                    FieldSchema::new("value", DataType::Float32, Some("m"), 2.0)
                        .unwrap()
                        .with_description("scaled value"),
                    FieldSchema::new("armed", DataType::Boolean, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("mode", DataType::Utf8, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("message", DataType::LargeUtf8, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let chunk = Arc::new(
            Chunk::try_new(
                Int64Array::from(vec![10]),
                vec![
                    Arc::new(Float32Array::from(vec![Some(1.5)])),
                    Arc::new(BooleanArray::from(vec![Some(true)])),
                    Arc::new(StringArray::from(vec![Some("AUTO")])),
                    Arc::new(LargeStringArray::from(vec![Some("ready")])),
                ],
                &topic_schema,
            )
            .unwrap(),
        );
        let store = Arc::new(TopicStore::from_chunks(topic_schema, vec![chunk]).unwrap());
        let snapshot = StoreSnapshot::from_registry(&registry, [(topic, store)], 1).unwrap();
        let model = BrowserModel::from_snapshot(&snapshot);
        (snapshot, model)
    }

    #[test]
    fn shared_schema_has_time_types_nullable_values_and_unit_metadata() {
        let (_, field) = snapshot_with_values(vec![1], vec![Some(1.0)]);
        let schema = export_schema(&[field]);
        assert_eq!(schema.field(0).name(), "t_us");
        assert_eq!(schema.field(0).data_type(), &DataType::Int64);
        assert!(!schema.field(0).is_nullable());
        assert_eq!(schema.field(1).name(), "t_s");
        assert_eq!(schema.field(1).data_type(), &DataType::Float64);
        assert!(!schema.field(1).is_nullable());
        assert_eq!(schema.field(2).name(), "flight / ATT.Roll [rad]");
        assert_eq!(schema.field(2).data_type(), &DataType::Float64);
        assert!(schema.field(2).is_nullable());
        assert_eq!(
            schema.field(2).metadata().get("unit").map(String::as_str),
            Some("rad")
        );
    }

    #[test]
    fn shared_batches_preserve_rows_nulls_origin_and_multiplier() {
        let (snapshot, field) = snapshot_with_values(
            vec![1_000_000, 2_000_000, 3_000_000],
            vec![Some(1.5), None, Some(-2.0)],
        );
        let batches = ExportBatchReader::try_new(
            &snapshot,
            std::slice::from_ref(&field),
            (1_000_000, 3_000_000),
            ResampleMode::None,
            1_000_000,
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

        assert_eq!(batches.len(), 1);
        let batch = &batches[0];
        assert_eq!(batch.num_rows(), 2);
        let times = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let seconds = batch
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let values = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(times.values(), &[1_000_000, 3_000_000]);
        assert_eq!(seconds.values(), &[0.0, 2.0]);
        assert_eq!(values.values(), &[3.0, -4.0]);
    }

    #[test]
    fn shared_batches_use_nulls_for_missing_union_cells() {
        let (snapshot, fields) = snapshot_with_staggered_fields();
        let batch =
            ExportBatchReader::try_new(&snapshot, &fields, (10, 20), ResampleMode::None, 10)
                .unwrap()
                .next()
                .unwrap()
                .unwrap();
        let roll = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let pitch = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(roll.value(0), 1.0);
        assert!(roll.is_null(1));
        assert!(pitch.is_null(0));
        assert_eq!(pitch.value(1), 2.0);
    }

    #[test]
    fn shared_batches_are_bounded_to_8192_rows() {
        let timestamps = (0..8_193).map(i64::from).collect::<Vec<_>>();
        let values = (0..8_193)
            .map(|value| Some(value as f64))
            .collect::<Vec<_>>();
        let (snapshot, field) = snapshot_with_values(timestamps, values);
        let sizes =
            ExportBatchReader::try_new(&snapshot, &[field], (0, 8_192), ResampleMode::None, 0)
                .unwrap()
                .map(|batch| batch.unwrap().num_rows())
                .collect::<Vec<_>>();
        assert_eq!(sizes, vec![8_192, 1]);
    }

    #[test]
    fn csv_writer_preserves_header_and_precision() {
        let (snapshot, field) =
            snapshot_with_values(vec![1_000, 2_000], vec![Some(1.25), Some(-3.0)]);
        let batches =
            ExportBatchReader::try_new(&snapshot, &[field], (1_000, 2_000), ResampleMode::None, 0)
                .unwrap();
        let mut output = Vec::new();
        let rows = write_csv(&mut output, batches, (1_000, 2_000), &ExportCtl::default()).unwrap();
        assert_eq!(rows, 2);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "t_us,t_s,flight / ATT.Roll [rad]\n1000,0.001,2.5\n2000,0.002,-6.0\n"
        );
    }

    #[test]
    fn csv_writer_quotes_headers_and_blanks_missing_union_cells() {
        let (snapshot, mut fields) = snapshot_with_staggered_fields();
        fields[0].label = "flight,one / ATT.Roll".into();
        let batches =
            ExportBatchReader::try_new(&snapshot, &fields, (10, 20), ResampleMode::None, 10)
                .unwrap();
        let mut output = Vec::new();
        let rows = write_csv(&mut output, batches, (10, 20), &ExportCtl::default()).unwrap();
        assert_eq!(rows, 2);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "t_us,t_s,\"flight,one / ATT.Roll [rad]\",flight / ATT.Pitch [rad]\n10,0.0,1.0,\n20,9.999999999999999e-6,,2.0\n"
        );
    }

    #[test]
    fn parquet_route_writes_structured_raw_values_and_manifest_metadata() {
        use delog_parquet_format::decode_schema;
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

        let (snapshot, field) =
            snapshot_with_values(vec![1_000_000, 2_000_000], vec![Some(1.25), Some(-3.0)]);
        let file = tempfile::NamedTempFile::new().unwrap();
        let rows = write_export_file(
            file.path(),
            ExportFormat::Parquet,
            &snapshot,
            std::slice::from_ref(&field),
            (1_000_000, 2_000_000),
            ResampleMode::None,
            1_000_000,
            &ExportCtl::default(),
        )
        .unwrap();
        assert_eq!(rows, 2);

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        let manifest = decode_schema(builder.schema().as_ref()).unwrap().unwrap();
        assert_eq!(manifest.topics.len(), 1);
        assert_eq!(manifest.topics[0].original_source, "flight");
        assert_eq!(manifest.topics[0].original_topic, "ATT");
        assert_eq!(manifest.topics[0].fields[0].name, "Roll");
        assert_eq!(manifest.topics[0].fields[0].unit.as_deref(), Some("rad"));
        assert_eq!(manifest.topics[0].fields[0].multiplier, 2.0);
        let batch = builder.build().unwrap().next().unwrap().unwrap();
        let values = batch
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(values.values(), &[1.25, -3.0]);
    }

    #[derive(Default)]
    struct WriteFailure;

    impl std::io::Write for WriteFailure {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected write failure"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FlushFailure {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl std::io::Write for FlushFailure {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            if self.flushes == 1 {
                Ok(())
            } else {
                Err(std::io::Error::other("injected final flush failure"))
            }
        }
    }

    #[test]
    fn writer_failure_is_returned() {
        let (snapshot, field) = snapshot_with_values(vec![1], vec![Some(1.0)]);
        let batches =
            ExportBatchReader::try_new(&snapshot, &[field], (1, 1), ResampleMode::None, 0).unwrap();

        let error = write_csv(WriteFailure, batches, (1, 1), &ExportCtl::default()).unwrap_err();

        assert!(error.to_string().contains("injected write failure"));
    }

    #[test]
    fn final_flush_failure_is_returned() {
        let (snapshot, field) = snapshot_with_values(vec![1], vec![Some(1.0)]);
        let batches =
            ExportBatchReader::try_new(&snapshot, &[field], (1, 1), ResampleMode::None, 0).unwrap();

        let error = write_csv(
            FlushFailure::default(),
            batches,
            (1, 1),
            &ExportCtl::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("injected final flush failure"));
    }

    #[test]
    fn failed_export_preserves_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.parquet");
        std::fs::write(&path, b"prior export").unwrap();
        let error = write_atomic(&path, |temporary| -> Result<(), DataExportError> {
            std::io::Write::write_all(temporary, b"partial replacement")?;
            Err(std::io::Error::other("injected export failure").into())
        })
        .unwrap_err();

        assert!(error.to_string().contains("injected export failure"));
        assert_eq!(std::fs::read(&path).unwrap(), b"prior export");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    struct FinalFlushFailingFile<'a> {
        inner: &'a mut std::fs::File,
    }

    impl std::io::Write for FinalFlushFailingFile<'_> {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.inner.write(buffer)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other(
                "injected structured final flush failure",
            ))
        }
    }

    #[test]
    fn parquet_final_flush_failure_preserves_existing_destination() {
        let (snapshot, field) = snapshot_with_values(vec![1], vec![Some(1.0)]);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.parquet");
        std::fs::write(&path, b"prior export").unwrap();

        let error = write_atomic(&path, |temporary| {
            crate::parquet_export::write_structured_parquet(
                FinalFlushFailingFile { inner: temporary },
                &snapshot,
                &[field],
                (1, 1),
                &ExportCtl::default(),
            )
        })
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injected structured final flush failure")
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"prior export");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn nonmonotonic_parquet_export_preserves_existing_destination() {
        let (snapshot, field) = snapshot_with_nonmonotonic_chunks();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.parquet");
        std::fs::write(&path, b"prior export").unwrap();

        let error = write_export_file(
            &path,
            ExportFormat::Parquet,
            &snapshot,
            &[field],
            (0, 300),
            ResampleMode::None,
            0,
            &ExportCtl::default(),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("topic timestamps are non-monotonic across chunks")
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"prior export");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn successful_export_replaces_existing_destination() {
        let (snapshot, field) = snapshot_with_values(vec![1], vec![Some(1.0)]);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.csv");
        std::fs::write(&path, b"prior export").unwrap();

        let rows = write_export_file(
            &path,
            ExportFormat::Csv,
            &snapshot,
            &[field],
            (1, 1),
            ResampleMode::None,
            0,
            &ExportCtl::default(),
        )
        .unwrap();

        assert_eq!(rows, 1);
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "t_us,t_s,flight / ATT.Roll [rad]\n1,1e-6,2.0\n"
        );
    }

    #[test]
    fn parquet_row_groups_are_bounded_to_export_batch_rows() {
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

        let timestamps = (0..=EXPORT_BATCH_ROWS as i64).collect::<Vec<_>>();
        let values = timestamps
            .iter()
            .map(|value| Some(*value as f64))
            .collect::<Vec<_>>();
        let (snapshot, field) = snapshot_with_values(timestamps, values);
        let file = tempfile::NamedTempFile::new().unwrap();

        write_export_file(
            file.path(),
            ExportFormat::Parquet,
            &snapshot,
            &[field],
            (0, EXPORT_BATCH_ROWS as i64),
            ResampleMode::None,
            0,
            &ExportCtl::default(),
        )
        .unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(file.path()).unwrap())
                .unwrap();
        let row_group_sizes = builder
            .metadata()
            .row_groups()
            .iter()
            .map(|group| group.num_rows() as usize)
            .collect::<Vec<_>>();
        assert_eq!(row_group_sizes, vec![EXPORT_BATCH_ROWS, 1]);
    }

    fn long_export() -> (StoreSnapshot, ExportField) {
        let timestamps = (0..=EXPORT_BATCH_ROWS as i64).collect::<Vec<_>>();
        let values = timestamps
            .iter()
            .map(|value| Some(*value as f64))
            .collect::<Vec<_>>();
        snapshot_with_values(timestamps, values)
    }

    const STRIPED_EXPORT_ROWS: usize = EXPORT_BATCH_ROWS * 3;

    /// A topic long enough to need three stripes, beside a topic whose only row
    /// sits at the end of the window.
    fn snapshot_with_long_and_late_topics() -> (StoreSnapshot, Vec<ExportField>) {
        let mut registry = IdentityRegistry::new();
        let source = registry.add_source("flight");
        let mut stores = Vec::new();
        let mut fields = Vec::new();
        let last_us = STRIPED_EXPORT_ROWS as i64 - 1;
        for (topic_name, timestamps) in [
            ("LONG", (0..=last_us).collect::<Vec<_>>()),
            ("LATE", vec![last_us]),
        ] {
            let topic = registry.add_topic(source, topic_name).unwrap();
            let id = registry.add_field(topic, "value").unwrap();
            let schema = Arc::new(
                TopicSchema::new(
                    topic_name,
                    [FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap()],
                )
                .unwrap(),
            );
            let values = timestamps
                .iter()
                .map(|value| Some(*value as f64))
                .collect::<Vec<_>>();
            let chunk = Arc::new(
                Chunk::try_new(
                    Int64Array::from(timestamps),
                    vec![Arc::new(Float64Array::from(values))],
                    &schema,
                )
                .unwrap(),
            );
            stores.push((
                topic,
                Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
            ));
            fields.push(ExportField {
                id,
                source_id: source,
                topic_id: topic,
                source: "flight".into(),
                topic: topic_name.into(),
                name: "value".into(),
                label: format!("flight / {topic_name}.value"),
                dtype: DataType::Float64,
                unit: None,
                multiplier: 1.0,
                description: None,
            });
        }
        let snapshot = StoreSnapshot::from_registry(&registry, stores, 1).unwrap();
        (snapshot, fields)
    }

    fn recording_ctl() -> (ExportCtl, Arc<std::sync::Mutex<Vec<f32>>>) {
        let reports = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&reports);
        let ctl = ExportCtl::new(CancelToken::new(), move |fraction| {
            sink.lock().unwrap().push(fraction)
        });
        (ctl, reports)
    }

    fn assert_monotone_completing_fractions(reports: &Arc<std::sync::Mutex<Vec<f32>>>) {
        let reports = reports.lock().unwrap();
        assert!(reports.len() >= 2, "progress is reported while writing");
        assert!(
            reports.windows(2).all(|pair| pair[0] <= pair[1]),
            "progress never goes backwards: {reports:?}"
        );
        assert!(
            reports[0] > 0.0 && reports[0] < 1.0,
            "the first batch is partial: {}",
            reports[0]
        );
        assert_eq!(*reports.last().unwrap(), 1.0);
    }

    fn self_cancelling_ctl() -> ExportCtl {
        let cancel = CancelToken::new();
        let trigger = cancel.clone();
        ExportCtl::new(cancel, move |_| trigger.cancel())
    }

    #[test]
    fn csv_export_reports_window_progress_per_batch() {
        let (snapshot, field) = long_export();
        let file = tempfile::NamedTempFile::new().unwrap();
        let (ctl, reports) = recording_ctl();

        let rows = write_export_file(
            file.path(),
            ExportFormat::Csv,
            &snapshot,
            &[field],
            (0, EXPORT_BATCH_ROWS as i64),
            ResampleMode::None,
            0,
            &ctl,
        )
        .unwrap();

        assert_eq!(rows, EXPORT_BATCH_ROWS as u64 + 1);
        assert_monotone_completing_fractions(&reports);
    }

    #[test]
    fn parquet_export_reports_window_progress_per_stripe() {
        let (snapshot, field) = long_export();
        let file = tempfile::NamedTempFile::new().unwrap();
        let (ctl, reports) = recording_ctl();

        let rows = write_export_file(
            file.path(),
            ExportFormat::Parquet,
            &snapshot,
            &[field],
            (0, EXPORT_BATCH_ROWS as i64),
            ResampleMode::None,
            0,
            &ctl,
        )
        .unwrap();

        assert_eq!(rows, EXPORT_BATCH_ROWS as u64 + 1);
        assert_monotone_completing_fractions(&reports);
    }

    #[test]
    fn export_cancelled_before_the_first_batch_creates_no_destination() {
        let (snapshot, field) = snapshot_with_values(vec![1], vec![Some(1.0)]);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.csv");
        let cancel = CancelToken::new();
        cancel.cancel();

        let error = write_export_file(
            &path,
            ExportFormat::Csv,
            &snapshot,
            &[field],
            (1, 1),
            ResampleMode::None,
            0,
            &ExportCtl::new(cancel, |_| {}),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "export cancelled");
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn cancelling_mid_csv_export_preserves_the_existing_destination() {
        let (snapshot, field) = long_export();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.csv");
        std::fs::write(&path, b"prior export").unwrap();

        let error = write_export_file(
            &path,
            ExportFormat::Csv,
            &snapshot,
            &[field],
            (0, EXPORT_BATCH_ROWS as i64),
            ResampleMode::None,
            0,
            &self_cancelling_ctl(),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "export cancelled");
        assert_eq!(std::fs::read(&path).unwrap(), b"prior export");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn cancelling_mid_parquet_export_preserves_the_existing_destination() {
        let (snapshot, field) = long_export();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.parquet");
        std::fs::write(&path, b"prior export").unwrap();

        let error = write_export_file(
            &path,
            ExportFormat::Parquet,
            &snapshot,
            &[field],
            (0, EXPORT_BATCH_ROWS as i64),
            ResampleMode::None,
            0,
            &self_cancelling_ctl(),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "export cancelled");
        assert_eq!(std::fs::read(&path).unwrap(), b"prior export");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn parquet_progress_tracks_the_slowest_topic_instead_of_jumping_to_full() {
        let (snapshot, fields) = snapshot_with_long_and_late_topics();
        let file = tempfile::NamedTempFile::new().unwrap();
        let (ctl, reports) = recording_ctl();

        write_export_file(
            file.path(),
            ExportFormat::Parquet,
            &snapshot,
            &fields,
            (0, STRIPED_EXPORT_ROWS as i64 - 1),
            ResampleMode::None,
            0,
            &ctl,
        )
        .unwrap();

        let reports = reports.lock().unwrap();
        assert!(
            reports.windows(2).all(|pair| pair[0] <= pair[1]),
            "a finished short topic must not pull progress back: {reports:?}"
        );
        assert!(
            reports[0] < 0.5,
            "the first stripe covers a third of the long topic: {reports:?}"
        );
        assert_eq!(*reports.last().unwrap(), 1.0);
    }

    #[test]
    fn export_progress_completes_when_the_data_ends_before_the_window() {
        for format in [ExportFormat::Csv, ExportFormat::Parquet] {
            let (snapshot, field) = long_export();
            let file = tempfile::NamedTempFile::new().unwrap();
            let (ctl, reports) = recording_ctl();

            write_export_file(
                file.path(),
                format,
                &snapshot,
                &[field],
                (0, 100_000),
                ResampleMode::None,
                0,
                &ctl,
            )
            .unwrap();

            let reports = reports.lock().unwrap();
            assert!(
                reports[reports.len() - 2] < 0.2,
                "{format:?} tracks the data, not the window: {reports:?}"
            );
            assert_eq!(*reports.last().unwrap(), 1.0, "{format:?} finishes at 100%");
        }
    }

    #[test]
    fn window_fraction_maps_timestamps_into_the_exported_window() {
        assert_eq!(window_fraction(0, (0, 100)), 0.0);
        assert_eq!(window_fraction(25, (0, 100)), 0.25);
        assert_eq!(window_fraction(100, (0, 100)), 1.0);
        assert_eq!(window_fraction(-50, (-100, 100)), 0.25);
        assert_eq!(window_fraction(-5, (0, 100)), 0.0);
        assert_eq!(window_fraction(150, (0, 100)), 1.0);
        assert_eq!(window_fraction(5, (5, 5)), 1.0);
        assert_eq!(window_fraction(i64::MAX, (i64::MIN, i64::MAX)), 1.0);
    }

    #[test]
    fn active_export_shows_a_percentage_then_cancellation() {
        let progress = ExportProgress::default();
        progress.set(0.456);
        let active = ActiveExport::new(
            7,
            std::path::Path::new("/tmp/flight.parquet"),
            progress,
            CancelToken::new(),
        );

        assert_eq!(active.status(), "flight.parquet — 45%");
        assert!((active.fraction() - 0.456).abs() < 0.001);

        active.request_cancel();

        assert_eq!(active.status(), "flight.parquet — cancelling…");
    }

    #[test]
    fn default_range_is_visible_window() {
        assert!(DataExportState::default().visible_range);
    }

    #[test]
    fn available_fields_retain_every_supported_field_schema() {
        let (snapshot, model) = snapshot_with_supported_fields();

        let available = available_fields(&snapshot, &model);

        assert_eq!(
            available
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["value", "armed", "mode", "message"],
        );
        assert!(
            available
                .iter()
                .find(|field| field.name == "mode")
                .unwrap()
                .parquet_compatible()
        );
        assert!(
            !available
                .iter()
                .find(|field| field.name == "mode")
                .unwrap()
                .csv_compatible()
        );
        let value = available
            .iter()
            .find(|field| field.name == "value")
            .unwrap();
        assert_eq!(value.source_id, model.sources[0].id);
        assert_eq!(value.topic_id, model.sources[0].topics[0].id);
        assert_eq!(value.dtype, DataType::Float32);
        assert_eq!(value.multiplier, 2.0);
        assert_eq!(value.description.as_deref(), Some("scaled value"));
    }

    #[test]
    fn switching_from_parquet_to_csv_prunes_only_incompatible_string_selections() {
        let (snapshot, model) = snapshot_with_supported_fields();
        let available = available_fields(&snapshot, &model);
        let mut state = DataExportState::default();
        state.set_format(ExportFormat::Parquet, &available);
        state.selected = available.iter().map(|field| field.id).collect();
        let compatible_ids = available
            .iter()
            .filter(|field| field.csv_compatible())
            .map(|field| field.id)
            .collect::<Vec<_>>();

        state.set_format(ExportFormat::Csv, &available);

        assert_eq!(state.selected, compatible_ids);
        let selected_names = state
            .selected
            .iter()
            .map(|id| {
                available
                    .iter()
                    .find(|field| field.id == *id)
                    .unwrap()
                    .name
                    .as_str()
            })
            .collect::<Vec<_>>();
        assert_eq!(selected_names, ["value", "armed"]);
        assert!(!selected_names.contains(&"mode"));
        assert!(!selected_names.contains(&"message"));
    }

    #[test]
    fn parquet_request_forces_native_samples_after_linear_csv_state() {
        let state = DataExportState {
            format: ExportFormat::Parquet,
            selected: vec![FieldId(1)],
            mode_ix: 2,
            dt_s: 0.5,
            ..DataExportState::default()
        };

        let request = state.request((10, 20), (0, 100)).unwrap();

        assert_eq!(request.mode, ResampleMode::None);
    }

    fn export_field(id: u32, label: &str) -> ExportField {
        ExportField {
            id: delog_core::identity::FieldId(id),
            source_id: SourceId(0),
            topic_id: TopicId(0),
            source: "flight".into(),
            topic: "ATT".into(),
            name: label.into(),
            label: format!("flight / ATT.{label}"),
            dtype: DataType::Float64,
            unit: Some("rad".into()),
            multiplier: 1.0,
            description: None,
        }
    }

    #[test]
    fn format_helpers_are_consistent() {
        assert_eq!(ExportFormat::Csv.label(), "CSV");
        assert_eq!(ExportFormat::Csv.dialog_title(), "Export CSV");
        assert_eq!(ExportFormat::Csv.default_file_name(), "export.csv");
        assert_eq!(ExportFormat::Csv.filter_name(), "CSV");
        assert_eq!(ExportFormat::Csv.extension(), "csv");
        assert_eq!(ExportFormat::Csv.action_label(), "Export CSV…");

        assert_eq!(ExportFormat::Parquet.label(), "Parquet");
        assert_eq!(ExportFormat::Parquet.dialog_title(), "Export Parquet");
        assert_eq!(ExportFormat::Parquet.default_file_name(), "export.parquet");
        assert_eq!(ExportFormat::Parquet.filter_name(), "Parquet");
        assert_eq!(ExportFormat::Parquet.extension(), "parquet");
        assert_eq!(ExportFormat::Parquet.action_label(), "Export Parquet…");
    }

    #[test]
    fn opening_resets_format_search_and_selection() {
        let mut state = DataExportState {
            format: ExportFormat::Parquet,
            search: "roll".into(),
            ..DataExportState::default()
        };
        state.add(delog_core::identity::FieldId(7));

        state.open();

        assert!(state.open);
        assert_eq!(state.format, ExportFormat::Csv);
        assert!(state.search.is_empty());
        assert!(state.selected.is_empty());
        assert!(state.visible_range);
        assert_eq!(state.mode_ix, 0);
    }

    #[test]
    fn request_uses_radio_format_selection_order_and_visible_window() {
        let fields = [export_field(1, "Roll"), export_field(2, "Pitch")];
        let mut state = DataExportState {
            format: ExportFormat::Parquet,
            visible_range: true,
            ..DataExportState::default()
        };
        state.add(fields[1].id);
        state.add(fields[0].id);

        let request = state.request((10, 20), (0, 100)).unwrap();

        assert_eq!(request.format, ExportFormat::Parquet);
        assert_eq!(request.fields, vec![fields[1].id, fields[0].id]);
        assert_eq!(request.window, (10, 20));
        assert_eq!(request.mode, ResampleMode::None);
    }

    #[test]
    fn request_is_disabled_without_selected_fields() {
        assert!(
            DataExportState::default()
                .request((10, 20), (0, 100))
                .is_none()
        );
    }

    fn run_dialog_frame(
        ctx: &egui::Context,
        state: &mut DataExportState,
        available: &[ExportField],
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_200.0, 600.0),
            )),
            events,
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            let _ = dialog_ui(ui.ctx(), state, available, (0, 1), (0, 1));
        })
    }

    fn find_text_rect(shape: &egui::epaint::Shape, expected: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == expected => {
                Some(text.visual_bounding_rect())
            }
            egui::epaint::Shape::Vec(shapes) => shapes
                .iter()
                .find_map(|shape| find_text_rect(shape, expected)),
            _ => None,
        }
    }

    #[test]
    fn export_dialog_keeps_compact_height_and_footer_inside_window() {
        let ctx = egui::Context::default();
        let mut state = DataExportState::default();
        state.open();
        let fields = (0..200)
            .map(|id| export_field(id, &format!("Field {id}")))
            .collect::<Vec<_>>();
        for field in fields.iter().take(20) {
            state.add(field.id);
        }

        let _ = run_dialog_frame(&ctx, &mut state, &fields, vec![]);
        let _ = run_dialog_frame(&ctx, &mut state, &fields, vec![]);
        let initial_window = ctx
            .memory(|memory| memory.area_rect(egui::Id::new("Export Data")))
            .expect("export window should have a persisted area");
        let resize_start = initial_window.right_bottom() - egui::vec2(2.0, 2.0);
        let resize_end = resize_start - egui::vec2(0.0, 140.0);

        let _ = run_dialog_frame(
            &ctx,
            &mut state,
            &fields,
            vec![
                egui::Event::PointerMoved(resize_start),
                egui::Event::PointerButton {
                    pos: resize_start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let _ = run_dialog_frame(
            &ctx,
            &mut state,
            &fields,
            vec![egui::Event::PointerMoved(resize_end)],
        );
        let _ = run_dialog_frame(
            &ctx,
            &mut state,
            &fields,
            vec![egui::Event::PointerButton {
                pos: resize_end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let output = run_dialog_frame(&ctx, &mut state, &fields, vec![]);
        let resized_window = ctx
            .memory(|memory| memory.area_rect(egui::Id::new("Export Data")))
            .expect("export window should retain its area after resizing");
        let export_text = output
            .shapes
            .iter()
            .find_map(|shape| find_text_rect(&shape.shape, "Export CSV…"))
            .expect("export action should be painted");

        assert!(
            initial_window.height() < 540.0,
            "window consumed the viewport height: {initial_window:?}"
        );
        assert!(
            resized_window.height() < initial_window.height() - 80.0,
            "window rebounded from {initial_window:?} to {resized_window:?}"
        );
        assert!(resized_window.contains_rect(export_text));
    }

    #[test]
    fn field_picker_scroll_viewports_fill_allocated_height() {
        let ctx = egui::Context::default();
        let fields = vec![export_field(1, "Roll")];
        let mut state = DataExportState::default();
        let mut rects = None;

        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 500.0),
                )),
                ..Default::default()
            },
            |ui| {
                rects = Some(
                    ui.allocate_ui_with_layout(
                        egui::vec2(760.0, 400.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| field_picker_ui(ui, &mut state, &fields),
                    )
                    .inner,
                );
            },
        );

        let rects = rects.expect("picker should report both viewports");
        assert!(rects.0.height() > 300.0);
        assert!(rects.1.height() > 300.0);
    }

    #[test]
    fn selection_is_ordered_unique_removable_and_clearable() {
        let fields = vec![export_field(1, "Roll"), export_field(2, "Pitch")];
        let mut state = DataExportState::default();
        state.add(fields[1].id);
        state.add(fields[0].id);
        state.add(fields[1].id);
        assert_eq!(state.selected, vec![fields[1].id, fields[0].id]);
        assert_eq!(
            state
                .selected_fields(&fields)
                .unwrap()
                .iter()
                .map(|field| field.id)
                .collect::<Vec<_>>(),
            vec![fields[1].id, fields[0].id]
        );

        state.remove(fields[1].id);
        assert_eq!(state.selected, vec![fields[0].id]);
        state.clear();
        assert!(state.selected.is_empty());
    }

    #[test]
    fn exact_field_resolution_preserves_requested_order_and_cardinality() {
        let fields = vec![export_field(1, "Roll"), export_field(2, "Pitch")];

        let resolved = resolve_export_fields(&[fields[1].id, fields[0].id], &fields).unwrap();

        assert_eq!(
            resolved.iter().map(|field| field.id).collect::<Vec<_>>(),
            vec![fields[1].id, fields[0].id]
        );
    }

    #[test]
    fn exact_field_resolution_rejects_mixed_valid_and_stale_ids() {
        let fields = vec![export_field(1, "Roll"), export_field(2, "Pitch")];
        let stale = delog_core::identity::FieldId(99);

        let error =
            resolve_export_fields(&[fields[0].id, stale, fields[1].id], &fields).unwrap_err();

        assert_eq!(error.id, stale);
    }

    #[test]
    fn add_filtered_uses_available_field_order() {
        let fields = vec![
            export_field(1, "Roll"),
            export_field(2, "Pitch"),
            export_field(3, "RollRate"),
        ];
        let mut state = DataExportState {
            search: "roll".into(),
            ..DataExportState::default()
        };

        state.add_filtered(&fields);

        assert_eq!(state.selected, vec![fields[0].id, fields[2].id]);
    }

    #[test]
    fn add_source_fields_adds_the_whole_log_once_in_available_order() {
        let mut flight_roll = export_field(1, "Roll");
        flight_roll.source = "flight".into();
        let mut telemetry_altitude = export_field(2, "Altitude");
        telemetry_altitude.source = "telemetry".into();
        let mut flight_pitch = export_field(3, "Pitch");
        flight_pitch.source = "flight".into();
        let fields = vec![flight_roll, telemetry_altitude, flight_pitch];
        let mut state = DataExportState::default();
        state.add(fields[2].id);

        state.add_source_fields("flight", &fields);
        state.add_source_fields("flight", &fields);

        assert_eq!(state.selected, vec![fields[2].id, fields[0].id]);
    }
}

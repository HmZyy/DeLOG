use arrow::datatypes::DataType;
use delog_cache::{CacheManager, GapBehavior, TraceCache, TraceGeometry};
use delog_core::identity::{FieldId, SourceId, SourceKind, TopicId};
use delog_core::snapshot::StoreSnapshot;
use delog_core::time::TimeRange;
use delog_render::palette;
use std::sync::Arc;

use crate::plotting::axes;
use crate::plotting::compare::CompareMode;
use crate::ui::fuzzy::fuzzy_match_score;
use crate::plotting::gpu::{self, GpuBridge, PreparedYRange, SyncTrace};
use crate::plotting::plot::{ViewX, draw_zoom_drag_overlay};
use crate::sync::sync_alignment::{
    AlignmentError, AnchorKind, SampleNeighborhood, SyncSample, anchor, sample_neighborhood,
    target_offset_us,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetParseError {
    Syntax,
    Number,
    Unit,
    NonFinite,
    FractionalMicrosecond,
    Overflow,
}

pub fn parse_offset_us(text: &str) -> Result<i64, OffsetParseError> {
    let mut tokens = text.split_whitespace();
    let number = tokens.next().ok_or(OffsetParseError::Syntax)?;
    let unit = tokens.next().ok_or(OffsetParseError::Unit)?;
    if tokens.next().is_some() {
        return Err(OffsetParseError::Syntax);
    }

    let value: f64 = number.parse().map_err(|_| OffsetParseError::Number)?;
    if !value.is_finite() {
        return Err(OffsetParseError::NonFinite);
    }
    let multiplier_us = match unit {
        "us" => 1_i64,
        "ms" => 1_000,
        "s" => 1_000_000,
        _ => return Err(OffsetParseError::Unit),
    };
    // Preserve the full i64 domain for integer spellings; f64 cannot distinguish
    // i64::MAX from the next (out-of-range) integer.
    if let Ok(integer) = number.parse::<i64>() {
        return integer
            .checked_mul(multiplier_us)
            .ok_or(OffsetParseError::Overflow);
    }
    let multiplier = multiplier_us as f64;
    let scaled = value * multiplier;
    if !scaled.is_finite() {
        return Err(OffsetParseError::Overflow);
    }
    if scaled.fract() != 0.0 {
        return Err(OffsetParseError::FractionalMicrosecond);
    }
    // i64::MAX rounds up to 2^63 as f64, so the positive bound is exclusive.
    const I64_MIN_F64: f64 = -9_223_372_036_854_775_808.0;
    const I64_MAX_EXCLUSIVE_F64: f64 = 9_223_372_036_854_775_808.0;
    if scaled < I64_MIN_F64 || scaled >= I64_MAX_EXCLUSIVE_F64 {
        return Err(OffsetParseError::Overflow);
    }
    Ok(scaled as i64)
}

pub fn format_offset_us(value: i64) -> String {
    if value % 1_000_000 == 0 {
        format!("{} s", value / 1_000_000)
    } else if value % 1_000 == 0 {
        format!("{} ms", value / 1_000)
    } else {
        format!("{value} us")
    }
}

pub fn drag_delta_us(delta_px: f32, plot_width_px: f32, span_us: i64) -> Option<i64> {
    if !delta_px.is_finite() || !plot_width_px.is_finite() || plot_width_px < 2.0 || span_us <= 0 {
        return None;
    }
    let delta = f64::from(delta_px) / f64::from(plot_width_px) * span_us as f64;
    const I64_MIN_F64: f64 = -9_223_372_036_854_775_808.0;
    const I64_MAX_EXCLUSIVE_F64: f64 = 9_223_372_036_854_775_808.0;
    (delta.is_finite() && delta >= I64_MIN_F64 && delta < I64_MAX_EXCLUSIVE_F64)
        .then(|| delta as i64)
}

const SYNC_PLOT_MAX_HEIGHT: f32 = 360.0;
const SYNC_FOOTER_RESERVE: f32 = 36.0;

fn sync_plot_height(available_height: f32) -> f32 {
    (available_height - SYNC_FOOTER_RESERVE).clamp(1.0, SYNC_PLOT_MAX_HEIGHT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetMathError;

pub fn preview_delta_us(
    draft_offset_us: i64,
    current_offset_us: i64,
) -> Result<i64, OffsetMathError> {
    draft_offset_us
        .checked_sub(current_offset_us)
        .ok_or(OffsetMathError)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoAlignMethod {
    FirstToFirst,
    LastToLast,
    BackToBack,
    FirstChange,
}

fn anchor_kinds(method: AutoAlignMethod) -> (AnchorKind, AnchorKind) {
    match method {
        AutoAlignMethod::FirstToFirst => (AnchorKind::First, AnchorKind::First),
        AutoAlignMethod::LastToLast => (AnchorKind::Last, AnchorKind::Last),
        AutoAlignMethod::BackToBack => (AnchorKind::Last, AnchorKind::First),
        AutoAlignMethod::FirstChange => (AnchorKind::FirstChange, AnchorKind::FirstChange),
    }
}

fn alignment_error_text(error: AlignmentError) -> &'static str {
    match error {
        AlignmentError::FieldUnavailable => "Select a numeric or Boolean field for both sources",
        AlignmentError::NoFiniteSamples => "A selected field has no finite samples",
        AlignmentError::NoChange => "A selected field never changes value",
        AlignmentError::Overflow => "The aligned offset is outside the supported time range",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickError {
    PairUnavailable,
    UnexpectedSource,
    Alignment(AlignmentError),
}

#[derive(Debug, Clone, Copy)]
enum PickStage {
    Reference,
    Target { reference_sample: SyncSample },
}

#[derive(Debug, Clone, Copy)]
struct PickSession {
    reference: SourceId,
    target: SourceId,
    reference_field: FieldId,
    target_field: FieldId,
    stage: PickStage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyBlock {
    Clean,
    InvalidInput,
    Conflict,
    InsufficientSources,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceControls {
    pub movable: bool,
}

#[derive(Debug, Default)]
pub struct SyncWindowResponse {
    pub apply: Option<Vec<(SourceId, i64)>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct OverlayHitSegment {
    trace_index: usize,
    a: egui::Pos2,
    b: egui::Pos2,
}

impl OverlayHitSegment {
    fn new(trace_index: usize, a: egui::Pos2, b: egui::Pos2) -> Self {
        Self { trace_index, a, b }
    }
}

fn nearest_overlay_trace(
    pointer: egui::Pos2,
    traces: &[OverlayHitSegment],
    threshold_px: f32,
) -> Option<usize> {
    let max_distance_sq = threshold_px.max(0.0).powi(2);
    traces
        .iter()
        .filter_map(|trace| {
            let ab = trace.b - trace.a;
            let length_sq = ab.length_sq();
            let t = if length_sq > 0.0 {
                ((pointer - trace.a).dot(ab) / length_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let distance_sq = pointer.distance_sq(trace.a + ab * t);
            (distance_sq <= max_distance_sq).then_some((trace.trace_index, distance_sq))
        })
        .min_by(
            |(left_index, left_distance), (right_index, right_distance)| {
                left_distance
                    .total_cmp(right_distance)
                    .then_with(|| left_index.cmp(right_index))
            },
        )
        .map(|(index, _)| index)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ProjectedSample {
    source: SourceId,
    sample: SyncSample,
    position: egui::Pos2,
}

#[derive(Debug, Clone, Copy)]
struct ProjectedNeighborhood {
    previous: Option<ProjectedSample>,
    current: ProjectedSample,
    next: Option<ProjectedSample>,
}

#[derive(Debug, Clone, Copy)]
struct RenderedSyncTrace {
    source: SourceId,
    trace: SyncTrace,
}

fn prepared_y_ranges(
    mode: CompareMode,
    raw_ranges: &[Option<PreparedYRange>],
) -> Vec<PreparedYRange> {
    let Some(common_origin) = raw_ranges.iter().flatten().next().map(|range| range.origin) else {
        return vec![
            PreparedYRange::new(0.0, -1.0, 1.0).expect("fallback range is valid");
            raw_ranges.len()
        ];
    };
    if raw_ranges.is_empty() {
        return Vec::new();
    }
    match mode {
        CompareMode::Overlay => {
            let union = raw_ranges
                .iter()
                .flatten()
                .filter_map(|range| range.relative_to(common_origin))
                .fold(
                    PreparedYRange {
                        origin: common_origin,
                        min: f64::INFINITY,
                        max: f64::NEG_INFINITY,
                    },
                    |union, range| PreparedYRange {
                        min: union.min.min(range.min),
                        max: union.max.max(range.max),
                        ..union
                    },
                );
            let padded = union.padded();
            vec![padded; raw_ranges.len()]
        }
        CompareMode::Stacked => raw_ranges
            .iter()
            .map(|range| {
                range.map_or_else(
                    || PreparedYRange::new(0.0, -1.0, 1.0).expect("fallback range is valid"),
                    PreparedYRange::padded,
                )
            })
            .collect(),
    }
}

fn preparation_needs_repaint(building: impl IntoIterator<Item = bool>) -> bool {
    building.into_iter().any(|building| building)
}

fn prepare_rendered_geometry(
    candidates: &[RenderedSyncTrace],
    caches: &mut CacheManager,
    view: ViewX,
    mode: CompareMode,
) -> Vec<RenderedSyncTrace> {
    let mut rendered = Vec::with_capacity(candidates.len());
    let mut raw_ranges = Vec::with_capacity(candidates.len());
    for candidate in candidates.iter().copied() {
        let Some(cache) = caches.get(candidate.trace.field) else {
            continue;
        };
        let shift_s = candidate.trace.preview_delta_us as f64 * 1e-6;
        if !shift_s.is_finite() || shift_s < f32::MIN as f64 || shift_s > f32::MAX as f64 {
            continue;
        }
        let x_bounds = gpu::sync_x_bounds(view, cache.origin_us);
        let query_bounds = (x_bounds.0 - shift_s as f32, x_bounds.1 - shift_s as f32);
        let visible_y = cache.visible_y_range(
            query_bounds.0,
            query_bounds.1,
            TraceGeometry::Linear,
            GapBehavior::Connect,
        );
        rendered.push(candidate);
        raw_ranges.push(visible_y.is_finite().then(|| {
            PreparedYRange::new(cache.y_origin(), visible_y.min as f64, visible_y.max as f64)
                .expect("finite cache extrema form a valid range")
        }));
    }
    let ranges = prepared_y_ranges(mode, &raw_ranges);
    let lanes = gpu::sync_lane_fractions(rendered.len(), mode);
    for ((rendered_trace, range), lane) in rendered.iter_mut().zip(ranges).zip(lanes) {
        rendered_trace.trace.y_range = range;
        rendered_trace.trace.lane = Some(lane);
    }
    rendered
}

fn prepared_y_gutter(
    ui: &egui::Ui,
    outer_rect: egui::Rect,
    mode: CompareMode,
    rendered: &[RenderedSyncTrace],
) -> f32 {
    let lane_height = match mode {
        CompareMode::Overlay => (outer_rect.height() - axes::X_GUTTER).max(1.0),
        CompareMode::Stacked => {
            (outer_rect.height() - axes::X_GUTTER).max(1.0) / rendered.len().max(1) as f32
        }
    };
    rendered
        .iter()
        .map(|rendered_trace| {
            let range = rendered_trace.trace.y_range;
            axes::y_gutter_relative(ui, range.origin, (range.min, range.max), None, lane_height)
        })
        .fold(0.0_f32, f32::max)
}

fn trace_rebased_y_geometry(cache: &TraceCache, range: PreparedYRange) -> Option<(f64, f64)> {
    let lower = range.cache_lower(cache.y_origin());
    let span = range.span();
    (lower.is_finite() && span.is_finite() && span > 0.0).then_some((lower, span))
}

fn nearest_projected_sample(
    pointer: egui::Pos2,
    candidates: impl IntoIterator<Item = ProjectedSample>,
    hit_radius_px: f32,
) -> Option<ProjectedSample> {
    let max_distance_sq = hit_radius_px.max(0.0).powi(2);
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let distance_sq = pointer.distance_sq(candidate.position);
            (distance_sq <= max_distance_sq).then_some((candidate, distance_sq))
        })
        .min_by(|(left, left_distance), (right, right_distance)| {
            left_distance
                .total_cmp(right_distance)
                .then_with(|| left.sample.row.cmp(&right.sample.row))
        })
        .map(|(candidate, _)| candidate)
}

fn projected_candidate_rows_in_x_range(
    cache: &TraceCache,
    cache_x_min: f32,
    cache_x_max: f32,
) -> Vec<usize> {
    let (lo, hi) = cache.index_range(cache_x_min, cache_x_max);
    (lo..hi)
        .filter(|row| {
            cache
                .xy
                .get(row.saturating_mul(2).saturating_add(1))
                .is_some_and(|value| value.is_finite())
        })
        .collect()
}

fn pointer_fraction_in_lane(lane: egui::Rect, pointer: egui::Pos2) -> Option<f32> {
    (lane.contains(pointer) && axes::usable_plot_rect(lane))
        .then(|| ((pointer.x - lane.left()) / lane.width()).clamp(0.0, 1.0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlotPointerAction {
    DoubleClickFit,
    PrimaryPan,
    Other,
}

fn plot_pointer_action(double_clicked: bool, primary_dragged: bool) -> PlotPointerAction {
    if double_clicked {
        PlotPointerAction::DoubleClickFit
    } else if primary_dragged {
        PlotPointerAction::PrimaryPan
    } else {
        PlotPointerAction::Other
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameViews {
    trace_projection: ViewX,
    picker_projection: ViewX,
}

fn final_frame_views(
    initial: ViewX,
    action: PlotPointerAction,
    fitted: Option<ViewX>,
    middle_drag_dx_px: f32,
    plot_width_px: f32,
) -> FrameViews {
    let mut final_view = initial;
    match action {
        PlotPointerAction::DoubleClickFit => {
            if let Some(fitted) = fitted {
                final_view = fitted;
            }
        }
        PlotPointerAction::PrimaryPan => {
            gpu::apply_pan(&mut final_view, middle_drag_dx_px, plot_width_px)
        }
        PlotPointerAction::Other => {}
    }
    FrameViews {
        trace_projection: final_view,
        picker_projection: final_view,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffsetInput {
    pub text: String,
    pub valid: bool,
}

impl OffsetInput {
    fn normalized(value: i64) -> Self {
        Self {
            text: value.to_string(),
            valid: true,
        }
    }

    #[cfg(test)]
    fn set(&mut self, text: impl Into<String>) -> Option<i64> {
        self.text = text.into();
        let parsed = self.text.trim().parse().ok();
        self.valid = parsed.is_some();
        parsed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldSearchResult {
    source: SourceId,
    topic: TopicId,
    field: FieldId,
    label: String,
    score: u32,
}

pub struct SourceSync {
    pub source: SourceId,
    pub topic: Option<TopicId>,
    pub field: Option<FieldId>,
    pub search: String,
    pub baseline_offset_us: i64,
    pub draft_offset_us: i64,
    pub included: bool,
    pub input: OffsetInput,
}

pub struct SyncWindow {
    pub open: bool,
    sources: Vec<SourceSync>,
    reference: SourceId,
    active: Option<SourceId>,
    mode: CompareMode,
    view: Option<ViewX>,
    confirm_discard: bool,
    pending_apply: Option<PendingApply>,
    recovery_error: bool,
    alignment_error: Option<String>,
    picker: Option<PickSession>,
    picker_hover: Option<ProjectedSample>,
    zoom_drag_anchor_x: Option<f32>,
    drag_start_offset_us: Option<i64>,
}

struct PendingApply {
    values: Vec<(SourceId, i64)>,
    dispatch_epoch: u64,
}

impl SyncWindow {
    pub fn open(snapshot: &StoreSnapshot) -> Option<Self> {
        let sources = eligible_sources(snapshot, true);
        if sources.len() < 2 {
            return None;
        }
        let reference = sources[0].source;
        let active = Some(sources[1].source);
        let mut window = Self {
            open: true,
            sources,
            reference,
            active,
            mode: CompareMode::Overlay,
            view: None,
            confirm_discard: false,
            pending_apply: None,
            recovery_error: false,
            alignment_error: None,
            picker: None,
            picker_hover: None,
            zoom_drag_anchor_x: None,
            drag_start_offset_us: None,
        };
        window.fit_selected_plots(snapshot);
        Some(window)
    }

    pub fn reconcile(&mut self, snapshot: &StoreSnapshot) {
        if let Some(pending) = &self.pending_apply
            && snapshot.epoch > pending.dispatch_epoch
        {
            let accepted = pending.values.iter().all(|(id, offset)| {
                snapshot.source(*id).map(|source| source.entry.offset_us) == Some(*offset)
            });
            self.pending_apply = None;
            if accepted {
                self.reload_offsets(snapshot);
            } else {
                self.recovery_error = true;
            }
        }
        self.sources.retain(|item| {
            snapshot.source(item.source).is_some_and(|source| {
                !source.entry.removed && source.entry.kind == SourceKind::File
            })
        });
        for item in &mut self.sources {
            if item
                .topic
                .is_none_or(|topic| !topic_is_available(snapshot, item.source, topic))
            {
                item.topic = first_available_topic(snapshot, item.source);
                item.field = item
                    .topic
                    .and_then(|topic| first_plottable_field_in_topic(snapshot, item.source, topic));
            } else if item.field.is_some_and(|field| {
                !item
                    .topic
                    .is_some_and(|topic| field_is_plottable(snapshot, item.source, topic, field))
            }) {
                item.field = None;
            }
        }
        for candidate in eligible_sources(snapshot, false) {
            if !self
                .sources
                .iter()
                .any(|item| item.source == candidate.source)
            {
                self.sources.push(candidate);
            }
        }
        if !self
            .sources
            .iter()
            .any(|item| item.source == self.reference && item.included)
        {
            if let Some(next) = self.sources.iter().find(|item| item.included) {
                self.reference = next.source;
            }
        }
        if self.active.is_some_and(|id| !self.is_movable(id)) {
            self.active = self.first_movable();
        }
        if self
            .picker
            .is_some_and(|session| !self.pick_session_is_current(session))
        {
            self.cancel_sample_pick();
        }
    }

    pub fn apply_request(
        &self,
        snapshot: &StoreSnapshot,
    ) -> Result<Vec<(SourceId, i64)>, ApplyBlock> {
        if self.recovery_error {
            return Err(ApplyBlock::Conflict);
        }
        if self.sources.iter().any(|item| {
            snapshot
                .source(item.source)
                .map(|source| source.entry.offset_us)
                != Some(item.baseline_offset_us)
        }) {
            return Err(ApplyBlock::Conflict);
        }
        let included: Vec<_> = self.sources.iter().filter(|item| item.included).collect();
        if included
            .iter()
            .any(|item| item.field.is_none() || !item.input.valid)
        {
            return Err(ApplyBlock::InvalidInput);
        }
        if included.len() < 2 {
            return Err(ApplyBlock::InsufficientSources);
        }
        let request: Vec<_> = included
            .into_iter()
            .filter(|item| item.draft_offset_us != item.baseline_offset_us)
            .map(|item| (item.source, item.draft_offset_us))
            .collect();
        if request.is_empty() {
            Err(ApplyBlock::Clean)
        } else {
            Ok(request)
        }
    }

    pub fn mark_applied(&mut self, snapshot: &StoreSnapshot) {
        self.pending_apply = None;
        self.reload_offsets(snapshot);
    }

    pub fn reload_offsets(&mut self, snapshot: &StoreSnapshot) {
        self.pending_apply = None;
        self.recovery_error = false;
        for item in &mut self.sources {
            if let Some(source) = snapshot.source(item.source) {
                let offset = source.entry.offset_us;
                item.baseline_offset_us = offset;
                item.draft_offset_us = offset;
                item.input = OffsetInput::normalized(offset);
            }
        }
    }

    #[cfg(test)]
    pub fn included_ids(&self) -> Vec<SourceId> {
        self.sources
            .iter()
            .filter_map(|item| item.included.then_some(item.source))
            .collect()
    }
    fn selected_plot_time_range(&self, snapshot: &StoreSnapshot) -> Option<TimeRange> {
        self.sources
            .iter()
            .filter(|item| item.included)
            .filter_map(|item| {
                let topic = item.topic?;
                let field = item.field?;
                if !field_is_plottable(snapshot, item.source, topic, field) {
                    return None;
                }
                let first = anchor(snapshot, field, AnchorKind::First)
                    .ok()?
                    .raw_time_us
                    .checked_add(item.draft_offset_us)?;
                let last = anchor(snapshot, field, AnchorKind::Last)
                    .ok()?
                    .raw_time_us
                    .checked_add(item.draft_offset_us)?;
                TimeRange::new(first, last)
            })
            .reduce(TimeRange::union)
    }
    fn fit_selected_plots(&mut self, snapshot: &StoreSnapshot) -> bool {
        let Some(range) = self.selected_plot_time_range(snapshot) else {
            return false;
        };
        let Some(view) = ViewX::try_from_range(range) else {
            return false;
        };
        self.view = Some(view);
        true
    }
    #[cfg(test)]
    pub fn draft_offsets(&self) -> Vec<(SourceId, i64)> {
        self.sources
            .iter()
            .map(|item| (item.source, item.draft_offset_us))
            .collect()
    }
    #[cfg(test)]
    pub fn reference(&self) -> SourceId {
        self.reference
    }
    pub fn first_movable(&self) -> Option<SourceId> {
        self.sources.iter().find_map(|item| {
            (item.included && item.source != self.reference && item.field.is_some())
                .then_some(item.source)
        })
    }
    fn is_movable(&self, id: SourceId) -> bool {
        self.source(id).is_some_and(|item| {
            item.included && item.source != self.reference && item.field.is_some()
        })
    }
    pub fn source(&self, id: SourceId) -> Option<&SourceSync> {
        self.sources.iter().find(|item| item.source == id)
    }
    fn source_mut(&mut self, id: SourceId) -> Option<&mut SourceSync> {
        self.sources.iter_mut().find(|item| item.source == id)
    }
    pub fn draft_offset(&self, id: SourceId) -> Option<i64> {
        Some(self.source(id)?.draft_offset_us)
    }
    #[cfg(test)]
    pub fn input(&self, id: SourceId) -> Option<&str> {
        Some(&self.source(id)?.input.text)
    }
    pub fn relative_offset(&self, id: SourceId) -> Option<i64> {
        let reference = self.source(self.reference)?.draft_offset_us;
        self.source(id)?.draft_offset_us.checked_sub(reference)
    }
    pub fn is_dirty(&self) -> bool {
        self.sources.iter().any(|item| {
            item.included && (!item.input.valid || item.draft_offset_us != item.baseline_offset_us)
        })
    }
    pub fn controls(&self, id: SourceId) -> SourceControls {
        SourceControls {
            movable: self.is_movable(id),
        }
    }
    fn automatic_alignment_ready(&self) -> bool {
        self.pending_apply.is_none()
            && self.picker.is_none()
            && self.active.is_some_and(|active| {
                active != self.reference
                    && self
                        .source(self.reference)
                        .is_some_and(|source| source.field.is_some())
                    && self.is_movable(active)
            })
    }
    pub fn apply_block(&self, snapshot: &StoreSnapshot) -> Option<ApplyBlock> {
        if self.pending_apply.is_some() {
            return Some(ApplyBlock::Clean);
        }
        self.apply_request(snapshot).err()
    }
    #[cfg(test)]
    pub fn set_mode(&mut self, mode: CompareMode) {
        self.mode = mode;
    }
    fn rendered_sync_traces(&mut self, snapshot: &StoreSnapshot) -> Vec<RenderedSyncTrace> {
        self.sources
            .iter_mut()
            .enumerate()
            .filter_map(|(index, item)| {
                let field = item.included.then_some(item.field).flatten()?;
                let current_offset_us = snapshot.source(item.source)?.entry.offset_us;
                let preview_delta_us =
                    match preview_delta_us(item.draft_offset_us, current_offset_us) {
                        Ok(delta) => delta,
                        Err(_) => {
                            item.input.valid = false;
                            return None;
                        }
                    };
                Some(RenderedSyncTrace {
                    source: item.source,
                    trace: SyncTrace {
                        field,
                        preview_delta_us,
                        color: palette::trace_color(index).to_srgb_f32(),
                        y_range: PreparedYRange {
                            origin: f64::NAN,
                            min: f64::NAN,
                            max: f64::NAN,
                        },
                        lane: None,
                    },
                })
            })
            .collect()
    }
    pub fn set_reference(&mut self, id: SourceId) -> Result<(), ()> {
        self.source(id).filter(|item| item.included).ok_or(())?;
        let changed = self.reference != id;
        self.reference = id;
        if changed {
            self.cancel_sample_pick();
        }
        Ok(())
    }
    pub fn set_included(&mut self, id: SourceId, included: bool) -> Result<(), ()> {
        self.source(id).ok_or(())?;
        let replacement = if !included && id == self.reference {
            Some(
                self.sources
                    .iter()
                    .find(|item| item.included && item.source != id)
                    .map(|item| item.source)
                    .ok_or(())?,
            )
        } else {
            None
        };
        self.source_mut(id).expect("source checked above").included = included;
        if let Some(replacement) = replacement {
            self.reference = replacement;
        }
        if self.active.is_some_and(|active| !self.is_movable(active)) {
            self.active = self.first_movable();
        }
        self.cancel_sample_pick();
        Ok(())
    }
    pub fn set_active(&mut self, id: SourceId) -> Result<(), ()> {
        self.is_movable(id).then_some(()).ok_or(())?;
        let changed = self.active != Some(id);
        self.active = Some(id);
        if changed {
            self.cancel_sample_pick();
        }
        Ok(())
    }
    fn pick_session_is_current(&self, session: PickSession) -> bool {
        self.reference == session.reference
            && self.active == Some(session.target)
            && self.source(session.reference).is_some_and(|source| {
                source.included && source.field == Some(session.reference_field)
            })
            && self
                .source(session.target)
                .is_some_and(|source| source.included && source.field == Some(session.target_field))
    }
    fn begin_sample_pick(&mut self) -> Result<(), PickError> {
        let target = self.active.ok_or(PickError::PairUnavailable)?;
        let reference = self
            .source(self.reference)
            .filter(|source| source.included)
            .ok_or(PickError::PairUnavailable)?;
        let target_source = self
            .source(target)
            .filter(|source| source.included && target != self.reference)
            .ok_or(PickError::PairUnavailable)?;
        let session = PickSession {
            reference: self.reference,
            target,
            reference_field: reference.field.ok_or(PickError::PairUnavailable)?,
            target_field: target_source.field.ok_or(PickError::PairUnavailable)?,
            stage: PickStage::Reference,
        };
        self.picker = Some(session);
        self.alignment_error = None;
        Ok(())
    }
    fn cancel_sample_pick(&mut self) {
        self.picker = None;
        self.picker_hover = None;
    }
    fn pick_expected_source(&self) -> Option<SourceId> {
        let session = self.picker?;
        Some(match session.stage {
            PickStage::Reference => session.reference,
            PickStage::Target { .. } => session.target,
        })
    }
    fn accept_picked_sample(
        &mut self,
        source: SourceId,
        sample: SyncSample,
    ) -> Result<(), PickError> {
        let session = self.picker.ok_or(PickError::PairUnavailable)?;
        if !self.pick_session_is_current(session) {
            self.cancel_sample_pick();
            return Err(PickError::PairUnavailable);
        }
        if self.pick_expected_source() != Some(source) {
            return Err(PickError::UnexpectedSource);
        }
        match session.stage {
            PickStage::Reference => {
                self.picker = Some(PickSession {
                    stage: PickStage::Target {
                        reference_sample: sample,
                    },
                    ..session
                });
                Ok(())
            }
            PickStage::Target { reference_sample } => {
                let reference_offset_us = self
                    .draft_offset(session.reference)
                    .ok_or(PickError::PairUnavailable)?;
                let offset = target_offset_us(
                    reference_sample.raw_time_us,
                    reference_offset_us,
                    sample.raw_time_us,
                )
                .map_err(PickError::Alignment)?;
                self.set_draft_offset(session.target, offset)
                    .map_err(|()| PickError::PairUnavailable)?;
                self.cancel_sample_pick();
                self.alignment_error = None;
                Ok(())
            }
        }
    }
    fn align_active(
        &mut self,
        snapshot: &StoreSnapshot,
        method: AutoAlignMethod,
    ) -> Result<(), AlignmentError> {
        let result = (|| {
            let active = self.active.ok_or(AlignmentError::FieldUnavailable)?;
            let reference = self
                .source(self.reference)
                .ok_or(AlignmentError::FieldUnavailable)?;
            let target = self
                .source(active)
                .ok_or(AlignmentError::FieldUnavailable)?;
            let reference_field = reference.field.ok_or(AlignmentError::FieldUnavailable)?;
            let target_field = target.field.ok_or(AlignmentError::FieldUnavailable)?;
            let reference_offset_us = reference.draft_offset_us;
            let (reference_kind, target_kind) = anchor_kinds(method);
            let reference_anchor = anchor(snapshot, reference_field, reference_kind)?;
            let target_anchor = anchor(snapshot, target_field, target_kind)?;
            let offset = target_offset_us(
                reference_anchor.raw_time_us,
                reference_offset_us,
                target_anchor.raw_time_us,
            )?;
            self.set_draft_offset(active, offset)
                .map_err(|_| AlignmentError::FieldUnavailable)
        })();

        match result {
            Ok(()) => self.alignment_error = None,
            Err(error) => self.alignment_error = Some(alignment_error_text(error).to_owned()),
        }
        result
    }
    fn align_and_begin_apply(
        &mut self,
        snapshot: &StoreSnapshot,
        method: AutoAlignMethod,
    ) -> Option<Vec<(SourceId, i64)>> {
        self.align_active(snapshot, method).ok()?;
        let batch = self.apply_request(snapshot).ok()?;
        self.begin_apply(batch.clone(), snapshot.epoch);
        Some(batch)
    }
    pub fn set_topic(
        &mut self,
        snapshot: &StoreSnapshot,
        id: SourceId,
        topic: TopicId,
    ) -> Result<(), ()> {
        topic_is_available(snapshot, id, topic)
            .then_some(())
            .ok_or(())?;
        let field = first_plottable_field_in_topic(snapshot, id, topic);
        let item = self.source_mut(id).ok_or(())?;
        item.topic = Some(topic);
        item.field = field;
        if self.active.is_some_and(|active| !self.is_movable(active)) {
            self.active = self.first_movable();
        }
        self.cancel_sample_pick();
        Ok(())
    }
    fn select_search_result(&mut self, id: SourceId, result: FieldSearchResult) -> Result<(), ()> {
        if result.source != id {
            return Err(());
        }
        let item = self.source_mut(id).ok_or(())?;
        item.topic = Some(result.topic);
        item.field = Some(result.field);
        item.search.clear();
        if self.active.is_some_and(|active| !self.is_movable(active)) {
            self.active = self.first_movable();
        }
        self.cancel_sample_pick();
        Ok(())
    }
    pub fn set_draft_offset(&mut self, id: SourceId, offset: i64) -> Result<(), ()> {
        if !self.is_movable(id) {
            return Err(());
        }
        let item = self.source_mut(id).ok_or(())?;
        item.draft_offset_us = offset;
        item.input = OffsetInput::normalized(offset);
        Ok(())
    }
    #[cfg(test)]
    pub fn set_input(&mut self, id: SourceId, input: impl Into<String>) -> Result<(), ()> {
        if !self.is_movable(id) {
            return Err(());
        }
        let item = self.source_mut(id).ok_or(())?;
        if let Some(offset) = item.input.set(input) {
            item.draft_offset_us = offset;
        }
        Ok(())
    }

    pub fn begin_apply(&mut self, values: Vec<(SourceId, i64)>, dispatch_epoch: u64) {
        self.pending_apply = Some(PendingApply {
            values,
            dispatch_epoch,
        });
        self.recovery_error = false;
    }

    pub fn apply_dispatch_failed(&mut self) {
        self.pending_apply = None;
        self.recovery_error = true;
    }

    pub fn apply_drag_delta(
        &mut self,
        id: SourceId,
        start: i64,
        delta: i64,
    ) -> Result<(), OffsetMathError> {
        let Some(offset) = start.checked_add(delta) else {
            if let Some(item) = self.source_mut(id) {
                item.input.valid = false;
            }
            return Err(OffsetMathError);
        };
        self.set_draft_offset(id, offset)
            .map_err(|_| OffsetMathError)
    }
    pub fn reset_one(&mut self, id: SourceId) -> Result<(), ()> {
        let item = self.source_mut(id).ok_or(())?;
        item.draft_offset_us = item.baseline_offset_us;
        item.input = OffsetInput::normalized(item.baseline_offset_us);
        Ok(())
    }
    pub fn reset_all(&mut self) {
        for item in &mut self.sources {
            item.draft_offset_us = item.baseline_offset_us;
            item.input = OffsetInput::normalized(item.baseline_offset_us);
        }
    }

    pub fn pending_is_authoritative(&self, snapshot: &StoreSnapshot) -> bool {
        self.pending_apply.as_ref().is_some_and(|pending| {
            snapshot.epoch > pending.dispatch_epoch
                && pending.values.iter().all(|(id, offset)| {
                    snapshot.source(*id).map(|source| source.entry.offset_us) == Some(*offset)
                })
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        snapshot: &Arc<StoreSnapshot>,
        gpu: &GpuBridge,
        frame: &eframe::Frame,
        caches: &mut CacheManager,
    ) -> SyncWindowResponse {
        let mut response = SyncWindowResponse::default();
        let was_open = self.open;
        let mut requested_open = self.open;
        let viewport = ctx.content_rect();
        let max_size = egui::vec2(
            (viewport.width() - 32.0).max(1.0),
            (viewport.height() - 32.0).max(1.0),
        );
        let min_size = egui::vec2(720.0_f32.min(max_size.x), 480.0_f32.min(max_size.y));
        let default_size = egui::vec2(900.0_f32.min(max_size.x), 620.0_f32.min(max_size.y));
        egui::Window::new("Sync Sources")
            .open(&mut requested_open)
            .default_size(default_size)
            .min_size(min_size)
            .max_size(max_size)
            .show(ctx, |ui| {
                self.toolbar(ui, snapshot, &mut response);
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("sync-source-rows")
                    .max_height(180.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| self.source_rows(ui, snapshot));
                ui.separator();
                self.plot(ui, snapshot, gpu, frame, caches);
                self.picker_status(ui, snapshot);
                ui.separator();
                self.footer(ui, snapshot, &mut response);
            });

        if was_open && !requested_open {
            if self.is_dirty() || self.pending_apply.is_some() {
                self.confirm_discard = true;
                self.open = true;
            } else {
                self.open = false;
            }
        }
        self.discard_confirmation(ctx);
        response
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &StoreSnapshot,
        response: &mut SyncWindowResponse,
    ) {
        if self.picker.is_some()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.cancel_sample_pick();
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, CompareMode::Overlay, "Overlay");
            ui.selectable_value(&mut self.mode, CompareMode::Stacked, "Stacked");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Reset all").clicked() {
                    self.reset_all();
                }
                if ui.button("Reset zoom").clicked() {
                    self.fit_selected_plots(snapshot);
                }
            });
        });
        let active = self.active;
        let pair_ready = self.automatic_alignment_ready();
        let reference_label = source_label(snapshot, self.reference);
        let active_label = active
            .map(|active| source_label(snapshot, active))
            .unwrap_or_else(|| "Select active target".to_owned());
        ui.horizontal(|ui| {
            ui.label(reference_label);
            ui.add(sync_toolbar_icon(ui, crate::ui::icons::arrow_right()));
            ui.label(active_label);
            for (label, method) in [
                ("First to First", AutoAlignMethod::FirstToFirst),
                ("Last to Last", AutoAlignMethod::LastToLast),
                ("Back to back", AutoAlignMethod::BackToBack),
                ("First change", AutoAlignMethod::FirstChange),
            ] {
                if ui
                    .add_enabled(
                        pair_ready,
                        egui::Button::image_and_text(
                            sync_toolbar_icon(ui, crate::ui::icons::arrow_left_right()),
                            label,
                        ),
                    )
                    .clicked()
                {
                    if let Some(batch) = self.align_and_begin_apply(snapshot, method) {
                        response.apply = Some(batch);
                    }
                }
            }
            if self.picker.is_some() {
                if ui.button("Cancel picking").clicked() {
                    self.cancel_sample_pick();
                }
            } else if ui
                .add_enabled(pair_ready, egui::Button::new("Pick samples"))
                .clicked()
            {
                let _ = self.begin_sample_pick();
            }
        });
        if let Some(message) = &self.alignment_error {
            ui.colored_label(ui.visuals().error_fg_color, message);
        }
    }

    fn picker_status(&self, ui: &mut egui::Ui, snapshot: &StoreSnapshot) {
        let Some(expected) = self.pick_expected_source() else {
            return;
        };
        let mut status = format!("Click a sample on {}", source_label(snapshot, expected));
        if let Some(hovered) = self
            .picker_hover
            .filter(|hovered| hovered.source == expected)
        {
            let offset = self.draft_offset(expected).unwrap_or_default();
            let effective = hovered.sample.raw_time_us.checked_add(offset);
            status.push_str(&format!(
                " - raw {} us, effective {}, value {}",
                hovered.sample.raw_time_us,
                effective.map_or_else(|| "overflow".to_owned(), |time| format!("{time} us")),
                hovered.sample.value
            ));
        }
        ui.label(status);
    }

    fn source_rows(&mut self, ui: &mut egui::Ui, snapshot: &StoreSnapshot) {
        let ids: Vec<_> = self.sources.iter().map(|item| item.source).collect();
        for (index, id) in ids.into_iter().enumerate() {
            let Some(item) = self.source(id) else {
                continue;
            };
            let mut included = item.included;
            let mut active = self.active == Some(id);
            let mut reference = self.reference == id;
            let mut topic = item.topic;
            let mut field = item.field;
            let previous_field = item.field;
            let mut search = item.search.clone();
            let mut input = item.input.text.clone();
            let movable = self.controls(id).movable;
            ui.horizontal(|ui| {
                if ui.checkbox(&mut included, "").changed() {
                    let _ = self.set_included(id, included);
                }
                if ui.radio(active, "Active").clicked() {
                    active = self.set_active(id).is_ok();
                }
                if ui.radio(reference, "Reference").clicked() {
                    let _ = self.set_reference(id);
                    reference = true;
                }
                ui.colored_label(egui_trace_color(index), "■");
                ui.label(source_label(snapshot, id));
                ui.add_enabled_ui(included, |ui| {
                    let search_response = ui.add(
                        egui::TextEdit::singleline(&mut search)
                            .hint_text("Find topic/field…")
                            .desired_width(150.0),
                    );
                    let search_changed = search_response.changed();
                    if let Some(item) = self.source_mut(id) {
                        item.search = search.clone();
                    }
                    let results = field_search_results(snapshot, id, search.trim());
                    let popup_id = search_response.id.with("results");
                    let set_open = if search.trim().is_empty() {
                        Some(egui::SetOpenCommand::Bool(false))
                    } else if search_changed
                        || search_response.clicked()
                        || search_response.has_focus()
                    {
                        Some(egui::SetOpenCommand::Bool(true))
                    } else {
                        None
                    };
                    let highlight_id = search_response.id.with("highlight");
                    let mut selected = None;
                    egui::Popup::from_response(&search_response)
                        .id(popup_id)
                        .open_memory(set_open)
                        .width(280.0)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| {
                            let mut highlighted = if search_changed {
                                0
                            } else {
                                ui.memory_mut(|memory| {
                                    memory.data.get_temp::<usize>(highlight_id).unwrap_or(0)
                                })
                            };
                            if results.is_empty() {
                                ui.weak("No matching fields");
                                return;
                            }
                            highlighted = highlighted.min(results.len() - 1);
                            if ui.input_mut(|input| {
                                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                            }) {
                                highlighted = (highlighted + 1).min(results.len() - 1);
                            }
                            if ui.input_mut(|input| {
                                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                            }) {
                                highlighted = highlighted.saturating_sub(1);
                            }
                            if ui.input_mut(|input| {
                                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                            }) {
                                selected = Some(results[highlighted].clone());
                            }
                            ui.memory_mut(|memory| {
                                memory.data.insert_temp(highlight_id, highlighted)
                            });
                            egui::ScrollArea::vertical()
                                .max_height(220.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    for (result_index, result) in results.iter().enumerate() {
                                        let response = ui.selectable_label(
                                            result_index == highlighted,
                                            &result.label,
                                        );
                                        if result_index == highlighted {
                                            response.scroll_to_me(Some(egui::Align::Center));
                                        }
                                        if response.clicked() {
                                            selected = Some(result.clone());
                                        }
                                    }
                                });
                        });
                    if let Some(result) = selected
                        && self.select_search_result(id, result).is_ok()
                    {
                        topic = self.source(id).and_then(|item| item.topic);
                        field = self.source(id).and_then(|item| item.field);
                        search.clear();
                        egui::Popup::close_id(ui.ctx(), popup_id);
                    }
                    let previous_topic = topic;
                    egui::ComboBox::from_id_salt(("sync-topic", id.0))
                        .selected_text(topic.map_or_else(
                            || "Select topic".to_owned(),
                            |topic| topic_label(snapshot, topic),
                        ))
                        .show_ui(ui, |ui| {
                            for candidate in available_topics(snapshot, id) {
                                ui.selectable_value(
                                    &mut topic,
                                    Some(candidate),
                                    topic_label(snapshot, candidate),
                                );
                            }
                        });
                    if topic != previous_topic
                        && let Some(topic) = topic
                        && self.set_topic(snapshot, id, topic).is_ok()
                    {
                        field = self.source(id).and_then(|item| item.field);
                    }
                    egui::ComboBox::from_id_salt(("sync-field", id.0))
                        .selected_text(field.map_or_else(
                            || "Select field".to_owned(),
                            |f| field_label(snapshot, f),
                        ))
                        .show_ui(ui, |ui| {
                            if let Some(topic) = topic {
                                for candidate in plottable_fields(snapshot, id, topic) {
                                    ui.selectable_value(
                                        &mut field,
                                        Some(candidate),
                                        field_label(snapshot, candidate),
                                    );
                                }
                            }
                        });
                });
                if let Some(item) = self.source_mut(id) {
                    item.field = field;
                }
                if field != previous_field {
                    self.cancel_sample_pick();
                }
                ui.label(format!(
                    "relative {}",
                    format_offset_us(self.relative_offset(id).unwrap_or(0))
                ));
                ui.add_enabled_ui(movable, |ui| {
                    let edit = ui.add(egui::TextEdit::singleline(&mut input).desired_width(110.0));
                    if edit.changed() {
                        if let Some(item) = self.source_mut(id) {
                            item.input.text = input.clone();
                            match parse_offset_us(input.trim()).or_else(|_| {
                                input
                                    .trim()
                                    .parse::<i64>()
                                    .map_err(|_| OffsetParseError::Number)
                            }) {
                                Ok(value) => {
                                    item.input.valid = true;
                                    item.draft_offset_us = value;
                                }
                                Err(_) => item.input.valid = false,
                            }
                        }
                    }
                    if ui.button("Reset").clicked() {
                        let _ = self.reset_one(id);
                    }
                });
            });
        }
    }

    fn plot(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Arc<StoreSnapshot>,
        gpu: &GpuBridge,
        frame: &eframe::Frame,
        caches: &mut CacheManager,
    ) {
        let height = sync_plot_height(ui.available_height());
        let (rect, interaction) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click_and_drag(),
        );
        let initial_view = *self.view.get_or_insert_with(|| ViewX::new(0, 10_000_000));
        let candidates = self.rendered_sync_traces(snapshot);
        for rendered_trace in &candidates {
            let field = rendered_trace.trace.field;
            caches.request(field, snapshot);
        }
        if preparation_needs_repaint(candidates.iter().map(|candidate| {
            let field = candidate.trace.field;
            caches.is_building(field)
        })) {
            ui.ctx().request_repaint();
        }
        let pointer_action = plot_pointer_action(
            interaction.double_clicked(),
            interaction.dragged_by(egui::PointerButton::Primary),
        );
        let pan_width = if pointer_action == PlotPointerAction::PrimaryPan {
            let provisional =
                prepare_rendered_geometry(&candidates, caches, initial_view, self.mode);
            (rect.width() - prepared_y_gutter(ui, rect, self.mode, &provisional)).max(1.0)
        } else {
            rect.width().max(1.0)
        };
        let fitted = (pointer_action == PlotPointerAction::DoubleClickFit)
            .then(|| self.selected_plot_time_range(snapshot))
            .flatten()
            .and_then(ViewX::try_from_range);
        let frame_views = final_frame_views(
            initial_view,
            pointer_action,
            fitted,
            interaction.drag_delta().x,
            pan_width,
        );
        self.view = Some(frame_views.trace_projection);
        if pointer_action != PlotPointerAction::Other {
            self.drag_start_offset_us = None;
            self.zoom_drag_anchor_x = None;
        }

        let rendered =
            prepare_rendered_geometry(&candidates, caches, frame_views.trace_projection, self.mode);
        let y_gutter = prepared_y_gutter(ui, rect, self.mode, &rendered);
        let plot_rect = egui::Rect::from_min_max(
            egui::pos2((rect.left() + y_gutter).min(rect.right()), rect.top()),
            egui::pos2(
                rect.right(),
                (rect.bottom() - axes::X_GUTTER).max(rect.top()),
            ),
        );
        let traces: Vec<_> = rendered.iter().map(|rendered| rendered.trace).collect();
        let lanes = gpu::sync_lane_rects(plot_rect, &traces);

        if !axes::usable_plot_rect(plot_rect) {
            return;
        }

        let common_origin = snapshot.global_time_range().map_or(0, |range| range.min_us);
        axes::draw_x(
            ui,
            plot_rect,
            frame_views.trace_projection.seconds(common_origin),
        );
        match self.mode {
            CompareMode::Overlay => {
                if let Some(rendered_trace) = rendered.first() {
                    let range = rendered_trace.trace.y_range;
                    axes::draw_y_relative(
                        ui,
                        plot_rect,
                        range.origin,
                        (range.min, range.max),
                        None,
                    );
                }
                axes::draw_border(ui, plot_rect);
            }
            CompareMode::Stacked => {
                for (rendered_trace, lane) in rendered.iter().zip(&lanes) {
                    let range = rendered_trace.trace.y_range;
                    axes::draw_y_relative(ui, *lane, range.origin, (range.min, range.max), None);
                    axes::draw_border(ui, *lane);
                }
            }
        }
        if let Some(callback) = gpu.sync_plot_callback(
            ui,
            frame,
            caches,
            plot_rect,
            &traces,
            frame_views.trace_projection,
        ) {
            ui.painter().add(callback);
        }
        let view = frame_views.picker_projection;
        let hovered_neighborhood = self.picker.and_then(|session| {
            let expected = self.pick_expected_source()?;
            let index = rendered
                .iter()
                .position(|rendered| rendered.source == expected)?;
            let rendered_trace = rendered.get(index)?;
            let field = match session.stage {
                PickStage::Reference => session.reference_field,
                PickStage::Target { .. } => session.target_field,
            };
            interaction.hover_pos().and_then(|pointer| {
                projected_sample_neighborhood(
                    pointer,
                    expected,
                    field,
                    rendered_trace.trace.preview_delta_us,
                    rendered_trace.trace.y_range,
                    lanes[index],
                    view,
                    snapshot,
                    caches,
                )
            })
        });
        if let Some(neighborhood) = hovered_neighborhood {
            let neighbor_color = ui.visuals().selection.bg_fill;
            for neighbor in [neighborhood.previous, neighborhood.next]
                .into_iter()
                .flatten()
            {
                ui.painter()
                    .circle_filled(neighbor.position, 3.5, neighbor_color);
            }
            ui.painter().circle_stroke(
                neighborhood.current.position,
                6.0,
                egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
            );
        }
        if let Some(session) = self.picker
            && let PickStage::Target { reference_sample } = session.stage
            && let Some(index) = rendered
                .iter()
                .position(|rendered| rendered.source == session.reference)
            && let Some(rendered_trace) = rendered.get(index)
            && let Some(projected) = project_known_sample(
                session.reference,
                session.reference_field,
                rendered_trace.trace.preview_delta_us,
                rendered_trace.trace.y_range,
                reference_sample,
                lanes[index],
                view,
                caches,
            )
        {
            ui.painter().circle_stroke(
                projected.position,
                6.0,
                egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
            );
        }
        let selected_source = if pointer_action == PlotPointerAction::Other
            && interaction.clicked()
            && self.picker.is_none()
            && let Some(pointer) = interaction.interact_pointer_pos()
        {
            let selected = match self.mode {
                CompareMode::Stacked => gpu::sync_active_trace_at(&lanes, pointer),
                CompareMode::Overlay => nearest_overlay_trace(
                    pointer,
                    &overlay_hit_segments(plot_rect, view, pointer.x, &rendered, snapshot, caches),
                    7.0,
                ),
            };
            selected.map(|index| rendered[index].source)
        } else {
            None
        };
        self.picker_hover = hovered_neighborhood.map(|neighborhood| neighborhood.current);
        if pointer_action == PlotPointerAction::Other
            && interaction.clicked()
            && self.picker.is_some()
            && let Some(picked) = hovered_neighborhood.map(|neighborhood| neighborhood.current)
        {
            if let Err(error) = self.accept_picked_sample(picked.source, picked.sample) {
                self.alignment_error = Some(match error {
                    PickError::Alignment(error) => alignment_error_text(error).to_owned(),
                    PickError::PairUnavailable => "The selected source pair is unavailable".into(),
                    PickError::UnexpectedSource => "Pick the requested source first".into(),
                });
            }
        } else if let Some(selected) = selected_source {
            let _ = self.set_active(selected);
        }
        if pointer_action == PlotPointerAction::Other
            && self.picker.is_none()
            && interaction.drag_started_by(egui::PointerButton::Middle)
        {
            self.drag_start_offset_us = self.active.and_then(|id| self.draft_offset(id));
        }
        if pointer_action == PlotPointerAction::Other
            && self.picker.is_none()
            && interaction.dragged_by(egui::PointerButton::Middle)
            && let Some(active) = self.active
            && active != self.reference
            && let Some(total_drag) = interaction.total_drag_delta()
            && let Some(delta) = drag_delta_us(total_drag.x, plot_rect.width(), view.span_us())
            && let Some(start) = self.drag_start_offset_us
        {
            let _ = self.apply_drag_delta(active, start, delta);
        }
        if interaction.drag_stopped_by(egui::PointerButton::Middle) {
            self.drag_start_offset_us = None;
        }
        if pointer_action == PlotPointerAction::Other && interaction.hovered() {
            let scroll =
                ui.input(|input| input.smooth_scroll_delta.y + input.zoom_delta().ln() * 500.0);
            if scroll != 0.0 {
                let frac = interaction
                    .hover_pos()
                    .map_or(0.5, |p| (p.x - plot_rect.left()) / plot_rect.width());
                if let Some(view) = &mut self.view {
                    gpu::apply_zoom(view, frac, scroll);
                }
            }
        }
        if pointer_action == PlotPointerAction::Other
            && interaction.drag_started_by(egui::PointerButton::Secondary)
        {
            self.zoom_drag_anchor_x = interaction.interact_pointer_pos().map(|p| p.x);
        }
        if let Some(anchor_x) = self.zoom_drag_anchor_x
            && let Some(pointer) = interaction.interact_pointer_pos()
        {
            draw_zoom_drag_overlay(ui, plot_rect, anchor_x, pointer.x);
        }
        if pointer_action == PlotPointerAction::Other
            && interaction.drag_stopped_by(egui::PointerButton::Secondary)
            && let (Some(anchor), Some(pointer)) = (
                self.zoom_drag_anchor_x.take(),
                interaction.interact_pointer_pos(),
            )
            && let Some(current) = self.view
            && let Some(zoomed) = gpu::zoom_drag_view(
                current,
                plot_rect.left(),
                plot_rect.width(),
                anchor,
                pointer.x,
            )
        {
            self.view = Some(zoomed);
        }
    }

    fn footer(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &StoreSnapshot,
        response: &mut SyncWindowResponse,
    ) {
        ui.horizontal(|ui| {
            let block = self.apply_block(snapshot);
            if self.pending_apply.is_some() {
                ui.label("Applying…");
            } else if let Some(status) = match block {
                Some(ApplyBlock::InvalidInput) => Some("Invalid offset or field"),
                Some(ApplyBlock::Conflict) => Some("Source offsets changed externally"),
                Some(ApplyBlock::InsufficientSources) => Some("Include at least two sources"),
                None | Some(ApplyBlock::Clean) => None,
            } {
                ui.label(status);
            }
            if block == Some(ApplyBlock::Conflict) && ui.button("Reload current offsets").clicked()
            {
                self.reload_offsets(snapshot);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(block.is_none(), egui::Button::new("Apply"))
                    .clicked()
                    && let Ok(batch) = self.apply_request(snapshot)
                {
                    self.begin_apply(batch.clone(), snapshot.epoch);
                    response.apply = Some(batch);
                }
                if ui.button("Close").clicked() {
                    if self.is_dirty() || self.pending_apply.is_some() {
                        self.confirm_discard = true;
                    } else {
                        self.open = false;
                    }
                }
            });
        });
    }

    fn discard_confirmation(&mut self, ctx: &egui::Context) {
        if !self.confirm_discard {
            return;
        }
        egui::Window::new("Discard synchronization changes?")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Your synchronization changes have not been applied.");
                ui.horizontal(|ui| {
                    if ui.button("Discard changes").clicked() {
                        self.confirm_discard = false;
                        self.open = false;
                    }
                    if ui.button("Keep editing").clicked() {
                        self.confirm_discard = false;
                        self.open = true;
                    }
                });
            });
    }
}

fn overlay_hit_segments(
    rect: egui::Rect,
    view: ViewX,
    pointer_x: f32,
    rendered: &[RenderedSyncTrace],
    snapshot: &StoreSnapshot,
    caches: &mut CacheManager,
) -> Vec<OverlayHitSegment> {
    let mut segments = Vec::new();
    for (trace_index, rendered_trace) in rendered.iter().enumerate() {
        let Some(cache) = caches.get(rendered_trace.trace.field) else {
            continue;
        };
        if snapshot.source(rendered_trace.source).is_none() {
            continue;
        }
        let shift_s = rendered_trace.trace.preview_delta_us as f64 * 1e-6;
        if !shift_s.is_finite() {
            continue;
        }
        let shift_s = shift_s as f32;
        let x_bounds = view.seconds(cache.origin_us);
        let query_bounds = (x_bounds.0 - shift_s, x_bounds.1 - shift_s);
        let Some((y_lower, y_span)) = trace_rebased_y_geometry(cache, rendered_trace.trace.y_range)
        else {
            continue;
        };
        if !axes::usable_plot_rect(rect) {
            continue;
        }
        let pointer_frac = ((pointer_x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let target_x = query_bounds.0 + pointer_frac * (query_bounds.1 - query_bounds.0);
        let (lo, hi) = cache.finite_window(target_x, target_x);
        if lo >= hi || cache.xy.len() < hi * 2 {
            continue;
        }
        let left = lo;
        let right = hi - 1;
        let map = |index: usize| {
            let x = cache.xy[index * 2] + shift_s;
            let y = cache.xy[index * 2 + 1];
            let x_frac = (x - x_bounds.0) / (x_bounds.1 - x_bounds.0).max(f32::MIN_POSITIVE);
            let y_frac = ((y as f64 - y_lower) / y_span) as f32;
            egui::pos2(
                rect.left() + x_frac * rect.width(),
                rect.bottom() - y_frac * rect.height(),
            )
        };
        segments.push(OverlayHitSegment::new(trace_index, map(left), map(right)));
    }
    segments
}

#[allow(clippy::too_many_arguments)]
fn project_cache_row(
    source: SourceId,
    sample: SyncSample,
    cache: &TraceCache,
    preview_shift_s: f32,
    x_bounds: (f32, f32),
    y_geometry: (f64, f64),
    lane: egui::Rect,
) -> Option<ProjectedSample> {
    let index = sample.row.checked_mul(2)?;
    let x = *cache.xy.get(index)? + preview_shift_s;
    let y = *cache.xy.get(index + 1)?;
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let x_fraction = (x - x_bounds.0) / (x_bounds.1 - x_bounds.0).max(f32::MIN_POSITIVE);
    if !axes::usable_plot_rect(lane) {
        return None;
    }
    let y_fraction = ((y as f64 - y_geometry.0) / y_geometry.1) as f32;
    Some(ProjectedSample {
        source,
        sample,
        position: egui::pos2(
            lane.left() + x_fraction * lane.width(),
            lane.bottom() - y_fraction * lane.height(),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn projected_sample_neighborhood(
    pointer: egui::Pos2,
    source: SourceId,
    field: FieldId,
    preview_shift_us: i64,
    y_range: PreparedYRange,
    lane: egui::Rect,
    view: ViewX,
    snapshot: &StoreSnapshot,
    caches: &mut CacheManager,
) -> Option<ProjectedNeighborhood> {
    let cache = caches.get(field)?;
    let preview_shift_s = preview_shift_us as f64 * 1e-6;
    if !preview_shift_s.is_finite()
        || preview_shift_s < f32::MIN as f64
        || preview_shift_s > f32::MAX as f64
    {
        return None;
    }
    let preview_shift_s = preview_shift_s as f32;
    let x_bounds = gpu::sync_x_bounds(view, cache.origin_us);
    let query_bounds = (x_bounds.0 - preview_shift_s, x_bounds.1 - preview_shift_s);
    let y_geometry = trace_rebased_y_geometry(cache, y_range)?;
    let pointer_fraction = pointer_fraction_in_lane(lane, pointer)?;
    let pointer_cache_x = query_bounds.0 + pointer_fraction * (query_bounds.1 - query_bounds.0);
    let cache_x_radius = 7.0 / lane.width() * (query_bounds.1 - query_bounds.0).abs();
    let candidates = projected_candidate_rows_in_x_range(
        cache,
        pointer_cache_x - cache_x_radius,
        pointer_cache_x + cache_x_radius,
    )
    .into_iter()
    .filter_map(|row| {
        let index = row.checked_mul(2)?;
        let value = *cache.xy.get(index + 1)?;
        (cache.xy.get(index)?.is_finite() && value.is_finite()).then_some(ProjectedSample {
            source,
            sample: SyncSample {
                row,
                raw_time_us: 0,
                value: value as f64,
            },
            position: project_cache_row(
                source,
                SyncSample {
                    row,
                    raw_time_us: 0,
                    value: value as f64,
                },
                cache,
                preview_shift_s,
                x_bounds,
                y_geometry,
                lane,
            )?
            .position,
        })
    });
    let row = nearest_projected_sample(pointer, candidates, 7.0)?
        .sample
        .row;
    let SampleNeighborhood {
        previous,
        current,
        next,
    } = sample_neighborhood(snapshot, field, row).ok()?;
    Some(ProjectedNeighborhood {
        previous: previous.and_then(|sample| {
            project_cache_row(
                source,
                sample,
                cache,
                preview_shift_s,
                x_bounds,
                y_geometry,
                lane,
            )
        }),
        current: project_cache_row(
            source,
            current,
            cache,
            preview_shift_s,
            x_bounds,
            y_geometry,
            lane,
        )?,
        next: next.and_then(|sample| {
            project_cache_row(
                source,
                sample,
                cache,
                preview_shift_s,
                x_bounds,
                y_geometry,
                lane,
            )
        }),
    })
}

#[allow(clippy::too_many_arguments)]
fn project_known_sample(
    source: SourceId,
    field: FieldId,
    preview_shift_us: i64,
    y_range: PreparedYRange,
    sample: SyncSample,
    lane: egui::Rect,
    view: ViewX,
    caches: &mut CacheManager,
) -> Option<ProjectedSample> {
    let cache = caches.get(field)?;
    let preview_shift_s = (preview_shift_us as f64 * 1e-6) as f32;
    let x_bounds = gpu::sync_x_bounds(view, cache.origin_us);
    let y_geometry = trace_rebased_y_geometry(cache, y_range)?;
    project_cache_row(
        source,
        sample,
        cache,
        preview_shift_s,
        x_bounds,
        y_geometry,
        lane,
    )
}

fn egui_trace_color(index: usize) -> egui::Color32 {
    let color = palette::trace_color(index);
    egui::Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a)
}

fn source_label(snapshot: &StoreSnapshot, id: SourceId) -> String {
    snapshot
        .source(id)
        .map(|source| source.entry.label.clone())
        .unwrap_or_else(|| format!("Source {}", id.0))
}

fn field_label(snapshot: &StoreSnapshot, id: FieldId) -> String {
    snapshot
        .fields
        .get(id.index())
        .filter(|field| field.id == id)
        .map(|field| field.name.clone())
        .unwrap_or_else(|| format!("Field {}", id.0))
}

fn topic_label(snapshot: &StoreSnapshot, id: TopicId) -> String {
    snapshot
        .topic(id)
        .map(|topic| topic.entry.name.clone())
        .unwrap_or_else(|| format!("Topic {}", id.0))
}

fn available_topics(snapshot: &StoreSnapshot, source: SourceId) -> Vec<TopicId> {
    let Some(source) = snapshot.source(source) else {
        return Vec::new();
    };
    source
        .topics
        .iter()
        .filter_map(|id| {
            snapshot.topic(*id).and_then(|topic| {
                (!topic.entry.removed && topic.store.is_some()).then_some(topic.entry.id)
            })
        })
        .collect()
}

fn topic_is_available(snapshot: &StoreSnapshot, source: SourceId, wanted: TopicId) -> bool {
    available_topics(snapshot, source).contains(&wanted)
}

fn plottable_fields(snapshot: &StoreSnapshot, source: SourceId, topic: TopicId) -> Vec<FieldId> {
    if !topic_is_available(snapshot, source, topic) {
        return Vec::new();
    }
    let Some(topic) = snapshot.topic(topic) else {
        return Vec::new();
    };
    let Some(store) = topic.store.as_ref() else {
        return Vec::new();
    };
    snapshot
        .fields
        .iter()
        .filter(|field| field.topic == topic.entry.id)
        .zip(store.schema.fields().iter())
        .filter_map(|(field, schema)| {
            (!field.removed && is_plottable(&schema.dtype)).then_some(field.id)
        })
        .collect()
}

fn field_search_results(
    snapshot: &StoreSnapshot,
    source: SourceId,
    query: &str,
) -> Vec<FieldSearchResult> {
    let mut results = available_topics(snapshot, source)
        .into_iter()
        .flat_map(|topic| {
            plottable_fields(snapshot, source, topic)
                .into_iter()
                .map(move |field| (topic, field))
        })
        .filter_map(|(topic, field)| {
            let label = format!(
                "{} › {}",
                topic_label(snapshot, topic),
                field_label(snapshot, field)
            );
            let score = fuzzy_match_score(query, &label)?;
            Some(FieldSearchResult {
                source,
                topic,
                field,
                label,
                score,
            })
        })
        .collect::<Vec<_>>();
    results.sort_by(|left, right| {
        left.score
            .cmp(&right.score)
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
            .then_with(|| left.topic.0.cmp(&right.topic.0))
            .then_with(|| left.field.0.cmp(&right.field.0))
    });
    results
}

fn first_available_topic(snapshot: &StoreSnapshot, source: SourceId) -> Option<TopicId> {
    let topics = available_topics(snapshot, source);
    topics
        .iter()
        .copied()
        .find(|topic| !plottable_fields(snapshot, source, *topic).is_empty())
        .or_else(|| topics.first().copied())
}

fn eligible_sources(snapshot: &StoreSnapshot, included: bool) -> Vec<SourceSync> {
    snapshot
        .sources
        .iter()
        .filter(|source| !source.entry.removed && source.entry.kind == SourceKind::File)
        .map(|source| {
            let topic = first_available_topic(snapshot, source.entry.id);
            let field = topic
                .and_then(|topic| first_plottable_field_in_topic(snapshot, source.entry.id, topic));
            let offset = source.entry.offset_us;
            SourceSync {
                source: source.entry.id,
                topic,
                field,
                search: String::new(),
                baseline_offset_us: offset,
                draft_offset_us: offset,
                included,
                input: OffsetInput::normalized(offset),
            }
        })
        .collect()
}

fn first_plottable_field_in_topic(
    snapshot: &StoreSnapshot,
    source: SourceId,
    topic: TopicId,
) -> Option<FieldId> {
    plottable_fields(snapshot, source, topic).into_iter().next()
}

fn field_is_plottable(
    snapshot: &StoreSnapshot,
    source: SourceId,
    topic: TopicId,
    wanted: FieldId,
) -> bool {
    plottable_fields(snapshot, source, topic).contains(&wanted)
}

fn is_plottable(dtype: &DataType) -> bool {
    matches!(
        dtype,
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

fn sync_toolbar_icon(ui: &egui::Ui, source: egui::ImageSource<'static>) -> egui::Image<'static> {
    egui::Image::new(source)
        .fit_to_exact_size(egui::Vec2::splat(ui.spacing().icon_width))
        .tint(ui.visuals().text_color())
}

#[cfg(test)]
mod tests;

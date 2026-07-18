use arrow::datatypes::DataType;
use delog_cache::{CacheManager, GapBehavior, TraceCache, TraceGeometry};
use delog_core::identity::{FieldId, SourceId, SourceKind, TopicId};
use delog_core::snapshot::StoreSnapshot;
use delog_core::time::TimeRange;
use delog_render::palette;
use std::sync::Arc;

use crate::axes;
use crate::gpu::{self, GpuBridge, PreparedYRange, SyncTrace};
use crate::plot::ViewX;
use crate::sync_alignment::{
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
pub enum CompareMode {
    Overlay,
    Stacked,
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
    MiddlePan,
    Other,
}

fn plot_pointer_action(double_clicked: bool, middle_dragged: bool) -> PlotPointerAction {
    if double_clicked {
        PlotPointerAction::DoubleClickFit
    } else if middle_dragged {
        PlotPointerAction::MiddlePan
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
        PlotPointerAction::MiddlePan => {
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

fn fuzzy_match_score(query: &str, candidate: &str) -> Option<u32> {
    let candidate = candidate.to_lowercase();
    let mut matched_any = false;
    let mut total = 0_u32;
    for token in query.split_whitespace().map(str::to_lowercase) {
        if token.is_empty() {
            continue;
        }
        matched_any = true;
        total = total.checked_add(fuzzy_token_score(&token, &candidate)?)?;
    }
    matched_any.then_some(total)
}

fn fuzzy_token_score(token: &str, candidate: &str) -> Option<u32> {
    if let Some(position) = candidate.find(token) {
        return u32::try_from(
            position.saturating_mul(2) + candidate.len().saturating_sub(token.len()),
        )
        .ok();
    }

    let wanted: Vec<_> = token.chars().collect();
    let mut next = 0;
    let mut start = None;
    let mut previous = None;
    let mut gaps = 0_usize;
    for (index, character) in candidate.chars().enumerate() {
        if wanted.get(next) != Some(&character) {
            continue;
        }
        start.get_or_insert(index);
        if let Some(previous) = previous {
            gaps = gaps.saturating_add(index.saturating_sub(previous + 1));
        }
        previous = Some(index);
        next += 1;
        if next == wanted.len() {
            let score = 100_usize
                .saturating_add(start.unwrap_or_default())
                .saturating_add(gaps.saturating_mul(4))
                .saturating_add(candidate.chars().count());
            return u32::try_from(score).ok();
        }
    }
    None
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
        let mut window = Self {
            open: true,
            sources,
            reference,
            active: None,
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
    pub fn draft_offsets(&self) -> Vec<(SourceId, i64)> {
        self.sources
            .iter()
            .map(|item| (item.source, item.draft_offset_us))
            .collect()
    }
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
        self.picker.is_none()
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
                self.toolbar(ui, snapshot);
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

    fn toolbar(&mut self, ui: &mut egui::Ui, snapshot: &StoreSnapshot) {
        if self.picker.is_some()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.cancel_sample_pick();
        }
        ui.horizontal(|ui| {
            ui.menu_button("Sources", |ui| {
                let ids: Vec<_> = self.sources.iter().map(|item| item.source).collect();
                for id in ids {
                    let mut included = self.source(id).is_some_and(|item| item.included);
                    let label = source_label(snapshot, id);
                    if ui.checkbox(&mut included, label).changed() {
                        let _ = self.set_included(id, included);
                    }
                }
            });
            ui.selectable_value(&mut self.mode, CompareMode::Overlay, "Overlay");
            ui.selectable_value(&mut self.mode, CompareMode::Stacked, "Stacked");
            if ui.button("Reset zoom").clicked() {
                self.fit_selected_plots(snapshot);
            }
            if ui.button("Reset all").clicked() {
                self.reset_all();
            }
        });
        let active = self.active;
        let pair_ready = self.automatic_alignment_ready();
        let reference_label = source_label(snapshot, self.reference);
        let active_label = active
            .map(|active| source_label(snapshot, active))
            .unwrap_or_else(|| "Select active target".to_owned());
        ui.horizontal(|ui| {
            ui.label(format!("{reference_label} → {active_label}"));
            for (label, method) in [
                ("First ↔ First", AutoAlignMethod::FirstToFirst),
                ("Last ↔ Last", AutoAlignMethod::LastToLast),
                ("Back to back", AutoAlignMethod::BackToBack),
                ("First change", AutoAlignMethod::FirstChange),
            ] {
                if ui
                    .add_enabled(pair_ready, egui::Button::new(label))
                    .clicked()
                {
                    let _ = self.align_active(snapshot, method);
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
                " — raw {} us, effective {}, value {}",
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
            interaction.dragged_by(egui::PointerButton::Middle),
        );
        let pan_width = if pointer_action == PlotPointerAction::MiddlePan {
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
            && interaction.drag_started_by(egui::PointerButton::Primary)
        {
            self.drag_start_offset_us = self.active.and_then(|id| self.draft_offset(id));
        }
        if pointer_action == PlotPointerAction::Other
            && self.picker.is_none()
            && interaction.dragged_by(egui::PointerButton::Primary)
            && let Some(active) = self.active
            && active != self.reference
            && let Some(total_drag) = interaction.total_drag_delta()
            && let Some(delta) = drag_delta_us(total_drag.x, plot_rect.width(), view.span_us())
            && let Some(start) = self.drag_start_offset_us
        {
            let _ = self.apply_drag_delta(active, start, delta);
        }
        if interaction.drag_stopped_by(egui::PointerButton::Primary) {
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
            let status = if self.pending_apply.is_some() {
                "Applying…"
            } else {
                match block {
                    None => "Unsaved changes",
                    Some(ApplyBlock::Clean) => "No changes",
                    Some(ApplyBlock::InvalidInput) => "Invalid offset or field",
                    Some(ApplyBlock::Conflict) => "Source offsets changed externally",
                    Some(ApplyBlock::InsufficientSources) => "Include at least two sources",
                }
            };
            ui.label(status);
            if block == Some(ApplyBlock::Conflict) && ui.button("Reload current offsets").clicked()
            {
                self.reload_offsets(snapshot);
            }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::datatypes::DataType;
    use delog_core::identity::{IdentityRegistry, SourceId, SourceKind};
    use delog_core::schema::{FieldSchema, TopicSchema};
    use delog_core::snapshot::StoreSnapshot;
    use delog_core::store::TopicStore;

    use super::*;

    fn fixture_snapshot() -> StoreSnapshot {
        let mut identity = IdentityRegistry::new();
        let mut stores = Vec::new();
        for label in ["a", "b", "c"] {
            let source = identity.add_source_with_kind(label, SourceKind::File);
            let topic = identity.add_topic(source, "DATA").unwrap();
            identity.add_field(topic, "value").unwrap();
            let schema = Arc::new(
                TopicSchema::new(
                    "DATA",
                    [FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap()],
                )
                .unwrap(),
            );
            stores.push((topic, Arc::new(TopicStore::new(schema))));
        }
        StoreSnapshot::from_registry(&identity, stores, 0).unwrap()
    }

    fn multi_source_fixture(series: &[(&str, &[i64], &[f64])]) -> StoreSnapshot {
        let mut identity = IdentityRegistry::new();
        let mut stores = Vec::new();
        for (label, times, values) in series {
            let source = identity.add_source_with_kind(*label, SourceKind::File);
            let topic = identity.add_topic(source, "DATA").unwrap();
            identity.add_field(topic, "value").unwrap();
            let schema = Arc::new(
                TopicSchema::new(
                    "DATA",
                    [FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap()],
                )
                .unwrap(),
            );
            let cols: Vec<arrow::array::ArrayRef> =
                vec![Arc::new(arrow::array::Float64Array::from(values.to_vec()))];
            let chunk = Arc::new(
                delog_core::chunk::Chunk::try_new(
                    arrow::array::Int64Array::from(times.to_vec()),
                    cols,
                    &schema,
                )
                .unwrap(),
            );
            stores.push((
                topic,
                Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
            ));
        }
        StoreSnapshot::from_registry(&identity, stores, 0).unwrap()
    }

    fn multiplier_fixture(series: &[(&str, &[i64], &[f64], f64)]) -> StoreSnapshot {
        let mut identity = IdentityRegistry::new();
        let mut stores = Vec::new();
        for (label, times, values, multiplier) in series {
            let source = identity.add_source_with_kind(*label, SourceKind::File);
            let topic = identity.add_topic(source, "DATA").unwrap();
            identity.add_field(topic, "value").unwrap();
            let schema = Arc::new(
                TopicSchema::new(
                    "DATA",
                    [
                        FieldSchema::new("value", DataType::Float64, None::<String>, *multiplier)
                            .unwrap(),
                    ],
                )
                .unwrap(),
            );
            let cols: Vec<arrow::array::ArrayRef> =
                vec![Arc::new(arrow::array::Float64Array::from(values.to_vec()))];
            let chunk = Arc::new(
                delog_core::chunk::Chunk::try_new(
                    arrow::array::Int64Array::from(times.to_vec()),
                    cols,
                    &schema,
                )
                .unwrap(),
            );
            stores.push((
                topic,
                Arc::new(TopicStore::from_chunks(schema, [chunk]).unwrap()),
            ));
        }
        StoreSnapshot::from_registry(&identity, stores, 0).unwrap()
    }

    fn alignment_fixture() -> StoreSnapshot {
        multi_source_fixture(&[
            ("reference", &[100, 200, 400], &[1.0, 2.0, 3.0]),
            ("target", &[10, 30, 80], &[1.0, 2.0, 3.0]),
            ("untouched", &[7, 9], &[1.0, 2.0]),
        ])
    }

    #[test]
    fn selected_plot_time_range_unions_only_included_fields_with_drafts() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [reference, target, excluded] = sync.included_ids().try_into().unwrap();
        sync.set_draft_offset(target, 1_000).unwrap();
        sync.set_included(excluded, false).unwrap();
        let range = sync.selected_plot_time_range(&snapshot).unwrap();
        assert_eq!((range.min_us, range.max_us), (100, 1_080));
        assert_eq!(sync.reference(), reference);
    }

    #[test]
    fn plot_fit_excludes_multiplier_overflow_rows_but_keeps_later_valid_rows() {
        let snapshot = multiplier_fixture(&[
            ("overflow", &[10, 20], &[f64::MAX, 1.0], 2.0),
            ("valid", &[30], &[3.0], 1.0),
        ]);
        let sync = SyncWindow::open(&snapshot).unwrap();
        let range = sync.selected_plot_time_range(&snapshot).unwrap();
        assert_eq!((range.min_us, range.max_us), (20, 30));
    }

    #[test]
    fn plot_fit_handles_duplicate_and_max_boundary_samples() {
        let snapshot = multi_source_fixture(&[
            ("boundary", &[i64::MAX, i64::MAX], &[1.0, 2.0]),
            ("also-boundary", &[i64::MAX], &[3.0]),
        ]);
        let sync = SyncWindow::open(&snapshot).unwrap();
        assert_eq!(
            sync.view,
            Some(ViewX {
                min_us: i64::MAX - 1,
                max_us: i64::MAX,
            })
        );
    }

    #[test]
    fn unrepresentable_full_domain_fit_preserves_the_current_view() {
        let snapshot = multi_source_fixture(&[
            ("full", &[i64::MIN, i64::MAX], &[1.0, 2.0]),
            ("inside", &[0], &[3.0]),
        ]);
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        sync.view = Some(ViewX::new(10, 20));
        assert!(!sync.fit_selected_plots(&snapshot));
        assert_eq!(sync.view, Some(ViewX::new(10, 20)));
    }

    #[test]
    fn overflowing_trace_is_skipped_while_a_valid_trace_remains_fittable() {
        let snapshot = multi_source_fixture(&[
            ("overflow", &[1, 2], &[1.0, 2.0]),
            ("valid", &[20, 40], &[1.0, 2.0]),
        ]);
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [overflow, _valid] = sync.included_ids().try_into().unwrap();
        sync.source_mut(overflow).unwrap().draft_offset_us = i64::MAX;

        assert!(sync.fit_selected_plots(&snapshot));
        assert_eq!(sync.view, Some(ViewX::new(20, 40)));
    }

    #[test]
    fn changed_reference_draft_is_included_in_plot_fit() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let reference = sync.reference();
        sync.source_mut(reference).unwrap().draft_offset_us = 1_000;

        assert!(sync.fit_selected_plots(&snapshot));
        assert_eq!(sync.view, Some(ViewX::new(7, 1_400)));
    }

    #[test]
    fn plot_fit_skips_a_field_that_does_not_belong_to_its_source() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [first, second, third] = sync.included_ids().try_into().unwrap();
        let unrelated = sync.source(second).unwrap().field.unwrap();
        sync.source_mut(first).unwrap().field = Some(unrelated);
        sync.source_mut(first).unwrap().draft_offset_us = 10_000;
        sync.set_included(third, false).unwrap();

        assert!(sync.fit_selected_plots(&snapshot));
        assert_eq!(sync.view, Some(ViewX::new(10, 80)));
    }

    #[test]
    fn plot_pointer_precedence_is_double_then_middle_then_other_actions() {
        assert_eq!(
            plot_pointer_action(true, true),
            PlotPointerAction::DoubleClickFit
        );
        assert_eq!(
            plot_pointer_action(false, true),
            PlotPointerAction::MiddlePan
        );
        assert_eq!(plot_pointer_action(false, false), PlotPointerAction::Other);
    }

    #[test]
    fn accepted_view_actions_update_the_single_current_view() {
        let mut view = final_frame_views(
            ViewX::new(0, 100),
            PlotPointerAction::MiddlePan,
            None,
            10.0,
            100.0,
        )
        .trace_projection;
        assert_eq!(view, ViewX::new(-10, 90));

        view = gpu::zoom_drag_view(view, 0.0, 100.0, 25.0, 75.0).unwrap();
        assert_eq!(view, ViewX::new(15, 65));
    }

    #[test]
    fn accepted_navigation_supplies_one_final_view_to_trace_and_picker_paths() {
        let initial = ViewX::new(0, 100);
        let middle = final_frame_views(initial, PlotPointerAction::MiddlePan, None, 10.0, 100.0);
        assert_eq!(middle.trace_projection, ViewX::new(-10, 90));
        assert_eq!(middle.picker_projection, middle.trace_projection);

        let fitted = ViewX::new(1_000, 2_000);
        let double = final_frame_views(
            initial,
            PlotPointerAction::DoubleClickFit,
            Some(fitted),
            0.0,
            100.0,
        );
        assert_eq!(double.trace_projection, fitted);
        assert_eq!(double.picker_projection, double.trace_projection);
    }

    #[test]
    fn failed_plot_fit_preserves_the_current_view() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        sync.view = Some(ViewX::new(10, 20));
        assert!(!sync.fit_selected_plots(&snapshot));
        assert_eq!(sync.view, Some(ViewX::new(10, 20)));
    }

    fn change_fixture() -> StoreSnapshot {
        multi_source_fixture(&[
            ("reference", &[100, 200, 300], &[0.0, 0.0, 5.0]),
            ("target", &[10, 20, 40], &[8.0, 8.0, 9.0]),
            ("untouched", &[7, 9], &[1.0, 2.0]),
        ])
    }

    fn snapshot_with_offset(
        snapshot: &StoreSnapshot,
        source: SourceId,
        offset: i64,
    ) -> StoreSnapshot {
        let mut changed = snapshot.clone();
        let mut sources = changed.sources.to_vec();
        sources[source.index()].entry.offset_us = offset;
        changed.sources = Arc::from(sources);
        changed
    }

    #[test]
    fn automatic_methods_update_only_the_active_target() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [reference, target, untouched] = sync.included_ids().try_into().unwrap();
        sync.set_active(target).unwrap();

        sync.align_active(&snapshot, AutoAlignMethod::FirstToFirst)
            .unwrap();
        assert_eq!(sync.draft_offset(target), Some(90));
        assert_eq!(sync.draft_offset(untouched), Some(0));

        sync.align_active(&snapshot, AutoAlignMethod::LastToLast)
            .unwrap();
        assert_eq!(sync.draft_offset(target), Some(320));

        sync.align_active(&snapshot, AutoAlignMethod::BackToBack)
            .unwrap();
        assert_eq!(sync.draft_offset(target), Some(390));
        assert_eq!(sync.reference(), reference);
    }

    #[test]
    fn picker_accepts_reference_then_target_and_aligns_only_target() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [reference, target, untouched] = sync.included_ids().try_into().unwrap();
        sync.set_active(target).unwrap();
        sync.begin_sample_pick().unwrap();
        assert_eq!(sync.pick_expected_source(), Some(reference));

        sync.accept_picked_sample(
            reference,
            SyncSample {
                row: 1,
                raw_time_us: 200,
                value: 0.0,
            },
        )
        .unwrap();
        assert_eq!(sync.pick_expected_source(), Some(target));
        sync.accept_picked_sample(
            target,
            SyncSample {
                row: 1,
                raw_time_us: 30,
                value: 0.0,
            },
        )
        .unwrap();

        assert_eq!(sync.draft_offset(target), Some(170));
        assert_eq!(sync.draft_offset(untouched), Some(0));
        assert_eq!(sync.pick_expected_source(), None);
    }

    #[test]
    fn nearest_projected_sample_uses_radius_distance_and_row_tie_breaking() {
        let source = SourceId(0);
        let candidate = |row, x, y| ProjectedSample {
            source,
            sample: SyncSample {
                row,
                raw_time_us: row as i64,
                value: row as f64,
            },
            position: egui::pos2(x, y),
        };
        let candidates = [
            candidate(2, 10.0, 10.0),
            candidate(1, 20.0, 20.0),
            candidate(3, 30.0, 30.0),
        ];

        assert_eq!(
            nearest_projected_sample(egui::pos2(18.0, 18.0), candidates, 7.0)
                .map(|picked| picked.sample.row),
            Some(1)
        );
        assert_eq!(
            nearest_projected_sample(egui::pos2(15.0, 15.0), candidates, 7.1)
                .map(|picked| picked.sample.row),
            Some(1)
        );
        assert_eq!(
            nearest_projected_sample(egui::pos2(100.0, 100.0), candidates, 7.0),
            None
        );
    }

    #[test]
    fn exact_interior_sample_is_a_projected_candidate_and_wins_the_click() {
        let snapshot = multi_source_fixture(&[
            ("reference", &[0, 1_000_000, 2_000_000], &[0.0, 1.0, 2.0]),
            ("target", &[0, 1_000_000, 2_000_000], &[0.0, 1.0, 2.0]),
        ]);
        let sync = SyncWindow::open(&snapshot).unwrap();
        let source = sync.reference();
        let field = sync.source(source).unwrap().field.unwrap();
        let cache = TraceCache::build(
            &snapshot,
            field,
            0,
            0,
            &delog_core::metrics::MetricsRegistry::new(),
        )
        .unwrap();

        let rows = projected_candidate_rows_in_x_range(&cache, 1.0, 1.0);
        assert_eq!(rows, vec![1], "the exact stored row must be eligible");
        let candidates = rows.into_iter().map(|row| ProjectedSample {
            source,
            sample: SyncSample {
                row,
                raw_time_us: row as i64 * 1_000_000,
                value: row as f64,
            },
            position: egui::pos2(cache.xy[row * 2], cache.xy[row * 2 + 1]),
        });
        assert_eq!(
            nearest_projected_sample(egui::pos2(1.0, 1.0), candidates, 0.1)
                .map(|picked| picked.sample.row),
            Some(1)
        );
    }

    #[test]
    fn farther_x_sample_within_hit_radius_can_win_in_screen_space() {
        let snapshot = multi_source_fixture(&[
            ("reference", &[0, 1_000_000], &[100.0, 0.0]),
            ("target", &[0, 1_000_000], &[0.0, 0.0]),
        ]);
        let sync = SyncWindow::open(&snapshot).unwrap();
        let source = sync.reference();
        let field = sync.source(source).unwrap().field.unwrap();
        let cache = TraceCache::build(
            &snapshot,
            field,
            0,
            0,
            &delog_core::metrics::MetricsRegistry::new(),
        )
        .unwrap();

        let rows = projected_candidate_rows_in_x_range(&cache, 0.0, 1.0);
        assert_eq!(rows, vec![0, 1]);
        let candidates = [
            ProjectedSample {
                source,
                sample: SyncSample {
                    row: rows[0],
                    raw_time_us: 0,
                    value: 100.0,
                },
                position: egui::pos2(1.0, 100.0),
            },
            ProjectedSample {
                source,
                sample: SyncSample {
                    row: rows[1],
                    raw_time_us: 1_000_000,
                    value: 0.0,
                },
                position: egui::pos2(6.0, 0.0),
            },
        ];
        assert_eq!(
            nearest_projected_sample(egui::pos2(0.0, 0.0), candidates, 7.0)
                .map(|sample| sample.sample.row),
            Some(1),
            "the immediate-X row is vertically far, so the farther-X row must win"
        );
    }

    #[test]
    fn stacked_picker_rejects_pointer_just_inside_adjacent_lane() {
        let expected_lane = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
        let adjacent_pointer = egui::pos2(50.0, 50.1);

        assert_eq!(
            pointer_fraction_in_lane(expected_lane, adjacent_pointer),
            None
        );
    }

    #[test]
    fn picker_rejects_out_of_order_sources_without_mutation() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [_, target, _] = sync.included_ids().try_into().unwrap();
        sync.set_active(target).unwrap();
        sync.begin_sample_pick().unwrap();
        let before = sync.draft_offsets();
        assert_eq!(
            sync.accept_picked_sample(
                target,
                SyncSample {
                    row: 0,
                    raw_time_us: 10,
                    value: 0.0,
                },
            ),
            Err(PickError::UnexpectedSource),
        );
        assert_eq!(sync.draft_offsets(), before);
    }

    #[test]
    fn pair_or_field_changes_cancel_an_incomplete_pick() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [reference, target, other] = sync.included_ids().try_into().unwrap();
        sync.set_active(target).unwrap();
        sync.begin_sample_pick().unwrap();
        sync.set_active(other).unwrap();
        assert_eq!(sync.pick_expected_source(), None);

        sync.set_active(target).unwrap();
        sync.begin_sample_pick().unwrap();
        sync.set_reference(other).unwrap();
        assert_eq!(sync.pick_expected_source(), None);
        assert_ne!(reference, other);
    }

    #[test]
    fn first_change_alignment_and_failures_preserve_the_previous_draft() {
        let snapshot = change_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let target = sync.first_movable().unwrap();
        sync.set_active(target).unwrap();
        sync.align_active(&snapshot, AutoAlignMethod::FirstChange)
            .unwrap();
        assert_eq!(sync.draft_offset(target), Some(260));

        sync.source_mut(target).unwrap().field = None;
        assert_eq!(
            sync.align_active(&snapshot, AutoAlignMethod::FirstToFirst),
            Err(AlignmentError::FieldUnavailable)
        );
        assert_eq!(sync.draft_offset(target), Some(260));
    }

    #[test]
    fn automatic_alignment_respects_the_reference_draft_offset() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [reference, target, _] = sync.included_ids().try_into().unwrap();
        sync.source_mut(reference).unwrap().draft_offset_us = 50;
        sync.set_active(target).unwrap();
        sync.align_active(&snapshot, AutoAlignMethod::FirstToFirst)
            .unwrap();
        assert_eq!(sync.draft_offset(target), Some(140));
    }

    #[test]
    fn changed_reference_remains_dirty_and_is_applied_with_dependent_target() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [original_reference, changed_reference, target] =
            sync.included_ids().try_into().unwrap();

        sync.set_draft_offset(changed_reference, 50).unwrap();
        sync.set_reference(changed_reference).unwrap();
        sync.set_active(target).unwrap();
        sync.align_active(&snapshot, AutoAlignMethod::FirstToFirst)
            .unwrap();

        assert!(
            sync.is_dirty(),
            "the changed current reference must warn on close"
        );
        assert_eq!(
            sync.apply_request(&snapshot).unwrap(),
            vec![(changed_reference, 50), (target, 53)]
        );
        assert_eq!(sync.draft_offset(original_reference), Some(0));
    }

    #[test]
    fn automatic_toolbar_actions_are_not_ready_during_sample_picking() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let target = sync.first_movable().unwrap();
        sync.set_active(target).unwrap();
        assert!(sync.automatic_alignment_ready());

        sync.begin_sample_pick().unwrap();

        assert!(!sync.automatic_alignment_ready());
        assert_eq!(sync.pick_expected_source(), Some(sync.reference()));
    }

    #[test]
    fn rendered_and_legend_trace_colors_equal_the_standard_palette() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let traces = sync.rendered_sync_traces(&snapshot);
        assert!(traces.len() >= 3);

        for (index, rendered) in traces.iter().enumerate().take(3) {
            let expected = delog_render::palette::trace_color(index);
            assert_eq!(rendered.trace.color, expected.to_srgb_f32());
            assert_eq!(
                egui_trace_color(index),
                egui::Color32::from_rgba_unmultiplied(
                    expected.r, expected.g, expected.b, expected.a
                )
            );
        }
    }

    #[test]
    fn changing_reference_preserves_effective_drafts() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [a, b, c] = sync.included_ids().try_into().unwrap();
        sync.set_draft_offset(b, 500_000).unwrap();
        let before = sync.draft_offsets();
        sync.set_reference(c).unwrap();
        assert_eq!(sync.draft_offsets(), before);
        assert_eq!(sync.relative_offset(c), Some(0));
        assert_eq!(sync.reference(), c);
        assert_ne!(a, c);
    }

    #[test]
    fn reference_rejects_direct_draft_edits_without_mutation() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let reference = sync.reference();
        let before_draft = sync.draft_offset(reference);
        let before_input = sync.input(reference).map(str::to_owned);

        assert_eq!(sync.set_draft_offset(reference, 99), Err(()));
        assert_eq!(sync.draft_offset(reference), before_draft);
        assert_eq!(sync.input(reference), before_input.as_deref());
    }

    #[test]
    fn reference_rejects_input_edits_without_mutation() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let reference = sync.reference();
        let before_draft = sync.draft_offset(reference);
        let before_input = sync.input(reference).map(str::to_owned);

        assert_eq!(sync.set_input(reference, "99"), Err(()));
        assert_eq!(sync.draft_offset(reference), before_draft);
        assert_eq!(sync.input(reference), before_input.as_deref());
    }

    #[test]
    fn apply_request_omits_unchanged_reference_and_detects_conflict() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let reference = sync.reference();
        let movable = sync.first_movable().unwrap();
        sync.set_draft_offset(movable, 42).unwrap();
        let request = sync.apply_request(&snapshot).unwrap();
        assert_eq!(request, vec![(movable, 42)]);
        assert!(!request.iter().any(|(id, _)| *id == reference));
        let changed = snapshot_with_offset(&snapshot, movable, 10);
        assert_eq!(sync.apply_request(&changed), Err(ApplyBlock::Conflict));
    }

    #[test]
    fn exclusion_moves_reference_and_requires_two_sources() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [a, b, c] = sync.included_ids().try_into().unwrap();
        sync.set_included(a, false).unwrap();
        assert_eq!(sync.reference(), b);
        sync.set_included(c, false).unwrap();
        assert_eq!(
            sync.apply_request(&snapshot),
            Err(ApplyBlock::InsufficientSources)
        );
        assert_eq!(sync.set_included(b, false), Err(()));
        assert_eq!(sync.included_ids(), vec![b]);
        assert_eq!(sync.reference(), b);
    }

    #[test]
    fn reconcile_removes_sources_clears_missing_fields_and_excludes_new_sources() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [a, b, c] = sync.included_ids().try_into().unwrap();
        let mut changed = snapshot.clone();
        let mut sources = changed.sources.to_vec();
        sources[a.index()].entry.removed = true;
        let new_id = SourceId(sources.len() as u32);
        let mut added = sources[c.index()].clone();
        added.entry.id = new_id;
        added.entry.label = "new".into();
        sources.push(added);
        changed.sources = Arc::from(sources);
        let mut fields = changed.fields.to_vec();
        fields[sync.source(b).unwrap().field.unwrap().index()].removed = true;
        changed.fields = Arc::from(fields);
        sync.reconcile(&changed);
        assert!(sync.source(a).is_none());
        assert_eq!(sync.source(b).unwrap().field, None);
        assert_eq!(sync.source(new_id).unwrap().included, false);
    }

    #[test]
    fn file_sources_with_plottable_fields_are_the_only_offered_sources() {
        let mut identity = IdentityRegistry::new();
        let file = identity.add_source_with_kind("file", SourceKind::File);
        let live = identity.add_source_with_kind("live", SourceKind::Live);
        let text = identity.add_source_with_kind("text", SourceKind::File);
        let mut stores = Vec::new();
        for (source, dtype) in [
            (file, DataType::Float64),
            (live, DataType::Float64),
            (text, DataType::Utf8),
        ] {
            let topic = identity.add_topic(source, "DATA").unwrap();
            identity.add_field(topic, "value").unwrap();
            let schema = Arc::new(
                TopicSchema::new(
                    "DATA",
                    [FieldSchema::new("value", dtype, None::<String>, 1.0).unwrap()],
                )
                .unwrap(),
            );
            stores.push((topic, Arc::new(TopicStore::new(schema))));
        }
        let snapshot = StoreSnapshot::from_registry(&identity, stores, 0).unwrap();
        let sync = SyncWindow::open(&snapshot).expect("both file sources remain available");
        assert_eq!(sync.included_ids(), vec![file, text]);
        assert_eq!(sync.source(text).unwrap().field, None);
        assert_eq!(sync.apply_block(&snapshot), Some(ApplyBlock::InvalidInput));
    }

    #[test]
    fn excluded_or_unavailable_sources_cannot_activate_or_move_and_active_falls_back() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [_, b, c] = sync.included_ids().try_into().unwrap();
        sync.set_active(b).unwrap();
        sync.set_included(b, false).unwrap();
        assert_eq!(sync.active, Some(c));
        assert_eq!(sync.set_active(b), Err(()));
        assert_eq!(sync.set_draft_offset(b, 99), Err(()));
        assert_eq!(sync.draft_offset(b), Some(0));

        sync.source_mut(c).unwrap().field = None;
        assert_eq!(sync.set_active(c), Err(()));
        assert_eq!(sync.set_draft_offset(c, 99), Err(()));
    }

    #[test]
    fn preview_delta_uses_current_snapshot_offset_and_reports_overflow() {
        assert_eq!(preview_delta_us(120, 70), Ok(50));
        assert_eq!(preview_delta_us(i64::MIN, i64::MAX), Err(OffsetMathError));
    }

    #[test]
    fn rendered_trace_mapping_omits_overflowing_preceding_source_from_lane_indices() {
        let snapshot = alignment_fixture();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [omitted, expected_first_lane, expected_second_lane] =
            sync.included_ids().try_into().unwrap();
        sync.source_mut(omitted).unwrap().draft_offset_us = i64::MIN;
        let changed = snapshot_with_offset(&snapshot, omitted, i64::MAX);

        let rendered = sync.rendered_sync_traces(&changed);

        assert_eq!(
            rendered
                .iter()
                .map(|trace| trace.source)
                .collect::<Vec<_>>(),
            vec![expected_first_lane, expected_second_lane]
        );
        assert_eq!(
            rendered[0].trace.field,
            sync.source(expected_first_lane).unwrap().field.unwrap()
        );
    }

    #[test]
    fn rejected_pending_apply_clears_only_on_a_later_epoch() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let movable = sync.first_movable().unwrap();
        sync.set_draft_offset(movable, 42).unwrap();
        sync.begin_apply(vec![(movable, 42)], snapshot.epoch);
        sync.reconcile(&snapshot);
        assert!(
            sync.pending_apply.is_some(),
            "dispatch epoch cannot acknowledge"
        );

        let mut later = snapshot.clone();
        later.epoch += 1;
        sync.reconcile(&later);
        assert!(sync.pending_apply.is_none());
        assert_eq!(sync.apply_block(&later), Some(ApplyBlock::Conflict));
    }

    #[test]
    fn failed_apply_dispatch_is_immediately_reloadable() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let movable = sync.first_movable().unwrap();
        sync.set_draft_offset(movable, 42).unwrap();
        sync.begin_apply(vec![(movable, 42)], snapshot.epoch);
        sync.apply_dispatch_failed();
        assert!(sync.pending_apply.is_none());
        assert_eq!(sync.apply_block(&snapshot), Some(ApplyBlock::Conflict));
        sync.reload_offsets(&snapshot);
        assert_eq!(sync.apply_block(&snapshot), Some(ApplyBlock::Clean));
    }

    #[test]
    fn checked_drag_overflow_preserves_draft_and_marks_source_invalid() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let movable = sync.first_movable().unwrap();
        sync.set_draft_offset(movable, i64::MAX).unwrap();
        assert_eq!(
            sync.apply_drag_delta(movable, i64::MAX, 1),
            Err(OffsetMathError)
        );
        assert_eq!(sync.draft_offset(movable), Some(i64::MAX));
        assert!(!sync.source(movable).unwrap().input.valid);
    }

    #[test]
    fn removed_field_does_not_shift_schema_alignment() {
        let mut identity = IdentityRegistry::new();
        let source = identity.add_source_with_kind("file", SourceKind::File);
        let topic = identity.add_topic(source, "DATA").unwrap();
        let removed = identity.add_field(topic, "text").unwrap();
        let numeric = identity.add_field(topic, "value").unwrap();
        let schema = Arc::new(
            TopicSchema::new(
                "DATA",
                [
                    FieldSchema::new("text", DataType::Utf8, None::<String>, 1.0).unwrap(),
                    FieldSchema::new("value", DataType::Float64, None::<String>, 1.0).unwrap(),
                ],
            )
            .unwrap(),
        );
        let mut snapshot = StoreSnapshot::from_registry(
            &identity,
            [(topic, Arc::new(TopicStore::new(schema)))],
            0,
        )
        .unwrap();
        let mut fields = snapshot.fields.to_vec();
        fields[removed.index()].removed = true;
        snapshot.fields = Arc::from(fields);
        assert_eq!(
            first_plottable_field_in_topic(&snapshot, source, topic),
            Some(numeric)
        );
    }

    #[test]
    fn topic_selection_scopes_fields_and_selects_the_first_plottable_field() {
        let mut identity = IdentityRegistry::new();
        let first_source = identity.add_source_with_kind("first", SourceKind::File);
        let second_source = identity.add_source_with_kind("second", SourceKind::File);
        let primary = identity.add_topic(first_source, "PRIMARY").unwrap();
        let primary_field = identity.add_field(primary, "value").unwrap();
        let secondary = identity.add_topic(first_source, "SECONDARY").unwrap();
        let secondary_field = identity.add_field(secondary, "other").unwrap();
        let peer = identity.add_topic(second_source, "PEER").unwrap();
        identity.add_field(peer, "value").unwrap();

        let schema = |name: &str, field: &str| {
            Arc::new(
                TopicSchema::new(
                    name,
                    [FieldSchema::new(field, DataType::Float64, None::<String>, 1.0).unwrap()],
                )
                .unwrap(),
            )
        };
        let snapshot = StoreSnapshot::from_registry(
            &identity,
            [
                (
                    primary,
                    Arc::new(TopicStore::new(schema("PRIMARY", "value"))),
                ),
                (
                    secondary,
                    Arc::new(TopicStore::new(schema("SECONDARY", "other"))),
                ),
                (peer, Arc::new(TopicStore::new(schema("PEER", "value")))),
            ],
            0,
        )
        .unwrap();
        let mut sync = SyncWindow::open(&snapshot).unwrap();

        assert_eq!(sync.source(first_source).unwrap().topic, Some(primary));
        assert_eq!(
            sync.source(first_source).unwrap().field,
            Some(primary_field)
        );
        sync.set_topic(&snapshot, first_source, secondary).unwrap();
        assert_eq!(sync.source(first_source).unwrap().topic, Some(secondary));
        assert_eq!(
            sync.source(first_source).unwrap().field,
            Some(secondary_field)
        );
        assert_eq!(
            plottable_fields(&snapshot, first_source, secondary),
            vec![secondary_field]
        );
        assert_eq!(sync.set_topic(&snapshot, first_source, peer), Err(()));
    }

    #[test]
    fn fuzzy_field_search_matches_tokens_and_ranks_tighter_paths_first() {
        let tight = fuzzy_match_score("gps lat", "GPS › latitude").unwrap();
        let loose = fuzzy_match_score("gps lat", "MyGpsTopic › vehicle_latitude_raw").unwrap();
        assert!(tight < loose);
        assert!(fuzzy_match_score("gpt lat", "GPS › latitude").is_some());
        assert_eq!(fuzzy_match_score("gyro", "GPS › latitude"), None);
    }

    #[test]
    fn fuzzy_result_selects_topic_and_field_together() {
        let mut identity = IdentityRegistry::new();
        let first = identity.add_source_with_kind("first", SourceKind::File);
        let second = identity.add_source_with_kind("second", SourceKind::File);
        let attitude = identity.add_topic(first, "ATTITUDE").unwrap();
        identity.add_field(attitude, "roll").unwrap();
        let gps = identity.add_topic(first, "GPS").unwrap();
        let latitude = identity.add_field(gps, "latitude").unwrap();
        let peer = identity.add_topic(second, "PEER").unwrap();
        identity.add_field(peer, "value").unwrap();
        let schema = |name: &str, field: &str| {
            Arc::new(
                TopicSchema::new(
                    name,
                    [FieldSchema::new(field, DataType::Float64, None::<String>, 1.0).unwrap()],
                )
                .unwrap(),
            )
        };
        let snapshot = StoreSnapshot::from_registry(
            &identity,
            [
                (
                    attitude,
                    Arc::new(TopicStore::new(schema("ATTITUDE", "roll"))),
                ),
                (gps, Arc::new(TopicStore::new(schema("GPS", "latitude")))),
                (peer, Arc::new(TopicStore::new(schema("PEER", "value")))),
            ],
            0,
        )
        .unwrap();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let result = field_search_results(&snapshot, first, "gp lat")
            .into_iter()
            .next()
            .unwrap();

        sync.select_search_result(first, result).unwrap();
        assert_eq!(sync.source(first).unwrap().topic, Some(gps));
        assert_eq!(sync.source(first).unwrap().field, Some(latitude));
    }

    #[test]
    fn reset_and_apply_lifecycle_tracks_clean_dirty_and_input_validity() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let [_, b, c] = sync.included_ids().try_into().unwrap();
        assert_eq!(sync.apply_request(&snapshot), Err(ApplyBlock::Clean));
        sync.set_draft_offset(b, 5).unwrap();
        sync.set_draft_offset(c, 7).unwrap();
        assert!(sync.is_dirty());
        sync.reset_one(b).unwrap();
        assert_eq!(sync.draft_offset(b), Some(0));
        sync.reset_all();
        assert!(!sync.is_dirty());
        sync.set_input(b, "bad").unwrap();
        assert_eq!(sync.apply_request(&snapshot), Err(ApplyBlock::InvalidInput));
        sync.set_input(b, "12").unwrap();
        assert_eq!(sync.draft_offset(b), Some(12));
        let applied = snapshot_with_offset(&snapshot, b, 12);
        sync.mark_applied(&applied);
        assert!(!sync.is_dirty());
        assert_eq!(sync.input(b), Some("12"));
    }

    #[test]
    fn reference_controls_are_disabled_and_apply_tracks_policy() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        assert!(!sync.controls(sync.reference()).movable);
        assert_eq!(sync.apply_block(&snapshot), Some(ApplyBlock::Clean));
        let movable = sync.first_movable().unwrap();
        sync.set_draft_offset(movable, 1).unwrap();
        assert_eq!(sync.apply_block(&snapshot), None);
    }

    #[test]
    fn view_toggle_preserves_alignment_state() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let movable = sync.first_movable().unwrap();
        sync.set_draft_offset(movable, 77).unwrap();
        sync.set_mode(CompareMode::Stacked);
        assert_eq!(sync.draft_offset(movable), Some(77));
    }

    #[test]
    fn overlay_geometry_uses_one_padded_union() {
        let raw = [
            Some(PreparedYRange::new(0.0, 0.0, 10.0).unwrap()),
            Some(PreparedYRange::new(100.0, 0.0, 100.0).unwrap()),
        ];
        assert_eq!(
            prepared_y_ranges(CompareMode::Overlay, &raw),
            vec![
                PreparedYRange::new(0.0, -10.0, 210.0).unwrap(),
                PreparedYRange::new(0.0, -10.0, 210.0).unwrap(),
            ]
        );
    }

    #[test]
    fn stacked_geometry_pads_each_lane_independently() {
        let raw = [
            Some(PreparedYRange::new(0.0, 0.0, 10.0).unwrap()),
            Some(PreparedYRange::new(100.0, 0.0, 100.0).unwrap()),
        ];
        assert_eq!(
            prepared_y_ranges(CompareMode::Stacked, &raw),
            vec![
                PreparedYRange::new(0.0, -0.5, 10.5).unwrap(),
                PreparedYRange::new(100.0, -5.0, 105.0).unwrap(),
            ]
        );
    }

    #[test]
    fn stacked_geometry_keeps_ready_traces_without_visible_samples() {
        let raw = [Some(PreparedYRange::new(10.0, 2.0, 4.0).unwrap()), None];
        assert_eq!(
            prepared_y_ranges(CompareMode::Stacked, &raw),
            vec![
                PreparedYRange::new(10.0, 1.9, 4.1).unwrap(),
                PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),
            ]
        );
    }

    #[test]
    fn overlay_geometry_uses_finite_union_and_keeps_ready_empty_traces() {
        let raw = [None, Some(PreparedYRange::new(100.0, 2.0, 4.0).unwrap())];
        assert_eq!(
            prepared_y_ranges(CompareMode::Overlay, &raw),
            vec![
                PreparedYRange::new(100.0, 1.9, 4.1).unwrap(),
                PreparedYRange::new(100.0, 1.9, 4.1).unwrap(),
            ]
        );

        let all_empty = [None, None];
        assert_eq!(
            prepared_y_ranges(CompareMode::Overlay, &all_empty),
            vec![
                PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),
                PreparedYRange::new(0.0, -1.0, 1.0).unwrap(),
            ]
        );
    }

    #[test]
    fn preparation_repaint_stops_after_relevant_cache_build_finishes() {
        assert!(preparation_needs_repaint([true, false]));
        assert!(!preparation_needs_repaint([false, false]));
    }

    #[test]
    fn overlay_flat_padding_survives_large_origin() {
        let raw = [Some(PreparedYRange::new(1.0e20, 0.0, 0.0).unwrap())];
        let ranges = prepared_y_ranges(CompareMode::Overlay, &raw);
        assert_eq!(ranges[0].span(), 2.0);
    }

    #[test]
    fn stacked_flat_padding_survives_large_origin() {
        let raw = [Some(PreparedYRange::new(1.0e20, 0.0, 0.0).unwrap())];
        let ranges = prepared_y_ranges(CompareMode::Stacked, &raw);
        assert_eq!(ranges[0].span(), 2.0);
    }

    #[test]
    fn overlay_and_stacked_keep_single_trace_large_origin_geometry_in_parity() {
        let raw = [Some(PreparedYRange::new(1.0e20, -4.0, 6.0).unwrap())];
        assert_eq!(
            prepared_y_ranges(CompareMode::Overlay, &raw),
            prepared_y_ranges(CompareMode::Stacked, &raw),
        );
        assert_eq!(
            prepared_y_ranges(CompareMode::Overlay, &raw)[0].span(),
            11.0
        );
    }

    #[test]
    fn invalid_exact_edit_counts_as_dirty_for_close_policy() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let movable = sync.first_movable().unwrap();
        sync.set_input(movable, "bad").unwrap();
        assert!(sync.is_dirty());
    }

    #[test]
    fn conflict_reload_captures_current_offsets_and_preserves_presentation() {
        let snapshot = fixture_snapshot();
        let mut sync = SyncWindow::open(&snapshot).unwrap();
        let reference = sync.reference();
        let movable = sync.first_movable().unwrap();
        let field = sync.source(movable).unwrap().field;
        sync.set_mode(CompareMode::Stacked);
        sync.set_draft_offset(movable, 77).unwrap();
        let changed = snapshot_with_offset(&snapshot, movable, 42);
        assert_eq!(sync.apply_block(&changed), Some(ApplyBlock::Conflict));

        sync.reload_offsets(&changed);

        assert_eq!(sync.apply_block(&changed), Some(ApplyBlock::Clean));
        assert!(!sync.is_dirty());
        assert_eq!(sync.draft_offset(movable), Some(42));
        assert_eq!(sync.reference(), reference);
        assert_eq!(sync.source(movable).unwrap().field, field);
        assert_eq!(sync.mode, CompareMode::Stacked);
    }

    #[test]
    fn conflict_footer_exposes_reload_current_offsets_control() {
        let source = include_str!("sync_window.rs");
        assert!(source.contains("Reload current offsets"));
    }

    #[test]
    fn overlay_hit_test_selects_nearest_trace_and_misses_outside_threshold() {
        let traces = [
            OverlayHitSegment::new(0, egui::pos2(0.0, 10.0), egui::pos2(100.0, 10.0)),
            OverlayHitSegment::new(1, egui::pos2(0.0, 30.0), egui::pos2(100.0, 30.0)),
        ];
        assert_eq!(
            nearest_overlay_trace(egui::pos2(50.0, 11.0), &traces, 6.0),
            Some(0)
        );
        assert_eq!(
            nearest_overlay_trace(egui::pos2(50.0, 28.0), &traces, 6.0),
            Some(1)
        );
        assert_eq!(
            nearest_overlay_trace(egui::pos2(50.0, 50.0), &traces, 6.0),
            None
        );
    }

    #[test]
    fn overlay_hit_test_breaks_equal_distance_ties_by_trace_order() {
        let traces = [
            OverlayHitSegment::new(0, egui::pos2(0.0, 10.0), egui::pos2(100.0, 10.0)),
            OverlayHitSegment::new(1, egui::pos2(0.0, 20.0), egui::pos2(100.0, 20.0)),
        ];
        assert_eq!(
            nearest_overlay_trace(egui::pos2(50.0, 15.0), &traces, 6.0),
            Some(0)
        );
    }

    #[test]
    fn tiny_lane_rejects_pointer_projection() {
        let lane = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0e-7, 100.0));
        assert_eq!(pointer_fraction_in_lane(lane, egui::pos2(0.0, 50.0)), None);
    }

    #[test]
    fn exact_offset_parser_supports_required_units() {
        assert_eq!(parse_offset_us("500 us"), Ok(500));
        assert_eq!(parse_offset_us("-250 ms"), Ok(-250_000));
        assert_eq!(parse_offset_us("1.2 s"), Ok(1_200_000));
        assert_eq!(
            parse_offset_us("0.1 us"),
            Err(OffsetParseError::FractionalMicrosecond)
        );
        assert_eq!(parse_offset_us("1 minute"), Err(OffsetParseError::Unit));
    }

    #[test]
    fn exact_offset_parser_rejects_invalid_and_out_of_range_values() {
        assert_eq!(parse_offset_us("1"), Err(OffsetParseError::Unit));
        assert_eq!(parse_offset_us("1 ms extra"), Err(OffsetParseError::Syntax));
        assert_eq!(parse_offset_us("NaN s"), Err(OffsetParseError::NonFinite));
        assert_eq!(parse_offset_us("1e30 s"), Err(OffsetParseError::Overflow));
    }

    #[test]
    fn offset_formatter_uses_exact_largest_unit_and_round_trips() {
        for (value, formatted) in [
            (2_000_000, "2 s"),
            (-250_000, "-250 ms"),
            (501, "501 us"),
            (0, "0 s"),
            (i64::MAX, "9223372036854775807 us"),
            (i64::MIN, "-9223372036854775808 us"),
        ] {
            assert_eq!(format_offset_us(value), formatted);
            assert_eq!(parse_offset_us(formatted), Ok(value));
        }
    }

    #[test]
    fn drag_follows_visible_span() {
        assert_eq!(drag_delta_us(25.0, 100.0, 1_000_000), Some(250_000));
    }

    #[test]
    fn drag_rejects_invalid_or_overflowing_inputs() {
        assert_eq!(drag_delta_us(f32::NAN, 100.0, 1_000_000), None);
        assert_eq!(drag_delta_us(25.0, 0.0, 1_000_000), None);
        assert_eq!(drag_delta_us(1.0, 1.0e-7, 1_000_000), None);
        assert_eq!(drag_delta_us(25.0, 100.0, 0), None);
        assert_eq!(drag_delta_us(f32::MAX, 1.0, i64::MAX), None);
    }

    #[test]
    fn plot_height_reserves_footer_and_never_grows_with_unbounded_space() {
        assert_eq!(sync_plot_height(800.0), 360.0);
        assert_eq!(sync_plot_height(220.0), 184.0);
        assert_eq!(sync_plot_height(20.0), 1.0);
    }
}

//! Mapping between stable wire commands and native control calls.

mod batch;
mod convert;
mod fields;
mod state;

use convert::{color, ns_to_us, profile_dto};
pub use fields::{
    ControlFieldResolver, LiveFieldResolver, ResolvedControlField, ResolvedControlSource,
};

use delog_api::color::parse_hex_color;
use delog_api::control::{
    AnnotationFilter, AnnotationGeometry, AnnotationRequest, AnnotationStylePatch,
    AuthorizedControlHost, ControlPrincipal, ControlRequest, ControlResponse, GenerationRequest,
    LayoutRequest, MarkerFilter, MarkerPatch, MarkerRequest, PlaybackRequest, ResolvedVehicleField,
    ResourceGuard, SplitDirection, TraceMode, TraceRequest, VehicleFilter, VehicleModel,
    VehicleNedReference, VehicleOrientation, VehiclePatch, VehiclePosition, VehicleProfileRequest,
    VehicleRequest, VehicleSpec, WorkspaceRequest,
};
use delog_api::markers::PendingMarker;

use crate::control::handles::{ControlHandleRegistry, HandleTarget, NativePlotKey};
use crate::handles::OpaqueId;
use crate::protocol::v1::control::*;
use crate::protocol::v1::error::ApiError;

pub struct ControlMapper<'a> {
    host: &'a dyn AuthorizedControlHost,
    principal: ControlPrincipal,
    handles: &'a mut ControlHandleRegistry,
    fields: &'a dyn ControlFieldResolver,
}

impl<'a> ControlMapper<'a> {
    pub fn new(
        host: &'a dyn AuthorizedControlHost,
        principal: ControlPrincipal,
        handles: &'a mut ControlHandleRegistry,
        fields: &'a dyn ControlFieldResolver,
    ) -> Self {
        Self {
            host,
            principal,
            handles,
            fields,
        }
    }

    fn call(&self, request: ControlRequest) -> Result<ControlResponse, ApiError> {
        request.validate().map_err(ApiError::from)?;
        self.host
            .call_as(self.principal.clone(), request)
            .map_err(ApiError::from)
    }

    fn field(&mut self, handle: &OpaqueId) -> Result<ResolvedControlField, ApiError> {
        let field = self.fields.resolve(handle)?;
        self.handles.import_field(
            handle.clone(),
            HandleTarget::Field {
                field: field.id,
                source: field.source.clone(),
                topic: field.topic.clone(),
                name: field.name.clone(),
            },
        );
        Ok(field)
    }

    fn point(point: PointDto) -> Result<(i64, f64), ApiError> {
        Ok((ns_to_us(point.time_ns)?, point.y))
    }

    fn geometry(geometry: AnnotationGeometryDto) -> Result<AnnotationGeometry, ApiError> {
        Ok(match geometry {
            AnnotationGeometryDto::Text { at } => AnnotationGeometry::Text {
                at: Self::point(at)?,
            },
            AnnotationGeometryDto::Segment { from, to } => AnnotationGeometry::Segment {
                from: Self::point(from)?,
                to: Self::point(to)?,
            },
            AnnotationGeometryDto::Rect { a, b } => AnnotationGeometry::Rect {
                a: Self::point(a)?,
                b: Self::point(b)?,
            },
            AnnotationGeometryDto::Ellipse { a, b } => AnnotationGeometry::Ellipse {
                a: Self::point(a)?,
                b: Self::point(b)?,
            },
            AnnotationGeometryDto::HLine { y } => AnnotationGeometry::HLine { y },
        })
    }

    fn style(style: AnnotationStyleDto) -> Result<AnnotationStylePatch, ApiError> {
        AnnotationStylePatch::new(
            color(style.color)?,
            style.stroke_px,
            style.fill_opacity,
            style.font_px,
            style.arrow,
        )
        .map_err(ApiError::from)
    }

    fn vehicle_field(&mut self, handle: &OpaqueId) -> Result<ResolvedVehicleField, ApiError> {
        let field = self.field(handle)?;
        Ok(ResolvedVehicleField {
            id: field.id,
            path: field.vehicle_path,
        })
    }

    fn position(&mut self, value: VehiclePositionDto) -> Result<VehiclePosition, ApiError> {
        Ok(match value {
            VehiclePositionDto::Ned {
                north,
                east,
                down,
                reference,
            } => VehiclePosition::ned(
                self.vehicle_field(&north)?,
                self.vehicle_field(&east)?,
                self.vehicle_field(&down)?,
                reference
                    .map(|value| self.ned_reference(value))
                    .transpose()?,
            )
            .map_err(ApiError::from)?,
            VehiclePositionDto::Gps {
                lat,
                lon,
                alt,
                lat_lon_dege7,
                alt_mm,
                alt_offset_m,
            } => VehiclePosition::gps(
                self.vehicle_field(&lat)?,
                self.vehicle_field(&lon)?,
                self.vehicle_field(&alt)?,
                lat_lon_dege7,
                alt_mm,
                alt_offset_m,
            )
            .map_err(ApiError::from)?,
        })
    }

    fn ned_reference(
        &mut self,
        value: VehicleNedReferenceDto,
    ) -> Result<VehicleNedReference, ApiError> {
        Ok(match value {
            VehicleNedReferenceDto::Manual {
                lat_deg,
                lon_deg,
                alt_m,
            } => VehicleNedReference::manual(lat_deg, lon_deg, alt_m).map_err(ApiError::from)?,
            VehicleNedReferenceDto::Fields { lat, lon, alt } => VehicleNedReference::fields(
                self.vehicle_field(&lat)?,
                self.vehicle_field(&lon)?,
                self.vehicle_field(&alt)?,
            ),
        })
    }

    fn orientation(
        &mut self,
        value: VehicleOrientationDto,
    ) -> Result<VehicleOrientation, ApiError> {
        Ok(match value {
            VehicleOrientationDto::Static => VehicleOrientation::static_orientation(),
            VehicleOrientationDto::Euler {
                roll,
                pitch,
                yaw,
                degrees,
            } => VehicleOrientation::euler(
                self.vehicle_field(&roll)?,
                self.vehicle_field(&pitch)?,
                self.vehicle_field(&yaw)?,
                degrees,
            ),
            VehicleOrientationDto::Quat { w, x, y, z } => VehicleOrientation::quaternion(
                self.vehicle_field(&w)?,
                self.vehicle_field(&x)?,
                self.vehicle_field(&y)?,
                self.vehicle_field(&z)?,
            ),
        })
    }

    fn vehicle_patch(&mut self, patch: VehiclePatchDto) -> Result<VehiclePatch, ApiError> {
        Ok(VehiclePatch {
            label: patch.label,
            show: patch.show,
            show_path: patch.show_path,
            position: patch
                .position
                .map(|value| self.position(value))
                .transpose()?,
            orientation: patch
                .orientation
                .map(|value| self.orientation(value))
                .transpose()?,
            model: patch
                .model
                .map(|value| VehicleModel::parse(&value).map_err(ApiError::from))
                .transpose()?,
            color: color(patch.color)?,
            path_color: color(patch.path_color)?,
            scale: patch.scale,
        })
    }

    fn one_resource(
        &mut self,
        response: ControlResponse,
        kind: &'static str,
    ) -> Result<ControlResultDto, ApiError> {
        let mut window = None;
        let handle = match (kind, response) {
            ("window", ControlResponse::Window(info)) => self.handles.register_window(info.id),
            ("plot", ControlResponse::Plots(infos)) if infos.len() == 1 => {
                window = Some(self.handles.register_window(infos[0].window));
                self.handles.register_plot(NativePlotKey::with_instance_id(
                    infos[0].window,
                    infos[0].tile,
                    infos[0].instance_id,
                ))
            }
            ("annotation", ControlResponse::Annotations(infos)) if infos.len() == 1 => {
                self.handles.register_annotation(
                    NativePlotKey::with_instance_id(
                        infos[0].window,
                        infos[0].tile,
                        infos[0].plot_instance_id,
                    ),
                    infos[0].id,
                )
            }
            ("vehicle", ControlResponse::Vehicles(infos)) if infos.len() == 1 => {
                self.handles.register_vehicle(infos[0].id)
            }
            _ => {
                return Err(ApiError::internal(
                    "native control returned the wrong resource response",
                ));
            }
        };
        Ok(ControlResultDto::Resource { handle, window })
    }

    fn guarded(
        &self,
        guard: ResourceGuard,
        request: ControlRequest,
    ) -> Result<ControlResponse, ApiError> {
        self.call(ControlRequest::Guarded {
            guard,
            request: Box::new(request),
        })
    }

    fn plot_guard(key: NativePlotKey) -> ResourceGuard {
        ResourceGuard::Plot {
            window: key.window,
            tile: key.tile,
            instance_id: key.instance_id,
        }
    }

    pub fn execute(&mut self, command: ControlCommandDto) -> Result<ControlResultDto, ApiError> {
        match command {
            ControlCommandDto::WindowOpen { title } => {
                let response =
                    self.call(ControlRequest::Workspace(WorkspaceRequest::OpenWindow {
                        title,
                        owner: None,
                    }))?;
                self.one_resource(response, "window")
            }
            ControlCommandDto::WorkspaceAddPlot { window, direction } => {
                let window = window
                    .map(|handle| self.handles.resolve_window(&handle))
                    .transpose()?;
                let direction = SplitDirection::parse(&direction).map_err(ApiError::from)?;
                let response = self.call(ControlRequest::Workspace(WorkspaceRequest::AddPlot {
                    window,
                    direction,
                    owner: None,
                }))?;
                self.one_resource(response, "plot")
            }
            ControlCommandDto::WorkspaceSplit { plot, direction } => {
                let key = self.handles.resolve_plot(&plot)?;
                let direction = SplitDirection::parse(&direction).map_err(ApiError::from)?;
                let response = self.guarded(
                    Self::plot_guard(key),
                    ControlRequest::Workspace(WorkspaceRequest::Split {
                        window: key.window,
                        tile: key.tile,
                        direction,
                        owner: None,
                    }),
                )?;
                self.one_resource(response, "plot")
            }
            ControlCommandDto::WorkspaceClose { plot } => {
                let key = self.handles.resolve_plot(&plot)?;
                self.guarded(
                    Self::plot_guard(key),
                    ControlRequest::Workspace(WorkspaceRequest::Close {
                        window: key.window,
                        tile: key.tile,
                    }),
                )?
                .into_unit()
                .map_err(ApiError::from)?;
                self.handles.invalidate_plot(key);
                Ok(ControlResultDto::Unit)
            }
            ControlCommandDto::WorkspaceEqualize { window } => {
                let window = window
                    .map(|handle| self.handles.resolve_window(&handle))
                    .transpose()?;
                self.unit(ControlRequest::Workspace(WorkspaceRequest::Equalize {
                    window,
                }))
            }
            ControlCommandDto::SceneSetVisible { visible } => {
                self.unit(ControlRequest::Workspace(WorkspaceRequest::ShowScene {
                    visible,
                }))
            }
            ControlCommandDto::PlaybackSet { speed, follow_live } => {
                self.unit(ControlRequest::Playback(PlaybackRequest::Set {
                    speed,
                    follow_live,
                }))
            }
            ControlCommandDto::TraceAdd {
                plot,
                field,
                color: trace_color,
                width_px,
                mode,
            } => {
                let key = self.handles.resolve_plot(&plot)?;
                let field = self.field(&field)?;
                let mode = TraceMode::parse(&mode).map_err(ApiError::from)?;
                let info = self
                    .guarded(
                        Self::plot_guard(key),
                        ControlRequest::Traces(TraceRequest::AddReturning {
                            window: key.window,
                            tile: key.tile,
                            field_id: field.id,
                            field: field.trace_path,
                            color: color(trace_color)?,
                            width_px,
                            mode,
                            owner: None,
                        }),
                    )?
                    .into_trace()
                    .map_err(ApiError::from)?;
                Ok(ControlResultDto::Resource {
                    handle: self.handles.register_trace(
                        key,
                        info.index,
                        info.field_id,
                        info.instance_id,
                    ),
                    window: None,
                })
            }
            ControlCommandDto::TraceSet {
                trace,
                color: trace_color,
                width_px,
                mode,
                visible,
            } => {
                let (key, index, field_id, trace_instance_id) =
                    self.handles.resolve_trace(&trace)?;
                let mode = mode
                    .map(|value| TraceMode::parse(&value).map_err(ApiError::from))
                    .transpose()?;
                self.guarded(
                    ResourceGuard::Trace {
                        window: key.window,
                        tile: key.tile,
                        plot_instance_id: key.instance_id,
                        index,
                        trace_instance_id,
                    },
                    ControlRequest::Traces(TraceRequest::Set {
                        window: key.window,
                        tile: key.tile,
                        index,
                        field_id,
                        color: color(trace_color)?,
                        width_px,
                        mode,
                        visible,
                    }),
                )?
                .into_unit()
                .map_err(ApiError::from)?;
                Ok(ControlResultDto::Unit)
            }
            ControlCommandDto::TraceRemove { trace } => {
                let (key, index, _, trace_instance_id) = self.handles.resolve_trace(&trace)?;
                self.guarded(
                    ResourceGuard::Trace {
                        window: key.window,
                        tile: key.tile,
                        plot_instance_id: key.instance_id,
                        index,
                        trace_instance_id,
                    },
                    ControlRequest::Traces(TraceRequest::Remove {
                        window: key.window,
                        tile: key.tile,
                        index: Some(index),
                        field_id: None,
                        field: None,
                    }),
                )?
                .into_unit()
                .map_err(ApiError::from)?;
                self.handles.invalidate_traces(key);
                Ok(ControlResultDto::Unit)
            }
            ControlCommandDto::TraceClear { plot } => {
                let key = self.handles.resolve_plot(&plot)?;
                self.guarded(
                    Self::plot_guard(key),
                    ControlRequest::Traces(TraceRequest::Clear {
                        window: key.window,
                        tile: key.tile,
                    }),
                )?
                .into_unit()
                .map_err(ApiError::from)?;
                self.handles.invalidate_traces(key);
                Ok(ControlResultDto::Unit)
            }
            ControlCommandDto::AnnotationAdd {
                plot,
                geometry,
                label,
                style,
            } => {
                let key = self.handles.resolve_plot(&plot)?;
                let response = self.guarded(
                    Self::plot_guard(key),
                    ControlRequest::Annotations(AnnotationRequest::Add {
                        window: key.window,
                        tile: key.tile,
                        geometry: Self::geometry(geometry)?,
                        label,
                        style: Self::style(style)?,
                        owner: None,
                    }),
                )?;
                self.one_resource(response, "annotation")
            }
            ControlCommandDto::AnnotationSet {
                annotation,
                label,
                geometry,
                style,
            } => {
                let (key, id) = self.handles.resolve_annotation(&annotation)?;
                self.guarded(
                    ResourceGuard::Annotation {
                        window: key.window,
                        tile: key.tile,
                        plot_instance_id: key.instance_id,
                        id,
                    },
                    ControlRequest::Annotations(AnnotationRequest::Set {
                        window: key.window,
                        tile: key.tile,
                        id,
                        label,
                        geometry: geometry.map(Self::geometry).transpose()?,
                        style: Self::style(style)?,
                    }),
                )?
                .into_unit()
                .map_err(ApiError::from)?;
                Ok(ControlResultDto::Unit)
            }
            ControlCommandDto::AnnotationRemove { annotation } => {
                let (key, id) = self.handles.resolve_annotation(&annotation)?;
                self.guarded(
                    ResourceGuard::Annotation {
                        window: key.window,
                        tile: key.tile,
                        plot_instance_id: key.instance_id,
                        id,
                    },
                    ControlRequest::Annotations(AnnotationRequest::Remove {
                        target: Some((key.window, key.tile)),
                        filter: AnnotationFilter::Id(id),
                    }),
                )?
                .into_unit()
                .map_err(ApiError::from)?;
                self.handles.invalidate(&HandleTarget::Annotation {
                    window: key.window,
                    tile: key.tile,
                    id,
                    plot_instance_id: key.instance_id,
                });
                Ok(ControlResultDto::Unit)
            }
            ControlCommandDto::MarkerAdd {
                time_ns,
                label,
                color: marker_color,
                note,
            } => {
                let marker =
                    PendingMarker::new(ns_to_us(time_ns)?, label, marker_color.as_deref(), note)
                        .map_err(ApiError::from)?;
                let info = self
                    .call(ControlRequest::Markers(MarkerRequest::AppendReturning {
                        owner: self.principal.owner.name.clone(),
                        generation: self.principal.owner.generation,
                        marker,
                    }))?
                    .into_marker()
                    .map_err(ApiError::from)?;
                Ok(ControlResultDto::Resource {
                    handle: self.handles.register_marker(info.id),
                    window: None,
                })
            }
            ControlCommandDto::MarkerSet {
                marker,
                time_ns,
                label,
                color: marker_color,
                note,
            } => {
                let id = self.handles.resolve_marker(&marker)?;
                self.unit(ControlRequest::Markers(MarkerRequest::Set {
                    id,
                    patch: MarkerPatch {
                        t_us: time_ns.map(ns_to_us).transpose()?,
                        label,
                        color: color(marker_color)?,
                        note,
                    },
                }))
            }
            ControlCommandDto::MarkerRemove { marker } => {
                let id = self.handles.resolve_marker(&marker)?;
                self.call(ControlRequest::Markers(MarkerRequest::Remove(
                    MarkerFilter::Id(id),
                )))?
                .into_unit()
                .map_err(ApiError::from)?;
                self.handles.invalidate(&HandleTarget::Marker { id });
                Ok(ControlResultDto::Unit)
            }
            ControlCommandDto::VehicleAdd {
                source,
                label,
                show,
                show_path,
                position,
                orientation,
                model,
                color: vehicle_color,
                path_color,
                scale,
            } => {
                let source = self.fields.resolve_source(&source)?;
                let spec = VehicleSpec {
                    source_id: source.id,
                    source: source.label,
                    label,
                    show,
                    show_path,
                    position: self.position(position)?,
                    orientation: self.orientation(orientation)?,
                    model: VehicleModel::parse(&model).map_err(ApiError::from)?,
                    color: parse_hex_color(&vehicle_color).map_err(ApiError::from)?,
                    path_color: parse_hex_color(&path_color).map_err(ApiError::from)?,
                    scale,
                    owner: None,
                };
                let response = self.call(ControlRequest::Vehicles(Box::new(
                    VehicleRequest::Add(spec),
                )))?;
                self.one_resource(response, "vehicle")
            }
            ControlCommandDto::VehicleSet { vehicle, patch } => {
                let id = self.handles.resolve_vehicle(&vehicle)?;
                let patch = self.vehicle_patch(patch)?;
                let response =
                    self.call(ControlRequest::Vehicles(Box::new(VehicleRequest::Set {
                        id,
                        patch,
                    })))?;
                self.one_resource(response, "vehicle")
            }
            ControlCommandDto::VehicleRemove { vehicle } => {
                let id = self.handles.resolve_vehicle(&vehicle)?;
                self.call(ControlRequest::Vehicles(Box::new(VehicleRequest::Remove(
                    VehicleFilter::Id(id),
                ))))?
                .into_unit()
                .map_err(ApiError::from)?;
                self.handles.invalidate(&HandleTarget::Vehicle { id });
                Ok(ControlResultDto::Unit)
            }
            ControlCommandDto::VehicleProfileList => {
                self.names(ControlRequest::VehicleProfiles(VehicleProfileRequest::List))
            }
            ControlCommandDto::VehicleProfileSave { name, vehicle } => {
                let vehicle_id = self.handles.resolve_vehicle(&vehicle)?;
                self.unit(ControlRequest::VehicleProfiles(
                    VehicleProfileRequest::Save { name, vehicle_id },
                ))
            }
            ControlCommandDto::VehicleProfileLoad { name } => {
                let profile = self
                    .call(ControlRequest::VehicleProfiles(
                        VehicleProfileRequest::Load { name },
                    ))?
                    .into_vehicle_profile()
                    .map_err(ApiError::from)?;
                Ok(ControlResultDto::VehicleProfile {
                    profile: profile_dto(profile),
                })
            }
            ControlCommandDto::VehicleProfileApply { name, source } => {
                let source = self.fields.resolve_source(&source)?;
                let response = self.call(ControlRequest::VehicleProfiles(
                    VehicleProfileRequest::Apply {
                        name,
                        source_id: source.id,
                        source: source.label,
                        owner: None,
                    },
                ))?;
                self.one_resource(response, "vehicle")
            }
            ControlCommandDto::VehicleProfileDelete { name } => self.unit(
                ControlRequest::VehicleProfiles(VehicleProfileRequest::Delete { name }),
            ),
            ControlCommandDto::LayoutList => {
                self.names(ControlRequest::Layouts(LayoutRequest::List))
            }
            ControlCommandDto::LayoutSave { name } => {
                self.unit(ControlRequest::Layouts(LayoutRequest::Save { name }))
            }
            ControlCommandDto::LayoutLoad { name } => {
                self.layout_mutation(LayoutRequest::Load { name })
            }
            ControlCommandDto::LayoutDelete { name } => {
                self.unit(ControlRequest::Layouts(LayoutRequest::Delete { name }))
            }
            ControlCommandDto::LayoutRename { from, to } => {
                self.unit(ControlRequest::Layouts(LayoutRequest::Rename { from, to }))
            }
            ControlCommandDto::LayoutDuplicate { from, to } => {
                self.unit(ControlRequest::Layouts(LayoutRequest::Duplicate {
                    from,
                    to,
                }))
            }
            ControlCommandDto::LayoutImport { path } => {
                self.layout_mutation(LayoutRequest::ImportFile { path })
            }
            ControlCommandDto::LayoutExport { name, path } => {
                self.unit(ControlRequest::Layouts(LayoutRequest::ExportFile {
                    name,
                    path,
                }))
            }
            ControlCommandDto::LayoutClear => self.layout_mutation(LayoutRequest::Clear),
            ControlCommandDto::LayoutCurrent => {
                let json = self
                    .call(ControlRequest::Layouts(LayoutRequest::Current))?
                    .into_layout()
                    .map_err(ApiError::from)?;
                Ok(ControlResultDto::Layout { json })
            }
            ControlCommandDto::LayoutApply { json } => {
                self.layout_mutation(LayoutRequest::Apply { json })
            }
            ControlCommandDto::RemoveOwned => Err(ApiError::internal(
                "remove_owned must run through owner cleanup",
            )),
        }
    }

    pub fn remove_owned(&mut self) -> Result<usize, ApiError> {
        let removed = self
            .call(ControlRequest::Generation(GenerationRequest::RemoveOwned {
                owner: self.principal.owner.name.clone(),
            }))?
            .into_removed()
            .map_err(ApiError::from)?;
        self.handles.clear_resources();
        Ok(removed)
    }

    fn unit(&self, request: ControlRequest) -> Result<ControlResultDto, ApiError> {
        self.call(request)?.into_unit().map_err(ApiError::from)?;
        Ok(ControlResultDto::Unit)
    }
    fn names(&self, request: ControlRequest) -> Result<ControlResultDto, ApiError> {
        let names = self.call(request)?.into_names().map_err(ApiError::from)?;
        Ok(ControlResultDto::Names { names })
    }
    fn layout_mutation(&mut self, request: LayoutRequest) -> Result<ControlResultDto, ApiError> {
        let response = self.call(ControlRequest::Layouts(request))?;
        self.handles.clear_resources();
        match response {
            ControlResponse::Unit => Ok(ControlResultDto::Unit),
            ControlResponse::LoadReport(report) => Ok(ControlResultDto::LoadReport {
                ambiguous: report
                    .ambiguous
                    .into_iter()
                    .map(|issue| LayoutFieldIssueDto {
                        field: issue.field,
                        candidates: issue.candidates,
                    })
                    .collect(),
                unresolved: report.unresolved,
                warnings: report.warnings,
            }),
            _ => Err(ApiError::internal(
                "native layout returned an unexpected response",
            )),
        }
    }
}

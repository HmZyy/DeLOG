use std::collections::HashMap;

use delog_core::identity::FieldId;

use crate::handles::OpaqueId;
use crate::protocol::v1::error::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NativePlotKey {
    pub window: u64,
    pub tile: u64,
    pub instance_id: u64,
}

impl NativePlotKey {
    pub const fn new(window: u64, tile: u64) -> Self {
        Self {
            window,
            tile,
            instance_id: 0,
        }
    }
    pub const fn with_instance_id(window: u64, tile: u64, instance_id: u64) -> Self {
        Self {
            window,
            tile,
            instance_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HandleTarget {
    Window {
        id: u64,
    },
    Plot {
        window: u64,
        tile: u64,
        instance_id: u64,
    },
    Trace {
        window: u64,
        tile: u64,
        index: usize,
        field: FieldId,
        plot_instance_id: u64,
        trace_instance_id: u64,
    },
    Annotation {
        window: u64,
        tile: u64,
        id: u64,
        plot_instance_id: u64,
    },
    Marker {
        id: u64,
    },
    Vehicle {
        id: u64,
    },
    Field {
        field: FieldId,
        source: String,
        topic: String,
        name: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ControlHandleRegistry {
    by_target: HashMap<HandleTarget, OpaqueId>,
    by_handle: HashMap<OpaqueId, HandleTarget>,
}

impl ControlHandleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, target: HandleTarget) -> OpaqueId {
        if let Some(handle) = self.by_target.get(&target) {
            return handle.clone();
        }
        let handle =
            OpaqueId::generate().expect("the operating system must provide handle randomness");
        self.by_handle.insert(handle.clone(), target.clone());
        self.by_target.insert(target, handle.clone());
        handle
    }

    pub fn resolve(&self, handle: &OpaqueId) -> Result<&HandleTarget, ApiError> {
        self.by_handle
            .get(handle)
            .ok_or_else(|| ApiError::stale_handle("control handle is stale"))
    }

    pub fn invalidate(&mut self, target: &HandleTarget) {
        if let Some(handle) = self.by_target.remove(target) {
            self.by_handle.remove(&handle);
        }
    }

    pub fn import_field(&mut self, handle: OpaqueId, target: HandleTarget) {
        if !matches!(target, HandleTarget::Field { .. }) {
            return;
        }
        if let Some(previous) = self.by_handle.insert(handle.clone(), target.clone()) {
            self.by_target.remove(&previous);
        }
        if let Some(previous) = self.by_target.insert(target, handle) {
            self.by_handle.remove(&previous);
        }
    }

    pub fn register_window(&mut self, id: u64) -> OpaqueId {
        self.register(HandleTarget::Window { id })
    }
    pub fn register_plot(&mut self, key: NativePlotKey) -> OpaqueId {
        self.register(HandleTarget::Plot {
            window: key.window,
            tile: key.tile,
            instance_id: key.instance_id,
        })
    }
    pub fn register_trace(
        &mut self,
        key: NativePlotKey,
        index: usize,
        field: FieldId,
        trace_instance_id: u64,
    ) -> OpaqueId {
        self.register(HandleTarget::Trace {
            window: key.window,
            tile: key.tile,
            index,
            field,
            plot_instance_id: key.instance_id,
            trace_instance_id,
        })
    }
    pub fn register_annotation(&mut self, key: NativePlotKey, id: u64) -> OpaqueId {
        self.register(HandleTarget::Annotation {
            window: key.window,
            tile: key.tile,
            id,
            plot_instance_id: key.instance_id,
        })
    }
    pub fn register_marker(&mut self, id: u64) -> OpaqueId {
        self.register(HandleTarget::Marker { id })
    }
    pub fn register_vehicle(&mut self, id: u64) -> OpaqueId {
        self.register(HandleTarget::Vehicle { id })
    }

    pub fn resolve_plot(&self, handle: &OpaqueId) -> Result<NativePlotKey, ApiError> {
        match self.resolve(handle)? {
            HandleTarget::Plot {
                window,
                tile,
                instance_id,
            } => Ok(NativePlotKey::with_instance_id(
                *window,
                *tile,
                *instance_id,
            )),
            _ => Err(ApiError::stale_handle("handle does not address a plot")),
        }
    }
    pub fn resolve_trace(
        &self,
        handle: &OpaqueId,
    ) -> Result<(NativePlotKey, usize, FieldId, u64), ApiError> {
        match self.resolve(handle)? {
            HandleTarget::Trace {
                window,
                tile,
                index,
                field,
                plot_instance_id,
                trace_instance_id,
            } => Ok((
                NativePlotKey::with_instance_id(*window, *tile, *plot_instance_id),
                *index,
                *field,
                *trace_instance_id,
            )),
            _ => Err(ApiError::stale_handle("handle does not address a trace")),
        }
    }
    pub fn resolve_annotation(&self, handle: &OpaqueId) -> Result<(NativePlotKey, u64), ApiError> {
        match self.resolve(handle)? {
            HandleTarget::Annotation {
                window,
                tile,
                id,
                plot_instance_id,
            } => Ok((
                NativePlotKey::with_instance_id(*window, *tile, *plot_instance_id),
                *id,
            )),
            _ => Err(ApiError::stale_handle(
                "handle does not address an annotation",
            )),
        }
    }
    pub fn resolve_window(&self, handle: &OpaqueId) -> Result<u64, ApiError> {
        match self.resolve(handle)? {
            HandleTarget::Window { id } => Ok(*id),
            _ => Err(ApiError::stale_handle("handle does not address a window")),
        }
    }
    pub fn resolve_marker(&self, handle: &OpaqueId) -> Result<u64, ApiError> {
        match self.resolve(handle)? {
            HandleTarget::Marker { id } => Ok(*id),
            _ => Err(ApiError::stale_handle("handle does not address a marker")),
        }
    }
    pub fn resolve_vehicle(&self, handle: &OpaqueId) -> Result<u64, ApiError> {
        match self.resolve(handle)? {
            HandleTarget::Vehicle { id } => Ok(*id),
            _ => Err(ApiError::stale_handle("handle does not address a vehicle")),
        }
    }
    pub fn invalidate_plot(&mut self, key: NativePlotKey) {
        let targets: Vec<_> = self
            .by_target
            .keys()
            .filter(|target| match target {
                HandleTarget::Plot {
                    window,
                    tile,
                    instance_id,
                } => *window == key.window && *tile == key.tile && *instance_id == key.instance_id,
                HandleTarget::Trace {
                    window,
                    tile,
                    plot_instance_id,
                    ..
                }
                | HandleTarget::Annotation {
                    window,
                    tile,
                    plot_instance_id,
                    ..
                } => {
                    *window == key.window
                        && *tile == key.tile
                        && *plot_instance_id == key.instance_id
                }
                _ => false,
            })
            .cloned()
            .collect();
        for target in targets {
            self.invalidate(&target);
        }
    }
    pub fn invalidate_traces(&mut self, key: NativePlotKey) {
        let targets: Vec<_> = self.by_target.keys().filter(|target| matches!(target,
            HandleTarget::Trace { window, tile, plot_instance_id, .. } if *window == key.window && *tile == key.tile && *plot_instance_id == key.instance_id
        )).cloned().collect();
        for target in targets {
            self.invalidate(&target);
        }
    }
    pub fn clear_resources(&mut self) {
        let targets: Vec<_> = self
            .by_target
            .keys()
            .filter(|target| !matches!(target, HandleTarget::Field { .. }))
            .cloned()
            .collect();
        for target in targets {
            self.invalidate(&target);
        }
    }
    pub fn invalidate_window(&mut self, id: u64) {
        let targets: Vec<_> = self
            .by_target
            .keys()
            .filter(|target| match target {
                HandleTarget::Window { id: actual } => *actual == id,
                HandleTarget::Plot { window, .. }
                | HandleTarget::Trace { window, .. }
                | HandleTarget::Annotation { window, .. } => *window == id,
                _ => false,
            })
            .cloned()
            .collect();
        for target in targets {
            self.invalidate(&target);
        }
    }
    pub fn retain_live(&mut self, live: &std::collections::HashSet<HandleTarget>) {
        let stale: Vec<_> = self
            .by_target
            .keys()
            .filter(|target| {
                !matches!(target, HandleTarget::Field { .. }) && !live.contains(*target)
            })
            .cloned()
            .collect();
        for target in stale {
            self.invalidate(&target);
        }
    }
}

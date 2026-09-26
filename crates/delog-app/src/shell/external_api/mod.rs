use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use delog_api::control::{AccessMode, AuthorizedControlHost};
use delog_core::ingest::IngestSender;
use delog_core::snapshot::{DataStore, StoreSnapshot};
use delog_remote::{
    ClientId, ControlLimits, DiscoveryError, LeaseId, OwnerRegistry, RemoteConfig, RemoteServer,
    RemoteServerHandle, RemoteServices, ShutdownError, StartError, UploadConfig, UploadLimits,
};

mod settings_tab;

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalApiLimits {
    pub lease_idle_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub max_concurrent_downloads: usize,
    pub upload_max_mib: u64,
    pub upload_max_rows: u64,
    pub upload_max_fields: usize,
    pub max_concurrent_uploads: usize,
    pub max_queued_controls: usize,
    pub max_queued_controls_per_client: usize,
    pub control_timeout_secs: u64,
}

impl Default for ExternalApiLimits {
    fn default() -> Self {
        let uploads = UploadConfig::default();
        let control = ControlLimits::default();
        Self {
            lease_idle_timeout_secs: 300,
            request_timeout_secs: 5,
            max_concurrent_downloads: 4,
            upload_max_mib: uploads.limits.max_bytes / MIB,
            upload_max_rows: uploads.limits.max_rows,
            upload_max_fields: uploads.limits.max_fields,
            max_concurrent_uploads: uploads.max_concurrent,
            max_queued_controls: control.max_queued,
            max_queued_controls_per_client: control.max_queued_per_client,
            control_timeout_secs: control.timeout.as_secs(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningEndpoint {
    pub endpoint: std::net::SocketAddr,
    pub instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalApiStatus {
    Disabled,
    Running(RunningEndpoint),
}

pub struct ExternalApiController {
    ctx: egui::Context,
    control: Arc<dyn AuthorizedControlHost>,
    owners: Arc<OwnerRegistry>,
    confirming_full: bool,
    handle: Option<RemoteServerHandle>,
    running: Option<RunningEndpoint>,
    published: Option<(String, Option<String>)>,
    last_error: Option<String>,
    limits: ExternalApiLimits,
}

pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

pub(crate) fn instance_label(snapshot: &StoreSnapshot) -> String {
    let labels: Vec<&str> = snapshot
        .sources
        .iter()
        .filter(|source| !source.entry.removed)
        .map(|source| source.entry.label.as_str())
        .collect();
    if labels.is_empty() {
        "DeLOG session".to_owned()
    } else {
        labels.join(", ")
    }
}

pub(crate) fn loaded_file_basename(snapshot: &StoreSnapshot) -> Option<String> {
    snapshot
        .sources
        .iter()
        .find(|source| !source.entry.removed)
        .map(|source| source.entry.label.clone())
}

pub(crate) fn build_config(
    snapshot: &StoreSnapshot,
    limits: ExternalApiLimits,
    discovery_root: Option<PathBuf>,
) -> RemoteConfig {
    RemoteConfig {
        label: instance_label(snapshot),
        loaded_file: loaded_file_basename(snapshot),
        lease_idle_timeout: Duration::from_secs(limits.lease_idle_timeout_secs.max(1)),
        request_timeout: Duration::from_secs(limits.request_timeout_secs.max(1)),
        max_concurrent_downloads: limits.max_concurrent_downloads.max(1),
        discovery_root,
        control: ControlLimits {
            max_queued: limits.max_queued_controls.max(1),
            max_queued_per_client: limits.max_queued_controls_per_client.max(1),
            timeout: Duration::from_secs(limits.control_timeout_secs.max(1)),
            ..ControlLimits::default()
        },
        uploads: UploadConfig {
            limits: UploadLimits {
                max_bytes: limits.upload_max_mib.max(1).saturating_mul(MIB),
                max_rows: limits.upload_max_rows.max(1),
                max_fields: limits.upload_max_fields.max(1),
            },
            max_concurrent: limits.max_concurrent_uploads.max(1),
        },
    }
}

impl ExternalApiController {
    pub fn new(ctx: egui::Context, control: Arc<dyn AuthorizedControlHost>) -> Self {
        Self {
            ctx,
            control,
            owners: Arc::new(OwnerRegistry::new()),
            confirming_full: false,
            handle: None,
            running: None,
            published: None,
            last_error: None,
            limits: ExternalApiLimits::default(),
        }
    }

    pub fn status(&self) -> ExternalApiStatus {
        match &self.running {
            Some(running) => ExternalApiStatus::Running(running.clone()),
            None => ExternalApiStatus::Disabled,
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn enable(
        &mut self,
        config: RemoteConfig,
        store: Arc<DataStore>,
        ingest: IngestSender,
    ) -> Result<(), StartError> {
        let published = (config.label.clone(), config.loaded_file.clone());
        let services = RemoteServices {
            store,
            ingest,
            control: Arc::clone(&self.control),
            owners: Arc::clone(&self.owners),
        };
        match RemoteServer::spawn(config, services) {
            Ok(handle) => {
                self.confirming_full = false;
                self.published = Some(published);
                self.running = Some(RunningEndpoint {
                    endpoint: handle.endpoint(),
                    instance_id: handle.instance_id().to_string(),
                });
                self.handle = Some(handle);
                self.last_error = None;
                self.ctx.request_repaint();
                Ok(())
            }
            Err(error) => {
                self.last_error = Some(error_chain(&error));
                Err(error)
            }
        }
    }

    pub fn sync_session(&mut self, snapshot: &StoreSnapshot) -> Result<bool, DiscoveryError> {
        let (Some(handle), Some(published)) = (&self.handle, &mut self.published) else {
            return Ok(false);
        };
        let label = instance_label(snapshot);
        let loaded_file = loaded_file_basename(snapshot);
        if published.0 == label && published.1 == loaded_file {
            return Ok(false);
        }
        let updated = handle.update_session(&label, loaded_file.as_deref());
        *published = (label, loaded_file);
        updated
    }

    pub fn disable(&mut self) -> Result<(), ShutdownError> {
        self.confirming_full = false;
        self.running = None;
        self.published = None;
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        handle.shutdown()
    }

    pub fn running_config(&self) -> Option<&RemoteConfig> {
        self.handle.as_ref().map(RemoteServerHandle::config)
    }

    pub fn access_mode(&self) -> Option<AccessMode> {
        self.handle.as_ref().map(RemoteServerHandle::access_mode)
    }

    pub fn request_access_mode(&mut self, access: AccessMode) -> Option<AccessMode> {
        let handle = self.handle.as_ref()?;
        match access {
            AccessMode::Safe => {
                self.confirming_full = false;
                handle.set_access_mode(AccessMode::Safe);
                Some(AccessMode::Safe)
            }
            AccessMode::Full => {
                if handle.access_mode() != AccessMode::Full {
                    self.confirming_full = true;
                }
                None
            }
        }
    }

    pub fn confirm_full_access(&mut self) -> Option<AccessMode> {
        let confirmed = std::mem::take(&mut self.confirming_full);
        let handle = self.handle.as_ref()?;
        if !confirmed {
            return None;
        }
        handle.set_access_mode(AccessMode::Full);
        Some(AccessMode::Full)
    }

    pub fn cancel_full_access(&mut self) {
        self.confirming_full = false;
    }

    #[cfg(test)]
    pub(crate) fn upload_staging_dir(&self) -> Option<PathBuf> {
        self.handle
            .as_ref()
            .map(|handle| handle.upload_staging_dir().to_owned())
    }

    pub fn revoke_client(&self, client: &ClientId) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| handle.revoke_client(client))
    }

    pub fn revoke_lease(&self, lease: &LeaseId) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| handle.revoke_lease(lease))
    }
}

impl Drop for ExternalApiController {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.shutdown();
        }
    }
}

#[cfg(test)]
mod tests;

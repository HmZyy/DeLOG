use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::routes::{RouterState, router};
use crate::{
    ClientId, ClientSession, DiscoveryDescriptor, DiscoveryError, DiscoveryRegistration, LeaseId,
    LeaseStatus, OpaqueId, OwnerRegistry, SecretToken, UploadLimits,
};
use delog_api::control::{AccessMode, AuthorizedControlHost};
use delog_core::ingest::IngestSender;
use delog_core::snapshot::DataStore;

#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub label: String,
    pub loaded_file: Option<String>,
    pub lease_idle_timeout: Duration,
    pub request_timeout: Duration,
    pub max_concurrent_downloads: usize,
    pub discovery_root: Option<PathBuf>,
    pub control: ControlLimits,
    pub uploads: UploadConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadConfig {
    pub limits: UploadLimits,
    pub max_concurrent: usize,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            limits: UploadLimits::default(),
            max_concurrent: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlLimits {
    pub max_queued: usize,
    pub max_queued_per_client: usize,
    pub idempotency_ttl: Duration,
    pub max_idempotency_records_per_client: usize,
    pub timeout: Duration,
}

impl Default for ControlLimits {
    fn default() -> Self {
        Self {
            max_queued: 8,
            max_queued_per_client: 4,
            idempotency_ttl: Duration::from_secs(600),
            max_idempotency_records_per_client: 256,
            timeout: Duration::from_secs(5),
        }
    }
}

pub struct RemoteServices {
    pub store: Arc<DataStore>,
    pub ingest: IngestSender,
    pub control: Arc<dyn AuthorizedControlHost>,
    pub owners: Arc<OwnerRegistry>,
}

#[derive(Debug)]
pub struct ServerStatus {
    pub clients: Vec<ClientSession>,
    pub leases: Vec<LeaseStatus>,
    pub access: AccessMode,
    pub active_uploads: usize,
    pub active_downloads: usize,
    pub queued_controls: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("invalid remote server configuration")]
    InvalidConfig,
    #[error("could not generate remote server credentials")]
    Random,
    #[error("could not start the loopback server")]
    Io(#[source] std::io::Error),
    #[error("could not publish instance discovery")]
    Discovery(#[source] DiscoveryError),
    #[error("remote server thread stopped during startup")]
    Thread,
}

#[derive(Debug, thiserror::Error)]
pub enum ShutdownError {
    #[error("remote server shutdown deadline exceeded")]
    Timeout,
    #[error("remote server thread failed")]
    Thread,
}

pub struct RemoteServer;

pub struct RemoteServerHandle {
    state: Arc<RouterState>,
    thread: Option<JoinHandle<()>>,
    finished: mpsc::Receiver<Result<(), ShutdownError>>,
}

impl fmt::Debug for RemoteServerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteServerHandle")
            .field("endpoint", &self.endpoint())
            .finish_non_exhaustive()
    }
}

impl RemoteServer {
    pub fn spawn(
        config: RemoteConfig,
        services: RemoteServices,
    ) -> Result<RemoteServerHandle, StartError> {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("delog-remote".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = started_tx.send(Err(StartError::Io(error)));
                        return;
                    }
                };
                let timeout = config.request_timeout;
                let (result, deadline, state) = runtime.block_on(async move {
                    let listener =
                        match tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await {
                            Ok(listener) => listener,
                            Err(error) => {
                                let _ = started_tx.send(Err(StartError::Io(error)));
                                return (Ok(()), None, None);
                            }
                        };
                    let endpoint = match listener.local_addr() {
                        Ok(endpoint) => endpoint,
                        Err(error) => {
                            let _ = started_tx.send(Err(StartError::Io(error)));
                            return (Ok(()), None, None);
                        }
                    };
                    let state = match RouterState::new(config, services, endpoint) {
                        Ok(state) => state,
                        Err(error) => {
                            let _ = started_tx.send(Err(error));
                            return (Ok(()), None, None);
                        }
                    };
                    let descriptor = DiscoveryDescriptor::new(
                        &state.instance(),
                        endpoint,
                        state.bootstrap_token(),
                    );
                    let published = match &state.config.discovery_root {
                        Some(root) => DiscoveryRegistration::publish(root, descriptor),
                        None => DiscoveryRegistration::publish_default(descriptor),
                    };
                    match published {
                        Ok(registration) => state.attach_discovery(registration),
                        Err(error) => {
                            let _ = started_tx.send(Err(StartError::Discovery(error)));
                            return (Ok(()), None, None);
                        }
                    }
                    let reaper_state = state.clone();
                    let reaper = tokio::spawn(async move {
                        let period = (reaper_state.config.lease_idle_timeout / 4)
                            .clamp(Duration::from_millis(1), Duration::from_secs(1));
                        let mut ticks = tokio::time::interval(period);
                        loop {
                            tokio::select! {
                                biased;
                                _ = reaper_state.cancellation.cancelled() => break,
                                _ = ticks.tick() => reaper_state.reap(Instant::now()),
                            }
                        }
                    });
                    let cancellation = state.cancellation.clone();
                    let serve = axum::serve(listener, router(state.clone()))
                        .with_graceful_shutdown(cancellation.clone().cancelled_owned());
                    use std::future::IntoFuture;
                    let serve = serve.into_future();
                    tokio::pin!(serve);
                    if started_tx.send(Ok(state.clone())).is_err() {
                        state.request_stop(Instant::now() + timeout);
                    }
                    let served = tokio::select! {
                        result = &mut serve => Some(result.map_err(|_| ShutdownError::Thread)),
                        _ = cancellation.cancelled() => None,
                    };
                    let deadline = state.shutdown_deadline();
                    // Leave enough of the public deadline to remove discovery, report the
                    // result, and stop the runtime. Waiting until the exact same instant as
                    // `RemoteServerHandle::shutdown` makes the final send race its timeout.
                    let cleanup_reserve =
                        (timeout / 10).clamp(Duration::from_millis(1), Duration::from_millis(100));
                    let work_deadline = deadline.checked_sub(cleanup_reserve).unwrap_or(deadline);
                    let until = tokio::time::Instant::from_std(work_deadline);
                    let stopping = state.clone();
                    let revoked = tokio::task::spawn_blocking(move || stopping.stop());
                    let result = async {
                        let revoked = tokio::time::timeout_at(until, revoked).await;
                        let drained = served.is_some();
                        if let Some(served) = served {
                            served?;
                        }
                        revoked
                            .map_err(|_| ShutdownError::Timeout)?
                            .map_err(|_| ShutdownError::Thread)?;
                        let _ = tokio::time::timeout_at(until, reaper).await;
                        tokio::time::timeout_at(until, async {
                            while !state.producers_finished() {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                        })
                        .await
                        .map_err(|_| ShutdownError::Timeout)?;
                        if !drained {
                            let _ = tokio::time::timeout_at(until, &mut serve).await;
                        }
                        Ok(())
                    }
                    .await;
                    (result, Some(deadline), Some(state))
                });
                if let Some(registration) = state.and_then(|state| state.detach_discovery())
                    && let Err(error) = registration.remove()
                {
                    tracing::warn!(%error, "could not remove instance discovery descriptor");
                }
                let _ = finished_tx.send(result);
                let remaining = deadline.map_or(timeout, |deadline| {
                    deadline.saturating_duration_since(Instant::now())
                });
                runtime.shutdown_timeout(remaining);
            })
            .map_err(StartError::Io)?;
        match started_rx.recv() {
            Ok(Ok(state)) => Ok(RemoteServerHandle {
                state,
                thread: Some(thread),
                finished: finished_rx,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(StartError::Thread)
            }
        }
    }
}

impl RemoteServerHandle {
    pub fn endpoint(&self) -> SocketAddr {
        self.state.endpoint
    }
    pub fn bootstrap_token(&self) -> &SecretToken {
        self.state.bootstrap_token()
    }
    pub fn instance_id(&self) -> &OpaqueId {
        self.state.instance_id()
    }
    pub fn status(&self) -> ServerStatus {
        self.state.status()
    }
    pub fn config(&self) -> &RemoteConfig {
        &self.state.config
    }
    #[doc(hidden)]
    pub fn upload_staging_dir(&self) -> &std::path::Path {
        self.state.upload_staging_dir()
    }
    pub fn access_mode(&self) -> AccessMode {
        self.state.access_mode()
    }
    pub fn set_access_mode(&self, access: AccessMode) {
        self.state.set_access_mode(access);
    }
    pub fn revoke_client(&self, client: &ClientId) -> bool {
        self.state.revoke_client(client)
    }
    pub fn revoke_lease(&self, lease: &LeaseId) -> bool {
        self.state.revoke_lease(lease)
    }
    pub fn update_session(
        &self,
        label: &str,
        loaded_file: Option<&str>,
    ) -> Result<bool, DiscoveryError> {
        self.state.update_session(label, loaded_file)
    }

    pub fn shutdown(mut self) -> Result<(), ShutdownError> {
        let deadline = Instant::now() + self.state.config.request_timeout;
        self.state.request_stop(deadline);
        let result = self
            .finished
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => ShutdownError::Timeout,
                mpsc::RecvTimeoutError::Disconnected => ShutdownError::Thread,
            })?;
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| ShutdownError::Thread)?;
        }
        result
    }
}

impl Drop for RemoteServerHandle {
    fn drop(&mut self) {
        self.state
            .request_stop(Instant::now() + self.state.config.request_timeout);
    }
}

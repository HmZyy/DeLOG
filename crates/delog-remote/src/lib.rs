pub mod arrow_stream;
pub mod auth;
pub mod catalog;
pub mod control;
pub mod discovery;
pub mod handles;
pub mod idempotency;
pub mod leases;
pub mod protocol;
pub mod publications;
pub mod routes;
pub mod server;
pub mod upload;

pub use server::{
    ControlLimits, RemoteConfig, RemoteServer, RemoteServerHandle, RemoteServices, ServerStatus,
    ShutdownError, StartError, UploadConfig,
};

pub use arrow_stream::{
    ArrowStreamStats, DELOG_TIME_COLUMN, ReadQuery, SOURCE_TIME_COLUMN, TopicBatchIter, arrow_body,
    arrow_body_with_stats,
};
pub use auth::{
    ClientRegistry, ClientSession, OwnerRegistry, RegisterClientRequest, RegisterClientResponse,
    RegisteredClient, SecretToken, TokenParseError, external_owner_name,
};
pub use catalog::{FieldHandle, LeaseHandles};
pub use control::handles::{ControlHandleRegistry, NativePlotKey};
pub use discovery::{DiscoveryDescriptor, DiscoveryError, DiscoveryRegistration};
pub use handles::{ClientId, OpaqueId, OwnerId, RequestId};
pub use idempotency::{
    IdempotencyAction, IdempotencyRegistry, MAX_IDEMPOTENCY_KEY_BYTES, RequestFingerprint,
    RequestState, content_digest,
};
pub use leases::{
    CreatedLease, LeaseId, LeaseManager, LeaseReadGuard, LeaseStatus, MAX_LEASES_PER_CLIENT,
    PreparedLease,
};
pub use protocol::v1::catalog::{
    CatalogDto, FieldDto, FieldSelectorDto, FieldTypeDto, SourceDto, SourceKindDto, TimeRangeDto,
    TopicDto,
};
pub use protocol::v1::control::{
    ControlBatchDto, ControlCommandDto, ControlResultDto, ControlStateDto,
};
pub use protocol::v1::error::{ApiError, ErrorEnvelope, WireCompletion};
pub use protocol::v1::instance::InstanceDto;
pub use protocol::v1::publication::{PublicationDto, PublicationFieldDto, PublicationTopicDto};
pub use protocol::v1::request::{RequestStateDto, RequestStatusDto};
pub use protocol::v1::{API_MAJOR, API_MAX_MINOR, API_MIN_MINOR};
pub use publications::{OwnerRemoval, PublicationRegistry};
pub use upload::{StagedUpload, UploadLimits, decode_staged_upload, stage_body};

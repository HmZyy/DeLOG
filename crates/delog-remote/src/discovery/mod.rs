use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{InstanceDto, OpaqueId, SecretToken};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

pub const DESCRIPTOR_FORMAT: u32 = 1;
const MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;
const MAX_ID_LEN: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("no runtime directory is available for instance discovery")]
    NoRuntimeDirectory,
    #[error("discovery path {} failed the current-user permission check", .path.display())]
    Insecure { path: PathBuf },
    #[error("invalid discovery descriptor")]
    InvalidDescriptor,
    #[error("discovery file operation failed")]
    Io(#[from] io::Error),
}

pub(crate) fn insecure(path: &Path) -> DiscoveryError {
    DiscoveryError::Insecure {
        path: path.to_owned(),
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryDescriptor {
    format: u32,
    instance_id: OpaqueId,
    label: String,
    pid: u32,
    endpoint: SocketAddr,
    api_major: u16,
    api_min_minor: u16,
    api_max_minor: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_description: Option<String>,
    #[serde(
        serialize_with = "serialize_token",
        deserialize_with = "deserialize_token"
    )]
    bootstrap_token: SecretToken,
}

fn serialize_token<S: Serializer>(token: &SecretToken, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&token.expose())
}

fn deserialize_token<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SecretToken, D::Error> {
    let encoded = String::deserialize(deserializer)?;
    SecretToken::parse(encoded).map_err(|_| serde::de::Error::custom("invalid bootstrap token"))
}

impl DiscoveryDescriptor {
    pub fn new(instance: &InstanceDto, endpoint: SocketAddr, bootstrap: &SecretToken) -> Self {
        Self {
            format: DESCRIPTOR_FORMAT,
            instance_id: instance.instance_id.clone(),
            label: instance.label.clone(),
            pid: std::process::id(),
            endpoint,
            api_major: instance.api_major,
            api_min_minor: instance.api_min_minor,
            api_max_minor: instance.api_max_minor,
            session_description: instance.session_description.clone(),
            bootstrap_token: bootstrap.clone(),
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, DiscoveryError> {
        let descriptor: Self =
            serde_json::from_slice(bytes).map_err(|_| DiscoveryError::InvalidDescriptor)?;
        descriptor.validate()?;
        Ok(descriptor)
    }

    fn validate(&self) -> Result<(), DiscoveryError> {
        let loopback = match self.endpoint {
            SocketAddr::V4(endpoint) => {
                *endpoint.ip() == Ipv4Addr::LOCALHOST && endpoint.port() != 0
            }
            SocketAddr::V6(_) => false,
        };
        if self.format != DESCRIPTOR_FORMAT || !loopback || !valid_id(self.instance_id.as_str()) {
            return Err(DiscoveryError::InvalidDescriptor);
        }
        Ok(())
    }

    fn file_name(&self) -> String {
        format!("{}.json", self.instance_id)
    }

    pub fn format(&self) -> u32 {
        self.format
    }
    pub fn instance_id(&self) -> &OpaqueId {
        &self.instance_id
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }
    pub fn api_major(&self) -> u16 {
        self.api_major
    }
    pub fn api_min_minor(&self) -> u16 {
        self.api_min_minor
    }
    pub fn api_max_minor(&self) -> u16 {
        self.api_max_minor
    }
    pub fn session_description(&self) -> Option<&str> {
        self.session_description.as_deref()
    }
    pub fn bootstrap_token(&self) -> &SecretToken {
        &self.bootstrap_token
    }
}

impl fmt::Debug for DiscoveryDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscoveryDescriptor")
            .field("format", &self.format)
            .field("instance_id", &self.instance_id)
            .field("label", &self.label)
            .field("pid", &self.pid)
            .field("endpoint", &self.endpoint)
            .field("api_major", &self.api_major)
            .field("api_min_minor", &self.api_min_minor)
            .field("api_max_minor", &self.api_max_minor)
            .field("session_description", &self.session_description)
            .field("bootstrap_token", &"[REDACTED]")
            .finish()
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn staging_name(instance_id: &OpaqueId) -> String {
    format!(".{instance_id}.{}.tmp", std::process::id())
}

pub fn prepare_default_root() -> Result<PathBuf, DiscoveryError> {
    sys::prepare_default_root()
}

pub fn remove_stale_descriptors(root: &Path) -> Result<usize, DiscoveryError> {
    sys::verify_dir(root)?;
    let mut removed = 0;
    for entry in fs::read_dir(root)? {
        let Ok(entry) = entry else { continue };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let path = entry.path();
        let Some(pid) = stale_candidate_pid(&name, &path) else {
            continue;
        };
        if !sys::pid_alive(pid) && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

fn parse_pid(digits: &str) -> Option<u32> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn stale_candidate_pid(name: &str, path: &Path) -> Option<u32> {
    if let Some(staging) = name
        .strip_prefix('.')
        .and_then(|rest| rest.strip_suffix(".tmp"))
    {
        let (id, pid) = staging.rsplit_once('.')?;
        let pid = parse_pid(pid)?;
        return (valid_id(id) && sys::is_private_file(path)).then_some(pid);
    }
    let id = name.strip_suffix(".json")?;
    if !valid_id(id) || !sys::is_private_file(path) {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return None;
    }
    let descriptor = DiscoveryDescriptor::parse(&bytes).ok()?;
    (descriptor.instance_id.as_str() == id).then_some(descriptor.pid)
}

fn write_atomically(
    root: &Path,
    descriptor: &DiscoveryDescriptor,
) -> Result<PathBuf, DiscoveryError> {
    let bytes = serde_json::to_vec(descriptor).map_err(|_| DiscoveryError::InvalidDescriptor)?;
    let target = root.join(descriptor.file_name());
    let staging = root.join(staging_name(&descriptor.instance_id));
    let mut file = match sys::create_private_file(&staging) {
        Err(DiscoveryError::Io(error))
            if error.kind() == io::ErrorKind::AlreadyExists && sys::is_private_file(&staging) =>
        {
            fs::remove_file(&staging)?;
            sys::create_private_file(&staging)?
        }
        other => other?,
    };
    let written = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = written.and_then(|()| fs::rename(&staging, &target));
    if let Err(error) = result {
        let _ = fs::remove_file(&staging);
        return Err(error.into());
    }
    sys::sync_dir(root);
    Ok(target)
}

pub struct DiscoveryRegistration {
    root: PathBuf,
    path: PathBuf,
    instance_id: OpaqueId,
    active: bool,
}

impl DiscoveryRegistration {
    pub fn publish(root: &Path, descriptor: DiscoveryDescriptor) -> Result<Self, DiscoveryError> {
        descriptor.validate()?;
        sys::prepare_root(root)?;
        remove_stale_descriptors(root)?;
        let path = write_atomically(root, &descriptor)?;
        Ok(Self {
            root: root.to_owned(),
            path,
            instance_id: descriptor.instance_id,
            active: true,
        })
    }

    pub fn publish_default(descriptor: DiscoveryDescriptor) -> Result<Self, DiscoveryError> {
        let root = prepare_default_root()?;
        Self::publish(&root, descriptor)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn update(&mut self, descriptor: DiscoveryDescriptor) -> Result<(), DiscoveryError> {
        descriptor.validate()?;
        if descriptor.instance_id != self.instance_id {
            return Err(DiscoveryError::InvalidDescriptor);
        }
        sys::verify_dir(&self.root)?;
        write_atomically(&self.root, &descriptor)?;
        Ok(())
    }

    pub fn remove(mut self) -> Result<(), DiscoveryError> {
        self.active = false;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

impl fmt::Debug for DiscoveryRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscoveryRegistration")
            .field("path", &self.path)
            .field("instance_id", &self.instance_id)
            .finish()
    }
}

impl Drop for DiscoveryRegistration {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_file(&self.path);
        }
    }
}

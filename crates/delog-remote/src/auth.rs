use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::handles::{ClientId, OwnerId};
use crate::protocol::v1::error::ApiError;

const TOKEN_BYTES: usize = 32;

pub struct SecretToken([u8; TOKEN_BYTES]);

impl SecretToken {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    pub fn parse(encoded: impl AsRef<str>) -> Result<Self, TokenParseError> {
        let decoded = URL_SAFE_NO_PAD
            .decode(encoded.as_ref())
            .map_err(|_| TokenParseError)?;
        let bytes: [u8; TOKEN_BYTES] = decoded.try_into().map_err(|_| TokenParseError)?;
        Ok(Self(bytes))
    }

    pub fn expose(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn matches(&self, candidate: &SecretToken) -> bool {
        self.0.ct_eq(&candidate.0).into()
    }
}

impl Clone for SecretToken {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid secret token encoding")]
pub struct TokenParseError;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterClientRequest {
    pub name: String,
    #[serde(default)]
    pub takeover: bool,
}

#[derive(Clone, Serialize, PartialEq)]
pub struct RegisterClientResponse {
    pub client_id: ClientId,
    pub owner_id: OwnerId,
    pub owner_name: String,
    pub token: String,
}

impl fmt::Debug for RegisterClientResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisterClientResponse")
            .field("client_id", &self.client_id)
            .field("owner_id", &self.owner_id)
            .field("owner_name", &self.owner_name)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl From<&RegisteredClient> for RegisterClientResponse {
    fn from(registered: &RegisteredClient) -> Self {
        Self {
            client_id: registered.client_id.clone(),
            owner_id: registered.owner_id.clone(),
            owner_name: registered.owner_name.clone(),
            token: registered.token.expose(),
        }
    }
}

#[derive(Debug)]
pub struct RegisteredClient {
    pub client_id: ClientId,
    pub owner_id: OwnerId,
    pub owner_name: String,
    pub token: SecretToken,
    pub replaced: Option<ClientId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSession {
    pub client_id: ClientId,
    pub owner_id: OwnerId,
    pub owner_name: String,
}

struct ClientEntry {
    owner_id: OwnerId,
    owner_name: String,
    token: SecretToken,
    last_seen: Instant,
}

#[derive(Debug, Default)]
pub struct OwnerRegistry {
    owners: Mutex<HashMap<String, OwnerId>>,
}

impl OwnerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn owner(&self, name: &str) -> Result<OwnerId, ApiError> {
        let mut owners = self.owners.lock().expect("owner registry poisoned");
        if let Some(owner) = owners.get(name) {
            return Ok(owner.clone());
        }
        let owner = OwnerId::generate().map_err(random_generation_failed)?;
        owners.insert(name.to_owned(), owner.clone());
        Ok(owner)
    }

    pub fn find(&self, name: &str) -> Option<OwnerId> {
        self.owners
            .lock()
            .expect("owner registry poisoned")
            .get(name)
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.owners.lock().expect("owner registry poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Default)]
pub struct ClientRegistry {
    owners: Arc<OwnerRegistry>,
    clients: HashMap<ClientId, ClientEntry>,
}

impl ClientRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_owners(owners: Arc<OwnerRegistry>) -> Self {
        Self {
            owners,
            clients: HashMap::new(),
        }
    }

    pub fn owners(&self) -> &Arc<OwnerRegistry> {
        &self.owners
    }

    pub fn register(
        &mut self,
        bootstrap: &SecretToken,
        presented: &SecretToken,
        request: RegisterClientRequest,
    ) -> Result<RegisteredClient, ApiError> {
        if !bootstrap.matches(presented) {
            return Err(ApiError::forbidden(
                "presented token does not match the instance bootstrap token",
            ));
        }

        let name = validate_client_name(&request.name)?;
        let owner_id = self.owners.owner(&name)?;
        let active_client = self
            .clients
            .iter()
            .find(|(_, entry)| entry.owner_id == owner_id)
            .map(|(client_id, _)| client_id.clone());
        let replaced = match active_client {
            Some(_) if !request.takeover => {
                return Err(ApiError::conflict("client name is already connected"));
            }
            active => active,
        };

        let client_id = ClientId::generate().map_err(random_generation_failed)?;
        let token = SecretToken::generate().map_err(random_generation_failed)?;
        if let Some(replaced) = &replaced {
            self.clients.remove(replaced);
        }

        self.clients.insert(
            client_id.clone(),
            ClientEntry {
                owner_id: owner_id.clone(),
                owner_name: name.clone(),
                token: token.clone(),
                last_seen: Instant::now(),
            },
        );

        Ok(RegisteredClient {
            client_id,
            owner_id,
            owner_name: name,
            token,
            replaced,
        })
    }

    pub fn authenticate(
        &mut self,
        token: &SecretToken,
        now: Instant,
    ) -> Result<ClientSession, ApiError> {
        let matched = self
            .clients
            .iter()
            .find(|(_, entry)| entry.token.matches(token))
            .map(|(client_id, entry)| {
                (
                    client_id.clone(),
                    entry.owner_id.clone(),
                    entry.owner_name.clone(),
                )
            });

        let (client_id, owner_id, owner_name) =
            matched.ok_or_else(|| ApiError::forbidden("client token is unknown or revoked"))?;

        if let Some(entry) = self.clients.get_mut(&client_id) {
            entry.last_seen = now;
        }

        Ok(ClientSession {
            client_id,
            owner_id,
            owner_name,
        })
    }

    pub fn touch(&mut self, client: &ClientId, now: Instant) -> bool {
        match self.clients.get_mut(client) {
            Some(entry) => {
                entry.last_seen = entry.last_seen.max(now);
                true
            }
            None => false,
        }
    }

    pub fn contains(&self, client: &ClientId) -> bool {
        self.clients.contains_key(client)
    }

    pub fn revoke(&mut self, client: &ClientId) -> bool {
        self.clients.remove(client).is_some()
    }

    pub fn list(&self) -> Vec<ClientSession> {
        self.clients
            .iter()
            .map(|(id, entry)| ClientSession {
                client_id: id.clone(),
                owner_id: entry.owner_id.clone(),
                owner_name: entry.owner_name.clone(),
            })
            .collect()
    }

    pub fn reap(&mut self, now: Instant, ttl: Duration) -> Vec<ClientId> {
        let expired: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, entry)| now.saturating_duration_since(entry.last_seen) >= ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.clients.remove(id);
        }
        expired
    }
}

fn random_generation_failed(error: getrandom::Error) -> ApiError {
    ApiError::internal(format!("failed to generate random bytes: {error}"))
}

fn validate_client_name(raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    let length = trimmed.chars().count();

    if length == 0 || length > 64 {
        return Err(ApiError::invalid_input(
            "client name must be between 1 and 64 characters",
        ));
    }

    let is_allowed = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));

    if !is_allowed {
        return Err(ApiError::invalid_input(
            "client name must contain only ASCII letters, digits, '.', '_', or '-'",
        ));
    }

    Ok(trimmed.to_string())
}

pub fn external_owner_name(client_name: &str) -> String {
    format!("external/{client_name}")
}

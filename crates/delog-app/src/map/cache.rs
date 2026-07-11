use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::provider::{MapProviderId, TileId};

pub const DEFAULT_TILE_CACHE_BYTES: u64 = 1_073_741_824;
const MIN_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const INDEX_FILE: &str = "index.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheGeneration(pub u64);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
struct CacheKey {
    provider: String,
    zoom: u8,
    x: u32,
    y: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CacheMeta {
    size: u64,
    last_access: u64,
    checksum: u64,
}

#[derive(Default, Deserialize, Serialize)]
struct PersistedIndex {
    generation: u64,
    sequence: u64,
    entries: Vec<(CacheKey, CacheMeta)>,
}

pub struct TileDiskCache {
    root: PathBuf,
    limit: u64,
    generation: u64,
    sequence: u64,
    index: HashMap<CacheKey, CacheMeta>,
}

impl TileDiskCache {
    pub fn open(root: PathBuf, limit: u64) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let persisted = match fs::read(root.join(INDEX_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => PersistedIndex::default(),
            Err(error) => return Err(error),
        };
        let mut cache = Self {
            root,
            limit: limit.clamp(MIN_CACHE_BYTES, MAX_CACHE_BYTES),
            generation: persisted.generation,
            sequence: persisted.sequence,
            index: persisted.entries.into_iter().collect(),
        };
        cache.remove_invalid_entries()?;
        cache.prune()?;
        cache.persist_index()?;
        Ok(cache)
    }

    pub fn read(&mut self, provider: MapProviderId, tile: TileId) -> io::Result<Option<Vec<u8>>> {
        let key = CacheKey::new(provider, tile);
        let Some(expected) = self.index.get(&key).cloned() else {
            return Ok(None);
        };
        let path = self.path(&key);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.index.remove(&key);
                self.persist_index()?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if bytes.len() as u64 != expected.size || checksum(&bytes) != expected.checksum {
            remove_if_exists(&path)?;
            self.index.remove(&key);
            self.persist_index()?;
            return Ok(None);
        }
        let access = self.next_sequence();
        self.index.get_mut(&key).unwrap().last_access = access;
        self.persist_index()?;
        Ok(Some(bytes))
    }

    pub fn write(&mut self, provider: MapProviderId, tile: TileId, bytes: &[u8]) -> io::Result<()> {
        let key = CacheKey::new(provider, tile);
        let path = self.path(&key);
        let parent = path.parent().expect("tile path always has a parent");
        fs::create_dir_all(parent)?;
        let part = parent.join(format!(
            "{}.part-{}-{}",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id(),
            self.next_sequence()
        ));
        let result: io::Result<()> = (|| {
            let mut file = File::create(&part)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&part, &path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&part);
        }
        result?;
        let access = self.sequence;
        self.index.insert(
            key,
            CacheMeta {
                size: bytes.len() as u64,
                last_access: access,
                checksum: checksum(bytes),
            },
        );
        self.prune()?;
        self.persist_index()
    }

    pub fn set_limit(&mut self, bytes: u64) -> io::Result<()> {
        self.limit = bytes.clamp(MIN_CACHE_BYTES, MAX_CACHE_BYTES);
        self.prune()?;
        self.persist_index()
    }

    pub fn clear(&mut self) -> io::Result<CacheGeneration> {
        fs::remove_dir_all(&self.root)?;
        fs::create_dir_all(&self.root)?;
        self.index.clear();
        self.generation = self.generation.wrapping_add(1);
        self.persist_index()?;
        Ok(CacheGeneration(self.generation))
    }

    pub fn usage_bytes(&self) -> u64 {
        self.index.values().map(|meta| meta.size).sum()
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    fn path(&self, key: &CacheKey) -> PathBuf {
        self.root
            .join(&key.provider)
            .join(key.zoom.to_string())
            .join(key.x.to_string())
            .join(format!("{}.tile", key.y))
    }

    fn remove_invalid_entries(&mut self) -> io::Result<()> {
        let invalid: Vec<_> = self
            .index
            .iter()
            .filter_map(|(key, meta)| {
                fs::metadata(self.path(key))
                    .ok()
                    .filter(|data| data.is_file() && data.len() == meta.size)
                    .is_none()
                    .then(|| key.clone())
            })
            .collect();
        for key in invalid {
            remove_if_exists(&self.path(&key))?;
            self.index.remove(&key);
        }
        Ok(())
    }

    fn prune(&mut self) -> io::Result<()> {
        while self.usage_bytes() > self.limit {
            let Some(key) = self
                .index
                .iter()
                .min_by_key(|(_, meta)| meta.last_access)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            remove_if_exists(&self.path(&key))?;
            self.index.remove(&key);
        }
        Ok(())
    }

    fn persist_index(&self) -> io::Result<()> {
        let persisted = PersistedIndex {
            generation: self.generation,
            sequence: self.sequence,
            entries: self
                .index
                .iter()
                .map(|(key, meta)| (key.clone(), meta.clone()))
                .collect(),
        };
        let bytes = serde_json::to_vec(&persisted).map_err(io::Error::other)?;
        let part = self
            .root
            .join(format!("{INDEX_FILE}.part-{}", std::process::id()));
        let mut file = File::create(&part)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&part, self.root.join(INDEX_FILE))?;
        File::open(&self.root)?.sync_all()
    }
}

impl CacheKey {
    fn new(provider: MapProviderId, tile: TileId) -> Self {
        Self {
            provider: provider_name(provider).to_owned(),
            zoom: tile.zoom,
            x: tile.x,
            y: tile.y,
        }
    }
}

fn provider_name(provider: MapProviderId) -> &'static str {
    match provider {
        MapProviderId::None => "none",
        MapProviderId::BingSatellite => "bing_satellite",
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::map::provider::{MapProviderId, TileId};

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "delog-cache-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn tile(x: u32) -> TileId {
        TileId { zoom: 3, x, y: 2 }
    }

    #[test]
    fn write_read_round_trip() {
        let root = temp_root("round-trip");
        let mut cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(1), b"tile data")
            .unwrap();
        assert_eq!(
            cache.read(MapProviderId::BingSatellite, tile(1)).unwrap(),
            Some(b"tile data".to_vec())
        );
        assert_eq!(cache.usage_bytes(), 9);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_entry_is_deleted() {
        let root = temp_root("corrupt");
        let mut cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(1), b"valid")
            .unwrap();
        let path = root.join("bing_satellite/3/1/2.tile");
        fs::write(&path, b"changed size").unwrap();
        assert_eq!(
            cache.read(MapProviderId::BingSatellite, tile(1)).unwrap(),
            None
        );
        assert!(!path.exists());
        assert_eq!(cache.usage_bytes(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn part_files_never_count_as_hits() {
        let root = temp_root("part");
        let dir = root.join("bing_satellite/3/1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("2.tile.part-123"), b"partial").unwrap();
        let mut cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        assert_eq!(
            cache.read(MapProviderId::BingSatellite, tile(1)).unwrap(),
            None
        );
        assert_eq!(cache.usage_bytes(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lru_pruning_removes_oldest_entry() {
        let root = temp_root("lru");
        let mut cache = TileDiskCache::open(root.clone(), 64 * 1024 * 1024).unwrap();
        let chunk = vec![7; 32 * 1024 * 1024];
        cache
            .write(MapProviderId::BingSatellite, tile(1), &chunk)
            .unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(2), &chunk)
            .unwrap();
        cache.read(MapProviderId::BingSatellite, tile(1)).unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(3), &[8])
            .unwrap();
        assert!(cache
            .read(MapProviderId::BingSatellite, tile(1))
            .unwrap()
            .is_some());
        assert!(cache
            .read(MapProviderId::BingSatellite, tile(2))
            .unwrap()
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shrinking_limit_prunes() {
        let root = temp_root("shrink");
        let mut cache = TileDiskCache::open(root.clone(), 128 * 1024 * 1024).unwrap();
        let chunk = vec![4; 40 * 1024 * 1024];
        cache
            .write(MapProviderId::BingSatellite, tile(1), &chunk)
            .unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(2), &chunk)
            .unwrap();
        cache.set_limit(64 * 1024 * 1024).unwrap();
        assert!(cache
            .read(MapProviderId::BingSatellite, tile(1))
            .unwrap()
            .is_none());
        assert!(cache
            .read(MapProviderId::BingSatellite, tile(2))
            .unwrap()
            .is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clear_increments_generation_and_zeroes_usage() {
        let root = temp_root("clear");
        let mut cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(1), b"bytes")
            .unwrap();
        assert_eq!(cache.clear().unwrap(), CacheGeneration(1));
        assert_eq!(cache.usage_bytes(), 0);
        assert_eq!(cache.clear().unwrap(), CacheGeneration(2));
        fs::remove_dir_all(root).unwrap();
    }
}

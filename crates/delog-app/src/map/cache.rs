use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use super::provider::{MapProviderId, TileId};

const MIN_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const INDEX_FILE: &str = "index.json";
static PART_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        let (persisted, index_available) = match fs::read(root.join(INDEX_FILE)) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(persisted) => (persisted, true),
                Err(_) => (PersistedIndex::default(), false),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                (PersistedIndex::default(), false)
            }
            Err(error) => return Err(error),
        };
        let mut cache = Self {
            root,
            limit: limit.clamp(MIN_CACHE_BYTES, MAX_CACHE_BYTES),
            generation: persisted.generation,
            sequence: persisted.sequence,
            index: persisted.entries.into_iter().collect(),
        };
        cache.reconcile_tiles(index_available)?;
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
        let part = self.tile_part_path(&key);
        self.next_sequence();
        let result: io::Result<()> = (|| {
            let mut file = File::create(&part)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            atomic_replace(&part, &path)?;
            sync_parent(parent)?;
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

    fn tile_part_path(&self, key: &CacheKey) -> PathBuf {
        let path = self.path(key);
        path.with_file_name(format!(
            "{}.part-{}-{}",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id(),
            PART_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn reconcile_tiles(&mut self, index_available: bool) -> io::Result<()> {
        let mut files = Vec::new();
        collect_files(&self.root, &mut files)?;
        let old = std::mem::take(&mut self.index);
        for path in files {
            if path == self.root.join(INDEX_FILE) {
                continue;
            }
            let Some(key) = cache_key_from_path(&self.root, &path) else {
                remove_if_exists(&path)?;
                continue;
            };
            if self.path(&key) != path {
                remove_if_exists(&path)?;
                continue;
            }
            let bytes = fs::read(&path)?;
            let file_checksum = checksum(&bytes);
            let meta = match old.get(&key) {
                Some(meta) if meta.size == bytes.len() as u64 && meta.checksum == file_checksum => {
                    meta.clone()
                }
                Some(_) if index_available => {
                    remove_if_exists(&path)?;
                    continue;
                }
                _ => CacheMeta {
                    size: bytes.len() as u64,
                    last_access: self.next_sequence(),
                    checksum: file_checksum,
                },
            };
            self.index.insert(key, meta);
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
        let part = self.root.join(format!(
            "{INDEX_FILE}.part-{}-{}",
            std::process::id(),
            PART_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = File::create(&part)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            atomic_replace(&part, &self.root.join(INDEX_FILE))?;
            sync_parent(&self.root)
        })();
        if result.is_err() {
            let _ = remove_if_exists(&part);
        }
        result
    }
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both pointers reference NUL-terminated UTF-16 buffers that remain
    // alive for the duration of the call.
    let replaced = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn sync_parent(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(windows)]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    // MOVEFILE_WRITE_THROUGH waits for the replacement to reach storage.
    Ok(())
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&entry.path(), files)?;
        } else {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn cache_key_from_path(root: &Path, path: &Path) -> Option<CacheKey> {
    let mut components = path.strip_prefix(root).ok()?.components();
    let provider = components.next()?.as_os_str().to_str()?;
    if !matches!(provider, "none" | "bing_satellite") {
        return None;
    }
    let zoom = components.next()?.as_os_str().to_str()?.parse().ok()?;
    let x = components.next()?.as_os_str().to_str()?.parse().ok()?;
    let filename = components.next()?.as_os_str().to_str()?;
    if components.next().is_some() {
        return None;
    }
    let y = filename.strip_suffix(".tile")?.parse().ok()?;
    Some(CacheKey {
        provider: provider.to_owned(),
        zoom,
        x,
        y,
    })
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
    use std::fs;

    use super::*;
    use crate::map::provider::{MapProviderId, TileId};

    fn tile(x: u32) -> TileId {
        TileId { zoom: 3, x, y: 2 }
    }

    #[test]
    fn write_read_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let mut cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(1), b"tile data")
            .unwrap();
        assert_eq!(
            cache.read(MapProviderId::BingSatellite, tile(1)).unwrap(),
            Some(b"tile data".to_vec())
        );
        assert_eq!(cache.usage_bytes(), 9);
    }

    #[test]
    fn index_can_be_rewritten_repeatedly() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let mut cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();

        for x in 1..=8 {
            cache
                .write(MapProviderId::BingSatellite, tile(x), &[x as u8])
                .unwrap();
        }

        let persisted: PersistedIndex =
            serde_json::from_slice(&fs::read(root.join(INDEX_FILE)).unwrap()).unwrap();
        assert_eq!(persisted.entries.len(), 8);
    }

    #[test]
    fn writing_existing_tile_replaces_its_contents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let mut cache = TileDiskCache::open(root, u64::MAX).unwrap();

        cache
            .write(MapProviderId::BingSatellite, tile(1), b"old tile")
            .unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(1), b"replacement tile")
            .unwrap();

        assert_eq!(
            cache.read(MapProviderId::BingSatellite, tile(1)).unwrap(),
            Some(b"replacement tile".to_vec())
        );
        assert_eq!(cache.usage_bytes(), 16);
    }

    #[test]
    fn corrupt_entry_is_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
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
    }

    #[test]
    fn part_files_never_count_as_hits() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let dir = root.join("bing_satellite/3/1");
        fs::create_dir_all(&dir).unwrap();
        let part = dir.join("2.tile.part-123");
        fs::write(&part, b"partial").unwrap();
        let mut cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        assert_eq!(
            cache.read(MapProviderId::BingSatellite, tile(1)).unwrap(),
            None
        );
        assert_eq!(cache.usage_bytes(), 0);
        assert!(!part.exists());
    }

    #[test]
    fn lru_pruning_removes_oldest_entry() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
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
        assert!(
            cache
                .read(MapProviderId::BingSatellite, tile(1))
                .unwrap()
                .is_some()
        );
        assert!(
            cache
                .read(MapProviderId::BingSatellite, tile(2))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn shrinking_limit_prunes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let mut cache = TileDiskCache::open(root.clone(), 128 * 1024 * 1024).unwrap();
        let chunk = vec![4; 40 * 1024 * 1024];
        cache
            .write(MapProviderId::BingSatellite, tile(1), &chunk)
            .unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(2), &chunk)
            .unwrap();
        cache.set_limit(64 * 1024 * 1024).unwrap();
        assert!(
            cache
                .read(MapProviderId::BingSatellite, tile(1))
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .read(MapProviderId::BingSatellite, tile(2))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn clear_increments_generation_and_zeroes_usage() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let mut cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(1), b"bytes")
            .unwrap();
        assert_eq!(cache.clear().unwrap(), CacheGeneration(1));
        assert_eq!(cache.usage_bytes(), 0);
        assert_eq!(cache.clear().unwrap(), CacheGeneration(2));
    }

    #[test]
    fn missing_index_rebuilds_usage_from_tiles() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let path = root.join("bing_satellite/3/1/2.tile");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"orphaned tile").unwrap();

        let mut cache = TileDiskCache::open(root, u64::MAX).unwrap();

        assert_eq!(cache.usage_bytes(), 13);
        assert_eq!(
            cache.read(MapProviderId::BingSatellite, tile(1)).unwrap(),
            Some(b"orphaned tile".to_vec())
        );
    }

    #[test]
    fn corrupt_index_rebuilds_and_prunes_scanned_tiles() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        fs::write(root.join(INDEX_FILE), b"not json").unwrap();
        for x in [1, 2, 3] {
            let path = root.join(format!("bing_satellite/3/{x}/2.tile"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![x as u8; 32 * 1024 * 1024]).unwrap();
        }

        let cache = TileDiskCache::open(root.clone(), 64 * 1024 * 1024).unwrap();

        assert_eq!(cache.usage_bytes(), 64 * 1024 * 1024);
        let remaining = [1, 2, 3]
            .into_iter()
            .filter(|x| root.join(format!("bing_satellite/3/{x}/2.tile")).exists())
            .count();
        assert_eq!(remaining, 2);
    }

    #[test]
    fn valid_index_deletes_changed_indexed_tile_but_adopts_orphan() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let mut cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        cache
            .write(MapProviderId::BingSatellite, tile(1), b"trusted")
            .unwrap();
        drop(cache);

        let changed = root.join("bing_satellite/3/1/2.tile");
        fs::write(&changed, b"changed").unwrap();
        let orphan = root.join("bing_satellite/3/2/2.tile");
        fs::create_dir_all(orphan.parent().unwrap()).unwrap();
        fs::write(&orphan, b"orphan").unwrap();

        let mut cache = TileDiskCache::open(root, u64::MAX).unwrap();

        assert!(!changed.exists());
        assert_eq!(cache.usage_bytes(), 6);
        assert_eq!(
            cache.read(MapProviderId::BingSatellite, tile(2)).unwrap(),
            Some(b"orphan".to_vec())
        );
    }

    #[test]
    fn reconciliation_deletes_noncanonical_numeric_tile_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let canonical = root.join("bing_satellite/3/1/2.tile");
        fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        fs::write(&canonical, b"canonical").unwrap();
        let noncanonical = root.join("bing_satellite/03/01/02.tile");
        fs::create_dir_all(noncanonical.parent().unwrap()).unwrap();
        fs::write(&noncanonical, b"duplicate").unwrap();

        let cache = TileDiskCache::open(root, u64::MAX).unwrap();

        assert!(canonical.exists());
        assert!(!noncanonical.exists());
        assert_eq!(cache.usage_bytes(), 9);
    }

    #[test]
    fn tile_part_names_are_unique_across_cache_instances() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let cache_a = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        let cache_b = TileDiskCache::open(root, u64::MAX).unwrap();
        let key = CacheKey::new(MapProviderId::BingSatellite, tile(1));

        assert_ne!(cache_a.tile_part_path(&key), cache_b.tile_part_path(&key));
    }

    #[test]
    fn failed_index_persistence_removes_partial_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let cache = TileDiskCache::open(root.clone(), u64::MAX).unwrap();
        fs::remove_file(root.join(INDEX_FILE)).unwrap();
        fs::create_dir(root.join(INDEX_FILE)).unwrap();

        assert!(cache.persist_index().is_err());
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".part-")
        }));
    }
}

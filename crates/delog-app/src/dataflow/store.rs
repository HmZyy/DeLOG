use std::io::Write;
use std::path::{Path, PathBuf};

use delog_flow::doc;
use delog_flow::graph::Graph;

#[derive(Debug, thiserror::Error)]
pub enum GraphStoreError {
    #[error("invalid graph name '{0}'")]
    InvalidName(String),
    #[error("failed to {action} '{path}': {source}")]
    FileIo {
        action: &'static str,
        path: String,
        source: std::io::Error,
    },
    #[error("invalid JSON in '{path}': {source}")]
    InvalidJson {
        path: String,
        source: serde_json::Error,
    },
    #[error("failed to encode graph '{name}': {source}")]
    Encode {
        name: String,
        source: serde_json::Error,
    },
    #[error("failed to create temporary graph file: {0}")]
    TemporaryFile(#[source] std::io::Error),
    #[error("{0}")]
    Document(#[from] doc::DocError),
}

impl GraphStoreError {
    fn file_io(action: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::FileIo {
            action,
            path: path.display().to_string(),
            source,
        }
    }
}

pub struct GraphStore {
    dir: PathBuf,
}

impl GraphStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn default_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("org", "hmzyy", "DeLOG")
            .map(|dirs| dirs.data_dir().join("dataflows"))
    }

    pub fn list(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension().and_then(|extension| extension.to_str()) == Some("json"))
                    .then(|| path.file_stem()?.to_str().map(str::to_owned))
                    .flatten()
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    pub fn load(&self, name: &str) -> Result<Graph, GraphStoreError> {
        let path = self.path(name)?;
        let contents = std::fs::read_to_string(&path)
            .map_err(|error| GraphStoreError::file_io("read", &path, error))?;
        let value =
            serde_json::from_str(&contents).map_err(|source| GraphStoreError::InvalidJson {
                path: path.display().to_string(),
                source,
            })?;
        Ok(doc::from_json(&value)?)
    }

    pub fn save(&self, graph: &Graph) -> Result<(), GraphStoreError> {
        self.save_with_persist(graph, |temp, target| {
            temp.persist(target)
                .map(|_| ())
                .map_err(|error| error.error)
        })
    }

    fn save_with_persist(
        &self,
        graph: &Graph,
        persist: impl FnOnce(tempfile::NamedTempFile, &Path) -> std::io::Result<()>,
    ) -> Result<(), GraphStoreError> {
        let path = self.path(&graph.name)?;
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| GraphStoreError::file_io("create", &self.dir, error))?;
        let contents = serde_json::to_string_pretty(&doc::to_json(graph)).map_err(|source| {
            GraphStoreError::Encode {
                name: graph.name.clone(),
                source,
            }
        })?;
        let mut temp =
            tempfile::NamedTempFile::new_in(&self.dir).map_err(GraphStoreError::TemporaryFile)?;
        temp.write_all(contents.as_bytes())
            .and_then(|()| temp.flush())
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|error| GraphStoreError::file_io("write", &path, error))?;
        persist(temp, &path).map_err(|error| GraphStoreError::file_io("replace", &path, error))
    }

    pub fn delete(&self, name: &str) -> Result<(), GraphStoreError> {
        let path = self.path(name)?;
        std::fs::remove_file(&path)
            .map_err(|error| GraphStoreError::file_io("delete", &path, error))
    }

    fn path(&self, name: &str) -> Result<PathBuf, GraphStoreError> {
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            return Err(GraphStoreError::InvalidName(name.to_owned()));
        }
        Ok(self.dir.join(format!("{name}.json")))
    }
}

#[cfg(test)]
mod tests {
    use delog_flow::graph::Graph;

    use super::*;

    #[test]
    fn save_list_load_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = GraphStore::new(dir.path().to_path_buf());
        let mut graph = Graph::new("my-graph");
        graph.alloc_id();
        store.save(&graph).unwrap();
        assert_eq!(store.list(), vec!["my-graph".to_string()]);
        let loaded = store.load("my-graph").unwrap();
        assert_eq!(loaded.name, "my-graph");
        store.delete("my-graph").unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn bad_names_and_corrupt_files_error_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let store = GraphStore::new(dir.path().to_path_buf());
        assert!(store.save(&Graph::new("")).is_err());
        assert!(store.save(&Graph::new("a/b")).is_err());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("junk.json"), b"{").unwrap();
        assert!(store.load("junk").is_err());
    }

    #[test]
    fn graph_store_existing_messages_remain_stable() {
        let dir = tempfile::tempdir().unwrap();
        let store = GraphStore::new(dir.path().to_path_buf());
        assert_eq!(
            store.load("a/b").unwrap_err().to_string(),
            "invalid graph name 'a/b'"
        );
        assert!(
            store
                .load("missing")
                .unwrap_err()
                .to_string()
                .starts_with(&format!(
                    "failed to read '{}': ",
                    dir.path().join("missing.json").display()
                ))
        );
        std::fs::write(dir.path().join("junk.json"), b"{").unwrap();
        assert!(
            store
                .load("junk")
                .unwrap_err()
                .to_string()
                .starts_with(&format!(
                    "invalid JSON in '{}': ",
                    dir.path().join("junk.json").display()
                ))
        );
        let graph = Graph::new("replace-me");
        assert_eq!(
            store
                .save_with_persist(&graph, |_temp, _target| {
                    Err(std::io::Error::other("injected replacement failure"))
                })
                .unwrap_err()
                .to_string(),
            format!(
                "failed to replace '{}': injected replacement failure",
                dir.path().join("replace-me.json").display()
            )
        );
    }

    #[test]
    fn list_missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = GraphStore::new(dir.path().join("missing"));
        assert!(store.list().is_empty());
    }

    #[test]
    fn graph_store_errors_expose_typed_causes() {
        fn assert_error<T: std::error::Error>() {}
        assert_error::<GraphStoreError>();

        let dir = tempfile::tempdir().unwrap();
        let store = GraphStore::new(dir.path().to_path_buf());
        let missing = store.load("missing").unwrap_err();
        assert!(
            std::error::Error::source(&missing)
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .is_some()
        );

        std::fs::write(dir.path().join("junk.json"), b"{").unwrap();
        let malformed = store.load("junk").unwrap_err();
        assert!(
            std::error::Error::source(&malformed)
                .unwrap()
                .downcast_ref::<serde_json::Error>()
                .is_some()
        );

        let invalid = store.load("a/b").unwrap_err();
        assert!(std::error::Error::source(&invalid).is_none());
    }

    #[test]
    fn graph_store_document_error_keeps_message_and_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = GraphStore::new(dir.path().to_path_buf());
        std::fs::write(dir.path().join("invalid.json"), b"{}").unwrap();

        let error = store.load("invalid").unwrap_err();
        assert_eq!(error.to_string(), "missing delog_dataflow version");
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .downcast_ref::<delog_flow::doc::DocError>()
                .is_some()
        );
    }

    #[test]
    fn graph_store_error_variants_keep_display_contract() {
        for action in ["read", "create", "write", "replace", "delete"] {
            let error = GraphStoreError::FileIo {
                action,
                path: "graph.json".to_owned(),
                source: std::io::Error::other("denied"),
            };
            assert_eq!(
                error.to_string(),
                format!("failed to {action} 'graph.json': denied")
            );
            assert!(
                std::error::Error::source(&error)
                    .unwrap()
                    .downcast_ref::<std::io::Error>()
                    .is_some()
            );
        }
        assert_eq!(
            GraphStoreError::InvalidName("a/b".to_owned()).to_string(),
            "invalid graph name 'a/b'"
        );
        let invalid_json = GraphStoreError::InvalidJson {
            path: "graph.json".to_owned(),
            source: serde_json::Error::io(std::io::Error::other("bad token")),
        };
        assert_eq!(
            invalid_json.to_string(),
            "invalid JSON in 'graph.json': bad token"
        );
        assert!(
            std::error::Error::source(&invalid_json)
                .unwrap()
                .downcast_ref::<serde_json::Error>()
                .is_some()
        );
        let encode = GraphStoreError::Encode {
            name: "graph".to_owned(),
            source: serde_json::Error::io(std::io::Error::other("bad value")),
        };
        assert_eq!(
            encode.to_string(),
            "failed to encode graph 'graph': bad value"
        );
        assert!(
            std::error::Error::source(&encode)
                .unwrap()
                .downcast_ref::<serde_json::Error>()
                .is_some()
        );
        let temporary_file = GraphStoreError::TemporaryFile(std::io::Error::other("full"));
        assert_eq!(
            temporary_file.to_string(),
            "failed to create temporary graph file: full"
        );
        assert!(
            std::error::Error::source(&temporary_file)
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .is_some()
        );
    }

    #[test]
    fn failed_atomic_replacement_preserves_previous_graph_and_cleans_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = GraphStore::new(dir.path().to_path_buf());
        let original = Graph::new("replace-me");
        store.save(&original).unwrap();
        let mut replacement = Graph::new("replace-me");
        replacement.alloc_id();

        let error = store
            .save_with_persist(&replacement, |_temp, _target| {
                Err(std::io::Error::other("injected replacement failure"))
            })
            .unwrap_err();

        assert!(error.to_string().contains("injected replacement failure"));
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .is_some()
        );
        assert_eq!(store.load("replace-me").unwrap().alloc_id().0, 1);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn save_atomically_replaces_an_existing_graph() {
        let dir = tempfile::tempdir().unwrap();
        let store = GraphStore::new(dir.path().to_path_buf());
        store.save(&Graph::new("replace-me")).unwrap();
        let mut replacement = Graph::new("replace-me");
        replacement.alloc_id();

        store.save(&replacement).unwrap();

        assert_eq!(store.load("replace-me").unwrap().alloc_id().0, 2);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

use std::io::Write;
use std::path::{Path, PathBuf};

use delog_flow::doc;
use delog_flow::graph::Graph;

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

    pub fn load(&self, name: &str) -> Result<Graph, String> {
        let path = self.path(name)?;
        let contents = std::fs::read_to_string(&path)
            .map_err(|error| format!("failed to read '{}': {error}", path.display()))?;
        let value = serde_json::from_str(&contents)
            .map_err(|error| format!("invalid JSON in '{}': {error}", path.display()))?;
        doc::from_json(&value).map_err(|error| error.to_string())
    }

    pub fn save(&self, graph: &Graph) -> Result<(), String> {
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
    ) -> Result<(), String> {
        let path = self.path(&graph.name)?;
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| format!("failed to create '{}': {error}", self.dir.display()))?;
        let contents = serde_json::to_string_pretty(&doc::to_json(graph))
            .map_err(|error| format!("failed to encode graph '{}': {error}", graph.name))?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir)
            .map_err(|error| format!("failed to create temporary graph file: {error}"))?;
        temp.write_all(contents.as_bytes())
            .and_then(|()| temp.flush())
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|error| format!("failed to write '{}': {error}", path.display()))?;
        persist(temp, &path)
            .map_err(|error| format!("failed to replace '{}': {error}", path.display()))
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        let path = self.path(name)?;
        std::fs::remove_file(&path)
            .map_err(|error| format!("failed to delete '{}': {error}", path.display()))
    }

    fn path(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            return Err(format!("invalid graph name '{name}'"));
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

        assert!(error.contains("injected replacement failure"));
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

use super::doc::SequenceDoc;
use std::io::Write;
use std::path::PathBuf;

pub struct SequenceStore {
    dir: PathBuf,
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty()
        || name.chars().all(|c| c == '.')
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        || name.ends_with(['.', ' '])
    {
        return Err(format!("invalid sequence item name '{name}'"));
    }
    Ok(())
}

impl SequenceStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
    pub fn default_store() -> Option<Self> {
        directories::ProjectDirs::from("org", "hmzyy", "DeLOG")
            .map(|dirs| Self::new(dirs.data_dir().join("sequences")))
    }
    fn path(&self, name: &str) -> Result<PathBuf, String> {
        validate_name(name)?;
        Ok(self.dir.join(format!("{name}.json")))
    }
    pub fn list(&self) -> Result<Vec<String>, String> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut names = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|v| v == "json")
                && let Some(name) = path.file_stem().and_then(|v| v.to_str())
            {
                names.push(name.into());
            }
        }
        names.sort();
        Ok(names)
    }
    pub fn load(&self, name: &str) -> Result<SequenceDoc, String> {
        let bytes = std::fs::read(self.path(name)?).map_err(|e| e.to_string())?;
        let doc: SequenceDoc = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        doc.validate()?;
        if doc.name != name {
            return Err("sequence name does not match its file".into());
        }
        Ok(doc)
    }
    pub fn save(&self, doc: &SequenceDoc) -> Result<(), String> {
        doc.validate()?;
        let path = self.path(&doc.name)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
        temp.write_all(&bytes)
            .and_then(|()| temp.flush())
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        temp.persist(path).map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn rename(&self, old: &str, new: &str) -> Result<(), String> {
        if old == new {
            return Ok(());
        }
        if self.path(new)?.exists() {
            return Err("a sequence with that name already exists".into());
        }
        let mut doc = self.load(old)?;
        doc.name = new.into();
        self.save(&doc)?;
        if let Err(error) = self.delete(old) {
            return match self.delete(new) {
                Ok(()) => Err(error),
                Err(rollback) => Err(format!("{error}; rollback failed: {rollback}")),
            };
        }
        Ok(())
    }
    pub fn delete(&self, name: &str) -> Result<(), String> {
        std::fs::remove_file(self.path(name)?).map_err(|e| e.to_string())
    }
}

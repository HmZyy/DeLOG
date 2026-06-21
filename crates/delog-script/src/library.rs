//! Persistent global script library: `.py` files in a directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A scripts library rooted at a directory of `.py` files.
pub struct ScriptLibrary {
    dir: PathBuf,
}

impl ScriptLibrary {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn validate(name: &str) -> io::Result<()> {
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid script name '{name}'"),
            ));
        }
        Ok(())
    }

    fn path(&self, name: &str) -> io::Result<PathBuf> {
        Self::validate(name)?;
        Ok(self.dir.join(format!("{name}.py")))
    }

    /// Script names (file stems), sorted.
    pub fn list(&self) -> io::Result<Vec<String>> {
        let mut out = Vec::new();
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in rd {
            let p = entry?.path();
            if p.extension().and_then(|e| e.to_str()) == Some("py")
                && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
            {
                out.push(stem.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn load(&self, name: &str) -> io::Result<String> {
        fs::read_to_string(self.path(name)?)
    }

    pub fn save(&self, name: &str, source: &str) -> io::Result<()> {
        let path = self.path(name)?;
        fs::create_dir_all(&self.dir)?;
        fs::write(path, source)
    }

    pub fn delete(&self, name: &str) -> io::Result<()> {
        fs::remove_file(self.path(name)?)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_list_load_delete_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("delog-scripts-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let lib = ScriptLibrary::new(&tmp);

        lib.save("accel_mag", "print('hi')").unwrap();
        assert_eq!(lib.list().unwrap(), vec!["accel_mag".to_string()]);
        assert_eq!(lib.load("accel_mag").unwrap(), "print('hi')");
        lib.delete("accel_mag").unwrap();
        assert!(lib.list().unwrap().is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_names_with_path_separators() {
        let tmp = std::env::temp_dir().join("delog-scripts-bad");
        let lib = ScriptLibrary::new(&tmp);
        assert!(lib.save("../evil", "x").is_err());
        assert!(lib.load("a/b").is_err());
    }
}

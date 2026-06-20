//! Persistent library for custom Python log parsers.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_STAGING_FILE: AtomicU64 = AtomicU64::new(0);

struct StagedFile {
    path: PathBuf,
    moved: bool,
}

impl StagedFile {
    fn write(dir: &Path, source: &str) -> io::Result<Self> {
        let (staged, mut file) = Self::create(dir)?;
        file.write_all(source.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        Ok(staged)
    }

    fn create(dir: &Path) -> io::Result<(Self, File)> {
        loop {
            let sequence = NEXT_STAGING_FILE.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(
                ".delog-parser-{}-{sequence}.tmp",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((Self { path, moved: false }, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.moved {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn commit_replace(
    staged: &mut StagedFile,
    destination: &Path,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    replace(&staged.path, destination)?;
    staged.moved = true;
    Ok(())
}

fn publish_new(staged: &StagedFile, destination: &Path) -> io::Result<()> {
    fs::hard_link(&staged.path, destination)
}

/// A custom parser library rooted at a directory of `.py` files.
pub struct ParserLibrary {
    dir: PathBuf,
}

impl ParserLibrary {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Validates a local parser filename and ensures it has one `.py` suffix.
    pub fn normalize_name(&self, name: &str) -> io::Result<String> {
        if name.is_empty()
            || name.contains(['/', '\\'])
            || name.contains("..")
            || name
                .chars()
                .any(|character| character.is_control() || "<>:\"|?*".contains(character))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid parser name '{name}'"),
            ));
        }

        match name.split_once('.') {
            None if !Self::is_windows_reserved_stem(name) => Ok(format!("{name}.py")),
            Some((stem, "py")) if !stem.is_empty() && !Self::is_windows_reserved_stem(stem) => {
                Ok(name.to_owned())
            }
            Some(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("parser name must have exactly one .py extension: '{name}'"),
            )),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid parser name '{name}'"),
            )),
        }
    }

    fn is_windows_reserved_stem(stem: &str) -> bool {
        let stem = stem.to_ascii_uppercase();
        matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix)
                .is_some_and(|suffix| matches!(suffix.as_bytes(), [b'1'..=b'9']))
        })
    }

    /// Returns parser filenames, including `.py`, in lexical order.
    pub fn list(&self) -> io::Result<Vec<String>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && let Some(name) = entry.file_name().to_str()
                && matches!(self.normalize_name(name), Ok(normalized) if normalized == name)
            {
                names.push(name.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn load(&self, name: &str) -> io::Result<String> {
        fs::read_to_string(self.dir.join(self.normalize_name(name)?))
    }

    pub fn save(&self, old_name: Option<&str>, name: &str, source: &str) -> io::Result<String> {
        let destination_name = self.normalize_name(name)?;
        let old_name = old_name
            .map(|old_name| self.normalize_name(old_name))
            .transpose()?;
        fs::create_dir_all(&self.dir)?;
        let destination = self.dir.join(&destination_name);
        let mut staged = StagedFile::write(&self.dir, source)?;

        match old_name {
            None => publish_new(&staged, &destination)?,
            Some(old_name) if old_name == destination_name => {
                commit_replace(&mut staged, &destination, |source, destination| {
                    fs::rename(source, destination)
                })?;
            }
            Some(old_name) => {
                publish_new(&staged, &destination)?;
                if let Err(error) = fs::remove_file(self.dir.join(old_name)) {
                    let _ = fs::remove_file(&destination);
                    return Err(error);
                }
            }
        }
        Ok(destination_name)
    }

    pub fn delete(&self, name: &str) -> io::Result<()> {
        fs::remove_file(self.dir.join(self.normalize_name(name)?))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos();
            let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "delog-parser-library-{}-{nonce}-{sequence}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            match fs::remove_dir_all(&self.0) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("failed to clean up {}: {error}", self.0.display()),
            }
        }
    }

    #[test]
    fn save_load_rename_and_delete_roundtrip() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);

        assert_eq!(library.dir(), temp.0.as_path());
        assert_eq!(library.save(None, "dt46", "first").unwrap(), "dt46.py");
        assert_eq!(library.list().unwrap(), vec!["dt46.py"]);
        assert_eq!(library.load("dt46").unwrap(), "first");

        assert_eq!(
            library
                .save(Some("dt46.py"), "flight.py", "second")
                .unwrap(),
            "flight.py"
        );
        assert_eq!(library.list().unwrap(), vec!["flight.py"]);
        assert_eq!(library.load("flight.py").unwrap(), "second");

        library.delete("flight").unwrap();
        assert!(library.list().unwrap().is_empty());
    }

    #[test]
    fn rename_rejects_existing_destination_without_overwriting_it() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);
        library.save(None, "old.py", "old source").unwrap();
        library
            .save(None, "existing.py", "existing source")
            .unwrap();

        let error = library
            .save(Some("old.py"), "existing.py", "replacement")
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(library.load("old.py").unwrap(), "old source");
        assert_eq!(library.load("existing.py").unwrap(), "existing source");
    }

    #[test]
    fn new_save_rejects_existing_destination_without_overwriting_it() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);
        library
            .save(None, "existing.py", "original source")
            .unwrap();

        let error = library
            .save(None, "existing.py", "replacement source")
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(library.load("existing.py").unwrap(), "original source");
    }

    #[test]
    fn failed_same_name_publish_preserves_original_and_cleans_staging_file() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);
        library.save(None, "parser.py", "original source").unwrap();

        let mut staged = StagedFile::write(&temp.0, "replacement source").unwrap();
        let error = commit_replace(&mut staged, &temp.0.join("parser.py"), |_, _| {
            Err(io::Error::other("injected publish failure"))
        })
        .unwrap_err();
        drop(staged);

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(library.load("parser.py").unwrap(), "original source");
        assert_eq!(library.list().unwrap(), vec!["parser.py"]);
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
    }

    #[test]
    fn failed_old_remove_cleans_up_new_destination() {
        let temp = TestDir::new();
        fs::create_dir_all(temp.0.join("old.py")).unwrap();
        let library = ParserLibrary::new(&temp.0);

        assert!(
            library
                .save(Some("old.py"), "new.py", "new source")
                .is_err()
        );

        assert!(temp.0.join("old.py").is_dir());
        assert!(!temp.0.join("new.py").exists());
    }

    #[test]
    fn list_returns_only_sorted_python_filenames() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        fs::write(temp.0.join("zulu.py"), "z").unwrap();
        fs::write(temp.0.join("alpha.py"), "a").unwrap();
        fs::write(temp.0.join("ignored.txt"), "x").unwrap();
        fs::create_dir(temp.0.join("directory.py")).unwrap();

        let library = ParserLibrary::new(&temp.0);
        assert_eq!(library.list().unwrap(), vec!["alpha.py", "zulu.py"]);
    }

    #[test]
    fn missing_directory_lists_as_empty() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);

        assert!(library.list().unwrap().is_empty());
    }

    #[test]
    fn list_omits_files_that_do_not_normalize_to_their_own_name() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        fs::write(temp.0.join("parser.py"), "valid").unwrap();
        fs::write(temp.0.join("parser.v1.py"), "multiple extensions").unwrap();
        fs::write(temp.0.join(".py"), "empty stem").unwrap();
        fs::write(temp.0.join("C:parser.py"), "Windows drive prefix").unwrap();

        let library = ParserLibrary::new(&temp.0);
        assert_eq!(library.list().unwrap(), vec!["parser.py"]);
    }

    #[test]
    fn rejects_unsafe_paths() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);

        for name in ["", "../evil", "safe..evil", "nested/file", "nested\\file"] {
            assert_eq!(
                library.normalize_name(name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{name:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_non_portable_filename_components() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);

        for name in [
            "C:parser.py",
            "parser:name.py",
            "parser?.py",
            "parser*.py",
            "parser|name.py",
            "parser\u{7f}.py",
            "CON.py",
            "lpt1.py",
        ] {
            assert_eq!(
                library.normalize_name(name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{name:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_non_python_extensions() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);

        for name in ["parser.txt", "parser.rs", "parser.PY"] {
            assert_eq!(
                library.normalize_name(name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn rejects_multiple_extensions() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);

        for name in ["parser.py.py", "parser.txt.py", "parser.v1.py"] {
            assert_eq!(
                library.normalize_name(name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{name:?} should be rejected"
            );
        }
    }

    #[test]
    fn normalize_adds_one_python_suffix() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);

        assert_eq!(library.normalize_name("dt46").unwrap(), "dt46.py");
        assert_eq!(library.normalize_name("dt46.py").unwrap(), "dt46.py");
    }

    #[test]
    fn saving_to_the_same_name_overwrites_without_removing_the_file() {
        let temp = TestDir::new();
        let library = ParserLibrary::new(&temp.0);

        library.save(None, "parser", "old").unwrap();
        library.save(Some("parser.py"), "parser", "new").unwrap();

        assert_eq!(library.load("parser").unwrap(), "new");
    }
}

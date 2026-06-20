//! Persistent library for custom Python log parsers.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use same_file::Handle;

static NEXT_STAGING_FILE: AtomicU64 = AtomicU64::new(0);
const STAGING_PREFIX: &str = ".delog-parser-";
const STAGING_SUFFIX: &str = ".tmp";
const STALE_STAGING_AGE: Duration = Duration::from_secs(24 * 60 * 60);

fn private_create_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn remove_if_owned(path: &Path, created: &Handle) {
    if Handle::from_path(path).is_ok_and(|current| current == *created) {
        let _ = fs::remove_file(path);
    }
}

struct StagedFile {
    path: PathBuf,
    identity: Option<Handle>,
    moved: bool,
}

impl StagedFile {
    fn write(dir: &Path, source: &str) -> io::Result<Self> {
        Self::write_with(dir, source, |file, bytes| file.write_all(bytes))
    }

    fn write_with(
        dir: &Path,
        source: &str,
        write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
    ) -> io::Result<Self> {
        let (mut staged, mut file) = Self::create(dir)?;
        let result = (|| {
            write(&mut file, source.as_bytes())?;
            file.flush()?;
            file.sync_all()
        })();
        // same-file's Windows identity includes size, so capture it after the
        // write attempt before closing the handle used for cleanup.
        staged.identity = file
            .try_clone()
            .ok()
            .and_then(|file| Handle::from_file(file).ok());
        drop(file);
        result?;
        Ok(staged)
    }

    fn create(dir: &Path) -> io::Result<(Self, File)> {
        loop {
            let sequence = NEXT_STAGING_FILE.fetch_add(1, Ordering::Relaxed);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = dir.join(format!(
                "{STAGING_PREFIX}{}-{nonce}-{sequence}{STAGING_SUFFIX}",
                std::process::id(),
            ));
            match private_create_options().open(&path) {
                Ok(file) => {
                    return Ok((
                        Self {
                            path,
                            identity: None,
                            moved: false,
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.moved
            && let Some(identity) = &self.identity
        {
            remove_if_owned(&self.path, identity);
        }
    }
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "redox",
))]
fn rename_noclobber(source: &Path, destination: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;

    match renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(Errno::NOSYS | Errno::INVAL) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem does not support atomic no-replace rename",
        )),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn rename_noclobber(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(not(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "redox",
    windows,
)))]
fn rename_noclobber(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace rename is unavailable on this platform",
    ))
}

fn publish_noclobber(staged: &mut StagedFile, destination: &Path) -> io::Result<()> {
    rename_noclobber(&staged.path, destination)?;
    staged.moved = true;
    Ok(())
}

fn stage_and_publish(
    dir: &Path,
    destination: &Path,
    source: &str,
    write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut staged = StagedFile::write_with(dir, source, write)?;
    publish_noclobber(&mut staged, destination)
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

fn finish_rename(
    old: &Path,
    destination: &Path,
    permissions: fs::Permissions,
    remove_old: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    if let Err(error) = remove_old(old) {
        return Err(io::Error::other(format!(
            "partial parser rename: destination '{}' was written but old remains at '{}': {error}",
            destination.display(),
            old.display(),
        )));
    }
    fs::set_permissions(destination, permissions)
}

fn cleanup_stale_staging_files(dir: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(STAGING_PREFIX) || !name.ends_with(STAGING_SUFFIX) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file()
            || metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_none_or(|age| age < STALE_STAGING_AGE)
        {
            continue;
        }
        if let Ok(identity) = Handle::from_path(&path) {
            remove_if_owned(&path, &identity);
        }
    }
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
        cleanup_stale_staging_files(&self.dir, SystemTime::now());
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
        cleanup_stale_staging_files(&self.dir, SystemTime::now());
        let destination = self.dir.join(&destination_name);

        match old_name {
            None => stage_and_publish(&self.dir, &destination, source, |file, bytes| {
                file.write_all(bytes)
            })?,
            Some(old_name) if old_name == destination_name => {
                let permissions = fs::metadata(&destination)?.permissions();
                let mut staged = StagedFile::write(&self.dir, source)?;
                commit_replace(&mut staged, &destination, |source, destination| {
                    fs::rename(source, destination)
                })?;
                fs::set_permissions(&destination, permissions)?;
            }
            Some(old_name) => {
                let old = self.dir.join(old_name);
                let permissions = fs::metadata(&old)?.permissions();
                stage_and_publish(&self.dir, &destination, source, |file, bytes| {
                    file.write_all(bytes)
                })?;
                finish_rename(&old, &destination, permissions, |path| {
                    fs::remove_file(path)
                })?;
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
    fn failed_old_remove_reports_partial_completion_and_preserves_both_paths() {
        let temp = TestDir::new();
        fs::create_dir_all(temp.0.join("old.py")).unwrap();
        let library = ParserLibrary::new(&temp.0);

        let error = library
            .save(Some("old.py"), "new.py", "new source")
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("old.py"));
        assert!(message.contains("new.py"));
        assert!(message.contains("old remains"));
        assert!(temp.0.join("old.py").is_dir());
        assert_eq!(library.load("new.py").unwrap(), "new source");
    }

    #[test]
    fn injected_old_remove_failure_preserves_both_parser_files() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        let old = temp.0.join("old.py");
        let destination = temp.0.join("new.py");
        fs::write(&old, "old source").unwrap();
        stage_and_publish(&temp.0, &destination, "new source", |file, bytes| {
            file.write_all(bytes)
        })
        .unwrap();
        let permissions = fs::metadata(&old).unwrap().permissions();

        let error = finish_rename(&old, &destination, permissions, |_| {
            Err(io::Error::other("injected remove failure"))
        })
        .unwrap_err();

        assert!(error.to_string().contains("old remains"));
        assert_eq!(fs::read_to_string(old).unwrap(), "old source");
        assert_eq!(fs::read_to_string(destination).unwrap(), "new source");
    }

    #[cfg(unix)]
    #[test]
    fn new_files_are_private_and_edits_and_renames_preserve_mode() {
        use std::os::unix::fs::PermissionsExt;

        for mode in [0o600, 0o640] {
            let temp = TestDir::new();
            let library = ParserLibrary::new(&temp.0);
            library.save(None, "parser.py", "original").unwrap();
            let path = temp.0.join("parser.py");
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();

            library
                .save(Some("parser.py"), "parser.py", "replacement")
                .unwrap();

            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                mode
            );

            library
                .save(Some("parser.py"), "renamed.py", "renamed")
                .unwrap();
            assert_eq!(
                fs::metadata(temp.0.join("renamed.py"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                mode
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn staging_files_are_private_until_publication() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        let staged = StagedFile::write(&temp.0, "replacement").unwrap();

        assert_eq!(
            fs::metadata(&staged.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn failed_staged_write_never_exposes_destination_and_cleans_temp() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        let destination = temp.0.join("parser.py");

        let error = stage_and_publish(&temp.0, &destination, "replacement", |file, bytes| {
            file.write_all(&bytes[..3])?;
            assert!(!destination.exists());
            Err(io::Error::other("injected write failure"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
    }

    #[test]
    fn completed_staging_publishes_atomically_without_overwrite() {
        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        let destination = temp.0.join("parser.py");
        let mut staged = StagedFile::write(&temp.0, "complete source").unwrap();
        assert!(!destination.exists());

        publish_noclobber(&mut staged, &destination).unwrap();

        assert_eq!(fs::read_to_string(&destination).unwrap(), "complete source");
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
    }

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "redox",
        windows,
    ))]
    #[test]
    fn concurrent_publishers_never_overwrite_each_other() {
        use std::sync::{Arc, Barrier};

        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        let destination = temp.0.join("parser.py");
        let first = StagedFile::write(&temp.0, "first source").unwrap();
        let second = StagedFile::write(&temp.0, "second source").unwrap();
        let barrier = Arc::new(Barrier::new(2));

        let results = std::thread::scope(|scope| {
            let publish = |mut staged: StagedFile, barrier: Arc<Barrier>| {
                let destination = destination.clone();
                scope.spawn(move || {
                    barrier.wait();
                    publish_noclobber(&mut staged, &destination).map_err(|error| error.kind())
                })
            };
            let first = publish(first, Arc::clone(&barrier));
            let second = publish(second, barrier);
            [first.join().unwrap(), second.join().unwrap()]
        });

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(io::ErrorKind::AlreadyExists)))
                .count(),
            1
        );
        assert!(matches!(
            fs::read_to_string(&destination).unwrap().as_str(),
            "first source" | "second source"
        ));
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
    }

    #[test]
    fn list_cleans_only_staging_files_older_than_one_day() {
        use std::time::Duration;

        let temp = TestDir::new();
        fs::create_dir_all(&temp.0).unwrap();
        let stale = temp.0.join(".delog-parser-stale.tmp");
        let recent = temp.0.join(".delog-parser-recent.tmp");
        fs::write(&stale, "stale").unwrap();
        fs::write(&recent, "recent").unwrap();
        let old = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();

        let library = ParserLibrary::new(&temp.0);
        library.list().unwrap();

        assert!(!stale.exists());
        assert!(recent.exists());
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

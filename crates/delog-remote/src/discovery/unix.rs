use std::env;
use std::fs::{DirBuilder, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, Stat, fchmod, fstat, openat, statat};
use rustix::io::Errno;
use rustix::process::{Pid, geteuid, test_kill_process};

use super::{DiscoveryError, insecure};

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const GROUP_OTHER_BITS: u32 = 0o077;
const GROUP_OTHER_WRITE: u32 = 0o022;
const STICKY: u32 = 0o1000;

fn mode(stat: &Stat) -> u32 {
    let raw: u32 = stat.st_mode as _;
    raw
}

fn file_type(stat: &Stat) -> FileType {
    FileType::from_raw_mode(stat.st_mode as _)
}

fn owned_by_me(stat: &Stat) -> bool {
    stat.st_uid == geteuid().as_raw()
}

fn io_error(error: Errno) -> DiscoveryError {
    DiscoveryError::Io(error.into())
}

fn lstat(path: &Path) -> Result<Stat, Errno> {
    statat(CWD, path, AtFlags::SYMLINK_NOFOLLOW)
}

pub(super) fn verify_dir(path: &Path) -> Result<(), DiscoveryError> {
    let stat = lstat(path).map_err(io_error)?;
    if file_type(&stat) != FileType::Directory
        || !owned_by_me(&stat)
        || mode(&stat) & GROUP_OTHER_BITS != 0
    {
        return Err(insecure(path));
    }
    Ok(())
}

fn secure_dir(path: &Path) -> Result<(), DiscoveryError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let dir = openat(CWD, path, flags, Mode::empty()).map_err(|error| match error {
        Errno::LOOP | Errno::NOTDIR => insecure(path),
        error => io_error(error),
    })?;
    let stat = fstat(&dir).map_err(io_error)?;
    if file_type(&stat) != FileType::Directory
        || !owned_by_me(&stat)
        || mode(&stat) & GROUP_OTHER_WRITE != 0
    {
        return Err(insecure(path));
    }
    if mode(&stat) & GROUP_OTHER_BITS != 0 {
        fchmod(&dir, Mode::from_raw_mode(PRIVATE_DIR_MODE as _)).map_err(io_error)?;
    }
    Ok(())
}

fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => path,
    }
}

fn verify_parent(path: &Path) -> Result<(), DiscoveryError> {
    let parent = parent_of(path);
    let stat = statat(CWD, parent, AtFlags::empty()).map_err(io_error)?;
    let trusted_owner = owned_by_me(&stat) || stat.st_uid == 0;
    let shared_writable = mode(&stat) & GROUP_OTHER_WRITE != 0 && mode(&stat) & STICKY == 0;
    if file_type(&stat) != FileType::Directory || !trusted_owner || shared_writable {
        return Err(insecure(parent));
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), DiscoveryError> {
    match DirBuilder::new().mode(PRIVATE_DIR_MODE).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn prepare_component(path: &Path) -> Result<(), DiscoveryError> {
    verify_parent(path)?;
    create_private_dir(path)?;
    secure_dir(path)?;
    verify_dir(path)
}

pub(super) fn prepare_root(root: &Path) -> Result<(), DiscoveryError> {
    match lstat(root) {
        Ok(_) => {}
        Err(Errno::NOENT) => {
            let parent = parent_of(root);
            if let Err(Errno::NOENT) = statat(CWD, parent, AtFlags::empty()) {
                DirBuilder::new()
                    .recursive(true)
                    .mode(PRIVATE_DIR_MODE)
                    .create(parent)?;
            }
            verify_parent(root)?;
            create_private_dir(root)?;
        }
        Err(error) => return Err(io_error(error)),
    }
    verify_parent(root)?;
    secure_dir(root)?;
    verify_dir(root)
}

pub(super) fn prepare_default_root() -> Result<PathBuf, DiscoveryError> {
    let euid = geteuid().as_raw();
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let base = match runtime {
        Some(runtime) => runtime.join("delog"),
        None => env::temp_dir().join(format!("delog-runtime-{euid}")),
    };
    if !base.is_absolute() || !parent_of(&base).is_dir() {
        return Err(DiscoveryError::NoRuntimeDirectory);
    }
    prepare_component(&base)?;
    let instances = base.join("instances");
    prepare_component(&instances)?;
    Ok(instances)
}

pub(super) fn create_private_file(path: &Path) -> Result<File, DiscoveryError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)?;
    let verified = file
        .set_permissions(Permissions::from_mode(PRIVATE_FILE_MODE))
        .map_err(DiscoveryError::from)
        .and_then(|()| {
            let stat = fstat(&file).map_err(io_error)?;
            if file_type(&stat) != FileType::RegularFile
                || !owned_by_me(&stat)
                || mode(&stat) & 0o777 != PRIVATE_FILE_MODE
            {
                return Err(insecure(path));
            }
            Ok(())
        });
    if let Err(error) = verified {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

pub(super) fn is_private_file(path: &Path) -> bool {
    lstat(path).is_ok_and(|stat| {
        file_type(&stat) == FileType::RegularFile
            && owned_by_me(&stat)
            && mode(&stat) & GROUP_OTHER_BITS == 0
    })
}

pub(super) fn pid_alive(pid: u32) -> bool {
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return true;
    };
    !matches!(test_kill_process(pid), Err(Errno::SRCH))
}

pub(super) fn sync_dir(root: &Path) {
    if let Ok(dir) = File::open(root) {
        let _ = dir.sync_all();
    }
}

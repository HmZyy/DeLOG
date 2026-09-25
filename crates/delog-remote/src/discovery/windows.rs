use std::env;
use std::ffi::{OsStr, c_void};
use std::fs::{self, File};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_SUCCESS, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree, STILL_ACTIVE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, GetNamedSecurityInfoW, GetSecurityInfo, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorDacl,
    GetTokenInformation, INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::{DiscoveryError, insecure};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const SYSTEM_SID: &str = "S-1-5-18";
const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
const CREATOR_OWNER_SID: &str = "S-1-3-0";

struct LocalBox(*mut c_void);

impl Drop for LocalBox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains an interior NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

unsafe fn wide_to_string(value: *const u16) -> io::Result<String> {
    let mut len = 0;
    while unsafe { *value.add(len) } != 0 {
        len += 1;
    }
    let slice = unsafe { std::slice::from_raw_parts(value, len) };
    String::from_utf16(slice).map_err(|_| io::Error::other("invalid security identifier"))
}

fn current_user_sid_string() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    let mut len = 0u32;
    unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            len,
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut string: *mut u16 = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut string) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let string = LocalBox(string.cast());
    unsafe { wide_to_string(string.0.cast()) }
}

struct Sid(LocalBox);

impl Sid {
    fn parse(value: &str) -> io::Result<Self> {
        let value = wide(OsStr::new(value))?;
        let mut sid: PSID = ptr::null_mut();
        if unsafe { ConvertStringSidToSidW(value.as_ptr(), &mut sid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(LocalBox(sid)))
    }

    fn matches(&self, other: PSID) -> bool {
        !other.is_null() && unsafe { EqualSid(self.0.0, other) } != 0
    }
}

struct Principals {
    user_string: String,
    user: Sid,
    system: Sid,
    administrators: Sid,
    creator_owner: Sid,
}

impl Principals {
    fn current() -> io::Result<Self> {
        let user_string = current_user_sid_string()?;
        Ok(Self {
            user: Sid::parse(&user_string)?,
            user_string,
            system: Sid::parse(SYSTEM_SID)?,
            administrators: Sid::parse(ADMINISTRATORS_SID)?,
            creator_owner: Sid::parse(CREATOR_OWNER_SID)?,
        })
    }

    fn trusted(&self, sid: PSID) -> bool {
        self.user.matches(sid) || self.system.matches(sid) || self.administrators.matches(sid)
    }

    fn security_is_private(&self, owner: PSID, dacl: *mut ACL) -> bool {
        if owner.is_null() || dacl.is_null() || !self.trusted(owner) {
            return false;
        }
        let mut info = ACL_SIZE_INFORMATION {
            AceCount: 0,
            AclBytesInUse: 0,
            AclBytesFree: 0,
        };
        if unsafe {
            GetAclInformation(
                dacl,
                (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return false;
        }
        for index in 0..info.AceCount {
            let mut ace: *mut c_void = ptr::null_mut();
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
                return false;
            }
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            match header.AceType {
                ACCESS_DENIED_ACE_TYPE => {}
                ACCESS_ALLOWED_ACE_TYPE => {
                    let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
                    let sid = unsafe { ptr::addr_of_mut!((*allowed).SidStart) }.cast::<c_void>();
                    let inherit_only = u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0;
                    if !(self.trusted(sid) || (inherit_only && self.creator_owner.matches(sid))) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

struct SecurityInfo {
    _descriptor: LocalBox,
    owner: PSID,
    dacl: *mut ACL,
}

fn query_path(path: &Path) -> Option<SecurityInfo> {
    let name = wide(path.as_os_str()).ok()?;
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    let descriptor = LocalBox(descriptor);
    (status == ERROR_SUCCESS).then_some(SecurityInfo {
        _descriptor: descriptor,
        owner,
        dacl,
    })
}

fn query_handle(file: &File) -> Option<SecurityInfo> {
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    let descriptor = LocalBox(descriptor);
    (status == ERROR_SUCCESS).then_some(SecurityInfo {
        _descriptor: descriptor,
        owner,
        dacl,
    })
}

fn is_private(info: Option<SecurityInfo>) -> bool {
    let Ok(principals) = Principals::current() else {
        return false;
    };
    info.is_some_and(|info| principals.security_is_private(info.owner, info.dacl))
}

fn path_is_private(path: &Path) -> bool {
    is_private(query_path(path))
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub(super) fn verify_dir(path: &Path) -> Result<(), DiscoveryError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_reparse_point(&metadata) || !path_is_private(path) {
        return Err(insecure(path));
    }
    Ok(())
}

fn private_security(principals: &Principals, inheritance: &str) -> io::Result<LocalBox> {
    let user = &principals.user_string;
    let sddl = format!(
        "D:P(A;{inheritance};FA;;;{user})(A;{inheritance};FA;;;SY)(A;{inheritance};FA;;;BA)"
    );
    let sddl = wide(OsStr::new(&sddl))?;
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalBox(descriptor))
}

fn attributes(descriptor: &LocalBox) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    }
}

fn create_private_dir(path: &Path) -> Result<(), DiscoveryError> {
    let principals = Principals::current()?;
    let descriptor = private_security(&principals, "OICI")?;
    let attributes = attributes(&descriptor);
    let name = wide(path.as_os_str())?;
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
    }
    Ok(())
}

fn secure_dir(path: &Path) -> Result<(), DiscoveryError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err(insecure(path));
    }
    if path_is_private(path) {
        return Ok(());
    }
    let principals = Principals::current()?;
    let owned = query_path(path).is_some_and(|info| principals.user.matches(info.owner));
    if !owned {
        return Err(insecure(path));
    }
    let descriptor = private_security(&principals, "OICI")?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut ACL = ptr::null_mut();
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
        || present == 0
        || dacl.is_null()
    {
        return Err(insecure(path));
    }
    let name = wide(path.as_os_str())?;
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    let handle = OwnedHandle(handle);
    let status = unsafe {
        SetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(insecure(path));
    }
    Ok(())
}

fn prepare_component(path: &Path) -> Result<(), DiscoveryError> {
    create_private_dir(path)?;
    secure_dir(path)?;
    verify_dir(path)
}

pub(super) fn prepare_root(root: &Path) -> Result<(), DiscoveryError> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)?;
            }
            create_private_dir(root)?;
        }
        Err(error) => return Err(error.into()),
    }
    secure_dir(root)?;
    verify_dir(root)
}

pub(super) fn prepare_default_root() -> Result<PathBuf, DiscoveryError> {
    let local = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .ok_or(DiscoveryError::NoRuntimeDirectory)?;
    let app = local.join("DeLOG");
    fs::create_dir_all(&app)?;
    let runtime = app.join("runtime");
    prepare_component(&runtime)?;
    let instances = runtime.join("instances");
    prepare_component(&instances)?;
    Ok(instances)
}

pub(super) fn create_private_file(path: &Path) -> Result<File, DiscoveryError> {
    let principals = Principals::current()?;
    let descriptor = private_security(&principals, "")?;
    let attributes = attributes(&descriptor);
    let name = wide(path.as_os_str())?;
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE | READ_CONTROL,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    if !is_private(query_handle(&file)) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(insecure(path));
    }
    Ok(file)
}

pub(super) fn is_private_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && !is_reparse_point(&metadata) && path_is_private(path)
    })
}

pub(super) fn pid_alive(pid: u32) -> bool {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return unsafe { GetLastError() } != ERROR_INVALID_PARAMETER;
    }
    let handle = OwnedHandle(handle);
    let mut code = 0u32;
    if unsafe { GetExitCodeProcess(handle.0, &mut code) } == 0 {
        return true;
    }
    code == STILL_ACTIVE as u32
}

pub(super) fn sync_dir(_root: &Path) {}

from __future__ import annotations

import sys

if sys.platform == "win32":
    import ctypes
    from ctypes import wintypes

    _advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

    _SE_FILE_OBJECT = 1
    _OWNER_SECURITY_INFORMATION = 0x1
    _DACL_SECURITY_INFORMATION = 0x4
    _TOKEN_QUERY = 0x0008
    _TOKEN_USER = 1
    _ACL_SIZE_INFORMATION_CLASS = 2
    _ACCESS_ALLOWED_ACE_TYPE = 0
    _ACCESS_DENIED_ACE_TYPE = 1
    _INHERIT_ONLY_ACE = 0x08
    _PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    _ERROR_INVALID_PARAMETER = 87
    _STILL_ACTIVE = 259
    _SYSTEM_SID = "S-1-5-18"
    _ADMINISTRATORS_SID = "S-1-5-32-544"
    _CREATOR_OWNER_SID = "S-1-3-0"

    class _AclSizeInformation(ctypes.Structure):
        _fields_ = [
            ("AceCount", wintypes.DWORD),
            ("AclBytesInUse", wintypes.DWORD),
            ("AclBytesFree", wintypes.DWORD),
        ]

    class _AceHeader(ctypes.Structure):
        _fields_ = [
            ("AceType", wintypes.BYTE),
            ("AceFlags", wintypes.BYTE),
            ("AceSize", wintypes.WORD),
        ]

    _advapi32.GetNamedSecurityInfoW.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
    ]
    _advapi32.GetNamedSecurityInfoW.restype = wintypes.DWORD
    _advapi32.OpenProcessToken.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.HANDLE),
    ]
    _advapi32.OpenProcessToken.restype = wintypes.BOOL
    _advapi32.GetTokenInformation.argtypes = [
        wintypes.HANDLE,
        ctypes.c_int,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
    ]
    _advapi32.GetTokenInformation.restype = wintypes.BOOL
    _advapi32.EqualSid.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    _advapi32.EqualSid.restype = wintypes.BOOL
    _advapi32.ConvertStringSidToSidW.argtypes = [
        wintypes.LPCWSTR,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    _advapi32.ConvertStringSidToSidW.restype = wintypes.BOOL
    _advapi32.GetAclInformation.argtypes = [
        ctypes.c_void_p,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_int,
    ]
    _advapi32.GetAclInformation.restype = wintypes.BOOL
    _advapi32.GetAce.argtypes = [
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(ctypes.c_void_p),
    ]
    _advapi32.GetAce.restype = wintypes.BOOL
    _kernel32.GetCurrentProcess.argtypes = []
    _kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    _kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    _kernel32.OpenProcess.restype = wintypes.HANDLE
    _kernel32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    _kernel32.GetExitCodeProcess.restype = wintypes.BOOL
    _kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    _kernel32.CloseHandle.restype = wintypes.BOOL
    _kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    _kernel32.LocalFree.restype = ctypes.c_void_p

    def _string_sid(value: str) -> ctypes.c_void_p | None:
        sid = ctypes.c_void_p()
        if not _advapi32.ConvertStringSidToSidW(value, ctypes.byref(sid)):
            return None
        return sid

    def _user_sid_buffer() -> ctypes.Array[ctypes.c_char] | None:
        token = wintypes.HANDLE()
        if not _advapi32.OpenProcessToken(
            _kernel32.GetCurrentProcess(), _TOKEN_QUERY, ctypes.byref(token)
        ):
            return None
        try:
            needed = wintypes.DWORD()
            _advapi32.GetTokenInformation(token, _TOKEN_USER, None, 0, ctypes.byref(needed))
            if needed.value == 0:
                return None
            buffer = ctypes.create_string_buffer(needed.value)
            if not _advapi32.GetTokenInformation(
                token, _TOKEN_USER, buffer, needed, ctypes.byref(needed)
            ):
                return None
            return buffer
        finally:
            _kernel32.CloseHandle(token)

    def _security_is_private(owner: ctypes.c_void_p, dacl: ctypes.c_void_p) -> bool:
        user_buffer = _user_sid_buffer()
        if user_buffer is None or not owner.value or not dacl.value:
            return False
        user = ctypes.c_void_p.from_buffer(user_buffer)
        well_known = [
            _string_sid(s) for s in (_SYSTEM_SID, _ADMINISTRATORS_SID, _CREATOR_OWNER_SID)
        ]
        try:
            if any(sid is None for sid in well_known):
                return False
            system, administrators, creator_owner = (sid for sid in well_known if sid is not None)

            def trusted(sid: ctypes.c_void_p) -> bool:
                return any(
                    bool(_advapi32.EqualSid(sid, candidate))
                    for candidate in (user, system, administrators)
                )

            if not trusted(owner):
                return False
            info = _AclSizeInformation()
            if not _advapi32.GetAclInformation(
                dacl, ctypes.byref(info), ctypes.sizeof(info), _ACL_SIZE_INFORMATION_CLASS
            ):
                return False
            for index in range(info.AceCount):
                ace = ctypes.c_void_p()
                if not _advapi32.GetAce(dacl, index, ctypes.byref(ace)) or not ace.value:
                    return False
                header = _AceHeader.from_address(ace.value)
                if header.AceType == _ACCESS_DENIED_ACE_TYPE:
                    continue
                if header.AceType != _ACCESS_ALLOWED_ACE_TYPE:
                    return False
                ace_sid = ctypes.c_void_p(ace.value + ctypes.sizeof(_AceHeader) + 4)
                inherit_only = header.AceFlags & _INHERIT_ONLY_ACE != 0
                if not (
                    trusted(ace_sid)
                    or (inherit_only and bool(_advapi32.EqualSid(ace_sid, creator_owner)))
                ):
                    return False
            return True
        finally:
            for allocated in well_known:
                if allocated is not None:
                    _kernel32.LocalFree(allocated)

    def is_private(path: str) -> bool:
        owner = ctypes.c_void_p()
        dacl = ctypes.c_void_p()
        descriptor = ctypes.c_void_p()
        status = _advapi32.GetNamedSecurityInfoW(
            path,
            _SE_FILE_OBJECT,
            _OWNER_SECURITY_INFORMATION | _DACL_SECURITY_INFORMATION,
            ctypes.byref(owner),
            None,
            ctypes.byref(dacl),
            None,
            ctypes.byref(descriptor),
        )
        if status != 0:
            return False
        try:
            return _security_is_private(owner, dacl)
        finally:
            _kernel32.LocalFree(descriptor)

    def pid_alive(pid: int) -> bool:
        handle = _kernel32.OpenProcess(_PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
        if not handle:
            return ctypes.get_last_error() != _ERROR_INVALID_PARAMETER
        try:
            code = wintypes.DWORD()
            if not _kernel32.GetExitCodeProcess(handle, ctypes.byref(code)):
                return True
            return code.value == _STILL_ACTIVE
        finally:
            _kernel32.CloseHandle(handle)

//! The Windows implementation. Every `unsafe` block is one Win32 call.

use std::{
    ffi::{OsStr, c_void},
    fs::{File, OpenOptions},
    io,
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};

use windows_sys::Win32::{
    Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
        GetTokenInformation, INHERITED_ACE, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER, TokenUser, WinLocalSystemSid,
    },
    Storage::FileSystem::{
        CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_ALWAYS, OPEN_EXISTING,
    },
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE},
        Threading::{GetCurrentProcess, OpenProcessToken},
        WindowsProgramming::QueryInterruptTime,
    },
};

/// Milliseconds since boot on the interrupt-time clock, sleep included.
///
/// `QueryInterruptTime` counts 100-nanosecond intervals since boot and, as
/// Microsoft documents, includes time spent in sleep or hibernation. Its
/// "unbiased" counterpart, `QueryUnbiasedInterruptTime`, excludes that time,
/// so it is not used here. Requires Windows 10 or later.
pub fn interrupt_time_ms() -> u64 {
    let mut ticks = 0u64;
    // SAFETY: QueryInterruptTime writes one u64 through a valid, exclusive
    // pointer and has no failure mode.
    unsafe { QueryInterruptTime(&mut ticks) };
    ticks / 10_000
}

/// Creates one directory, `path`, that only the current user (and SYSTEM)
/// can use: the current user owns it, and its DACL is protected, so nothing
/// is inherited from the parent. The parent must exist. An existing path is
/// an error of kind [`io::ErrorKind::AlreadyExists`]; it is not checked or
/// changed.
pub fn create_private_directory(path: &Path) -> io::Result<()> {
    let descriptor = owner_only_descriptor(true)?;
    let attributes = security_attributes(&descriptor);
    let path = wide(path.as_os_str());
    // SAFETY: `path` is NUL-terminated UTF-16 and `attributes` points to a
    // security descriptor that outlives the call.
    if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Opens the file at `path` for reading without following a final symbolic
/// link or junction, which is opened itself instead. With `create`, the file
/// is also opened for writing and, if missing, created owner-only as in
/// [`create_private_directory`]. An existing file is never truncated, and its
/// permissions are not changed: check them with [`is_owner_only`].
pub fn open_private_file(path: &Path, create: bool) -> io::Result<File> {
    let descriptor = if create {
        Some(owner_only_descriptor(false)?)
    } else {
        None
    };
    let attributes = descriptor.as_ref().map(security_attributes);
    let access = if create {
        GENERIC_READ | GENERIC_WRITE
    } else {
        GENERIC_READ
    };
    let path = wide(path.as_os_str());
    // SAFETY: `path` is NUL-terminated UTF-16, the security attributes are
    // null or point to a descriptor that outlives the call, and no template
    // file is passed.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            attributes.as_ref().map_or(ptr::null(), ptr::from_ref),
            if create { OPEN_ALWAYS } else { OPEN_EXISTING },
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW succeeded, so `handle` is an open handle that
    // nothing else owns.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
}

/// Opens a file or directory for reading its metadata and permissions,
/// without following a final symbolic link or junction.
pub fn open_no_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// Whether `file` is owner-only: it is owned by the current user, its DACL
/// is protected from inheritance, it holds no inherited entry, and every entry
/// that grants access names the current user or SYSTEM. Entries that deny
/// access are allowed, and so is an empty DACL, which grants nothing. A
/// missing (null) DACL grants everyone everything, so it is not owner-only.
pub fn is_owner_only(file: &File) -> io::Result<bool> {
    let user = UserSid::current()?;
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is open for the life of `file`, and every out
    // pointer is valid. The owner and DACL point into `descriptor`, which is
    // freed only when `_descriptor` drops at the end of this function.
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
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _descriptor = LocalBox(descriptor);
    // SAFETY: both SIDs are valid: `owner` from GetSecurityInfo and the
    // user's from the token.
    if unsafe { EqualSid(owner, user.sid()) } == 0 || dacl.is_null() {
        return Ok(false);
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: `descriptor` is a valid security descriptor and both out
    // pointers are valid.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Ok(false);
    }
    // SAFETY: `dacl` is non-null and points to the DACL in `descriptor`.
    let count = unsafe { (*dacl).AceCount };
    for index in 0..u32::from(count) {
        let mut ace: *mut c_void = ptr::null_mut();
        // SAFETY: `dacl` is a valid ACL and `index` is below its entry count.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: GetAce returned a pointer to an entry inside the ACL, and
        // every entry starts with an ACE_HEADER.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERITED_ACE != 0 {
            return Ok(false);
        }
        match u32::from(header.AceType) {
            ACCESS_DENIED_ACE_TYPE => {}
            ACCESS_ALLOWED_ACE_TYPE => {
                // SAFETY: an ACCESS_ALLOWED_ACE stores its SID in place,
                // starting at `SidStart`, within the entry's size.
                let sid: PSID = unsafe {
                    ptr::addr_of_mut!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast()
                };
                // SAFETY: `sid` and the user's SID are both valid SIDs.
                let allowed = unsafe {
                    EqualSid(sid, user.sid()) != 0 || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                };
                if !allowed {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// A security descriptor that grants full control to the current user and
/// SYSTEM only, owned by the current user and protected from inheritance. A
/// directory's entries are also inherited by what is created inside it.
fn owner_only_descriptor(directory: bool) -> io::Result<LocalBox> {
    let user = UserSid::current()?.to_sddl()?;
    let inherit = if directory { "OICI" } else { "" };
    let sddl = wide(OsStr::new(&format!(
        "O:{user}D:P(A;{inherit};FA;;;{user})(A;{inherit};FA;;;SY)"
    )));
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated UTF-16 and the out pointer is valid;
    // the descriptor is freed with LocalFree when the LocalBox drops.
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

fn security_attributes(descriptor: &LocalBox) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

/// Memory that a Win32 call allocated for the caller to free with LocalFree.
struct LocalBox(*mut c_void);

impl Drop for LocalBox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a call documented to allocate
            // with LocalAlloc, and it is freed once, here.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// The current process's user SID, read from its token.
struct UserSid {
    /// A TOKEN_USER and the SID it points to. u64 keeps it 8-byte aligned.
    buffer: Vec<u64>,
}

impl UserSid {
    fn current() -> io::Result<Self> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // closing, and the out pointer is valid.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcessToken succeeded, so `token` is an open handle
        // that nothing else owns.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut needed = 0u32;
        // SAFETY: with a null buffer of length zero the call only reports
        // the size it needs; its expected failure is ignored.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        let length = u32::try_from(buffer.len() * 8).unwrap_or(u32::MAX);
        // SAFETY: `buffer` is writable, 8-byte aligned and `length` bytes
        // long, and the token is open with TOKEN_QUERY.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { buffer })
    }

    fn sid(&self) -> PSID {
        // SAFETY: GetTokenInformation wrote a TOKEN_USER at the start of the
        // buffer, and its SID points into the same buffer, whose heap
        // allocation does not move while `self` lives.
        unsafe { (*self.buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    fn to_sddl(&self) -> io::Result<String> {
        let mut text = ptr::null_mut::<u16>();
        // SAFETY: the SID is valid and the out pointer is valid; the string
        // is freed with LocalFree when the LocalBox drops.
        if unsafe { ConvertSidToStringSidW(self.sid(), &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _text = LocalBox(text.cast());
        let mut length = 0;
        // SAFETY: ConvertSidToStringSidW returns a NUL-terminated string.
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: the `length` characters before the NUL are initialized.
        let units = unsafe { std::slice::from_raw_parts(text, length) };
        String::from_utf16(units).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};
    use windows_sys::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;

    /// A new, empty directory under the system temporary directory, removed
    /// when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "langspan-windows-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_interrupt_clock_never_goes_back_and_includes_sleep() {
        let before = interrupt_time_ms();
        let after = interrupt_time_ms();
        assert!(after >= before && before > 0);
        let mut unbiased = 0u64;
        // SAFETY: one u64 written through a valid pointer.
        assert_ne!(unsafe { QueryUnbiasedInterruptTime(&mut unbiased) }, 0);
        // The unbiased clock leaves sleep out, so it can only be behind the
        // clock that counts it (equal if this machine has not slept since boot).
        assert!(interrupt_time_ms() >= unbiased / 10_000);
    }

    #[test]
    fn a_private_directory_is_owner_only_and_an_ordinary_one_is_not() {
        let scratch = Scratch::new("directory");
        let private = scratch.0.join("private");
        create_private_directory(&private).unwrap();
        assert!(is_owner_only(&open_no_follow(&private).unwrap()).unwrap());
        // What is created inside inherits the owner-only entries.
        let inner = private.join("inner");
        fs::create_dir(&inner).unwrap();
        assert!(fs::metadata(&inner).unwrap().is_dir());

        let ordinary = scratch.0.join("ordinary");
        fs::create_dir(&ordinary).unwrap();
        assert!(!is_owner_only(&open_no_follow(&ordinary).unwrap()).unwrap());

        let again = create_private_directory(&private).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_private_file_is_owner_only_and_an_ordinary_one_is_not() {
        let scratch = Scratch::new("file");
        let private = scratch.0.join("private.json");
        {
            use std::io::Write;
            let mut file = open_private_file(&private, true).unwrap();
            file.write_all(b"{}").unwrap();
            assert!(is_owner_only(&file).unwrap());
        }
        // Reopened without creating: readable, not truncated, still private.
        let reopened = open_private_file(&private, false).unwrap();
        assert!(is_owner_only(&reopened).unwrap());
        assert_eq!(reopened.metadata().unwrap().len(), 2);

        let ordinary = scratch.0.join("ordinary.json");
        fs::write(&ordinary, b"{}").unwrap();
        let file = open_private_file(&ordinary, false).unwrap();
        assert!(!is_owner_only(&file).unwrap());

        let missing = open_private_file(&scratch.0.join("missing"), false).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }
}

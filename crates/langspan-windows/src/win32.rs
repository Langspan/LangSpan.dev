//! The Windows implementation. Every `unsafe` block is one Win32 call, or
//! one read of memory a call returned, bounded by a size the API reported.

use std::{
    ffi::{OsStr, c_void},
    fs::{File, OpenOptions},
    io,
    mem::MaybeUninit,
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};

use windows_sys::Win32::{
    Foundation::{
        ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
        LocalFree,
    },
    Security::{
        ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
            SE_FILE_OBJECT,
        },
        DACL_SECURITY_INFORMATION, GetAclInformation, GetLengthSid, GetSecurityDescriptorControl,
        GetTokenInformation, INHERITED_ACE, IsValidAcl, IsValidSid, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
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
    let mut owner = MaybeUninit::<PSID>::uninit();
    let mut dacl = MaybeUninit::<*mut ACL>::uninit();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is open for the life of `file`, and every out
    // pointer is valid for one pointer-sized write. The owner and DACL point
    // into `descriptor`, which is freed only when `_descriptor` drops at the
    // end of this function.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            owner.as_mut_ptr(),
            ptr::null_mut(),
            dacl.as_mut_ptr(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _descriptor = LocalBox(descriptor);
    // SAFETY: GetSecurityInfo succeeded, and on success it writes both out
    // pointers, since both kinds of information were requested.
    let (owner, dacl) = unsafe { (owner.assume_init(), dacl.assume_init()) };
    if owner.is_null() || descriptor.is_null() {
        return Err(invalid_data(
            "GetSecurityInfo returned no owner or descriptor",
        ));
    }
    // SAFETY: `owner` is a non-null SID pointer into `descriptor`, which is
    // alive; IsValidSid only reads it.
    if unsafe { IsValidSid(owner) } == 0 {
        return Err(invalid_data("the owner is not a valid SID"));
    }
    // SAFETY: IsValidSid accepted `owner`, so GetLengthSid reads only its
    // header and returns the SID's length, all of it inside `descriptor`.
    let owner_length = unsafe { GetLengthSid(owner) } as usize;
    // SAFETY: `owner` is valid for `owner_length` bytes, as GetLengthSid just
    // reported, and they are not written while `descriptor` lives.
    let owner = unsafe { std::slice::from_raw_parts(owner.cast::<u8>(), owner_length) };
    if owner != user.bytes() || dacl.is_null() {
        return Ok(false);
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: `descriptor` is the valid security descriptor GetSecurityInfo
    // returned, and both out pointers are valid.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Ok(false);
    }
    // SAFETY: `dacl` is non-null and points to the DACL inside `descriptor`;
    // IsValidAcl only reads it.
    if unsafe { IsValidAcl(dacl) } == 0 {
        return Err(invalid_data("the DACL is not a valid ACL"));
    }
    let mut size = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };
    // SAFETY: `dacl` is a valid ACL, and `size` is a writable
    // ACL_SIZE_INFORMATION whose exact size is passed.
    if unsafe {
        GetAclInformation(
            dacl,
            ptr::from_mut(&mut size).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetAclInformation reports `AclBytesInUse` as the bytes of the
    // ACL, header included, that are in use; they are contiguous from
    // `dacl`, initialized, and not written while `descriptor` lives.
    let acl = unsafe { std::slice::from_raw_parts(dacl.cast::<u8>(), size.AclBytesInUse as usize) };
    entries_are_owner_only(acl, size.AceCount, user.bytes())
}

/// Whether the `count` entries of an ACL, whose bytes in use are `acl`, are
/// owner-only as [`is_owner_only`] describes, for the binary SID `user`.
/// Entries are read from the slice with every offset bounds-checked, so a
/// malformed ACL is an error, never a read outside it.
fn entries_are_owner_only(acl: &[u8], count: u32, user: &[u8]) -> io::Result<bool> {
    let malformed = || invalid_data("the DACL has a malformed entry");
    // An ACL header is 8 bytes. Each entry starts with a 4-byte ACE_HEADER
    // (type, flags, little-endian size), and an ACCESS_ALLOWED_ACE stores its
    // SID after a 4-byte access mask.
    let mut offset = size_of::<ACL>();
    for _ in 0..count {
        let [kind, flags, low, high] = *acl
            .get(offset..)
            .and_then(<[u8]>::first_chunk::<4>)
            .ok_or_else(malformed)?;
        let length = usize::from(u16::from_le_bytes([low, high]));
        if length < 4 {
            return Err(malformed());
        }
        let entry = acl.get(offset..offset + length).ok_or_else(malformed)?;
        offset += length;
        if u32::from(flags) & INHERITED_ACE != 0 {
            return Ok(false);
        }
        match u32::from(kind) {
            ACCESS_DENIED_ACE_TYPE => {}
            ACCESS_ALLOWED_ACE_TYPE => {
                let sid = entry.get(8..).and_then(sid_prefix).ok_or_else(malformed)?;
                if sid != user && sid != LOCAL_SYSTEM_SID {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// SYSTEM's SID, S-1-5-18, in binary form: revision 1, one subauthority,
/// the NT authority (5) as 6 big-endian bytes, and 18 as a little-endian u32.
const LOCAL_SYSTEM_SID: &[u8] = &[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];

/// The binary SID at the start of `bytes`, if one fits there: an 8-byte
/// revision-1 header and the 4-byte subauthorities it counts, at most 15.
fn sid_prefix(bytes: &[u8]) -> Option<&[u8]> {
    let (&revision, &count) = (bytes.first()?, bytes.get(1)?);
    if revision != 1 || count > 15 {
        return None;
    }
    bytes.get(..8 + 4 * usize::from(count))
}

/// A binary SID from [`sid_prefix`] in string form, such as `S-1-5-18`, as
/// `ConvertSidToStringSidW` and SDDL write it; `None` if it is shorter than
/// a SID's 8-byte header.
fn sid_string(sid: &[u8]) -> Option<String> {
    let (&[revision, _, ref authority @ ..], subauthorities) = sid.split_first_chunk::<8>()?;
    let authority = authority
        .iter()
        .fold(0u64, |value, &byte| (value << 8) | u64::from(byte));
    let mut text = if authority < 1 << 32 {
        format!("S-{revision}-{authority}")
    } else {
        format!("S-{revision}-0x{authority:012X}")
    };
    for &part in subauthorities.as_chunks::<4>().0 {
        text.push_str(&format!("-{}", u32::from_le_bytes(part)));
    }
    Some(text)
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

/// A security descriptor that grants full control to the current user and
/// SYSTEM only, owned by the current user and protected from inheritance. A
/// directory's entries are also inherited by what is created inside it.
fn owner_only_descriptor(directory: bool) -> io::Result<LocalBox> {
    let user = sid_string(UserSid::current()?.bytes())
        .ok_or_else(|| invalid_data("the user's SID is too short"))?;
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

/// The current process's user SID, read from its token, in binary form.
struct UserSid(Vec<u8>);

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
        // SAFETY: with a null buffer of length zero the call writes nothing
        // but `needed`, the size it requires.
        let sized = unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        // The sizing call must fail with ERROR_INSUFFICIENT_BUFFER; anything
        // else means `needed` is not a size to trust.
        let error = io::Error::last_os_error();
        if sized != 0 || error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(error);
        }
        if (needed as usize) < size_of::<TOKEN_USER>() {
            return Err(invalid_data("the token reported too small a TOKEN_USER"));
        }
        // u64 keeps the buffer 8-byte aligned for TOKEN_USER.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let length = buffer.len() * 8;
        // SAFETY: `buffer` is writable, 8-byte aligned and `length` bytes
        // long, at least the size the sizing call reported, and the token is
        // open with TOKEN_QUERY.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                u32::try_from(length).map_err(|_| invalid_data("TOKEN_USER is too large"))?,
                &mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the buffer is 8-byte aligned, initialized, and at least
        // size_of::<TOKEN_USER>() bytes long (checked above), and
        // GetTokenInformation wrote a TOKEN_USER at its start.
        let user = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read() };
        // SAFETY: the buffer's `length` bytes are initialized u64s, and any
        // initialized memory can be read as bytes.
        let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), length) };
        // The SID is stored in the same buffer, after the TOKEN_USER. It is
        // located by offset and read only through the bounds-checked slice.
        let outside = || invalid_data("the token's SID is outside its buffer");
        let offset = (user.User.Sid as usize)
            .checked_sub(buffer.as_ptr() as usize)
            .ok_or_else(outside)?;
        let sid = bytes
            .get(offset..)
            .and_then(sid_prefix)
            .ok_or_else(outside)?;
        Ok(Self(sid.to_vec()))
    }

    fn bytes(&self) -> &[u8] {
        &self.0
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

    #[test]
    fn the_user_sid_reads_as_windows_writes_it() {
        let user = UserSid::current().unwrap();
        let mut text = ptr::null_mut::<u16>();
        // SAFETY: a valid SID (the bytes are a whole SID) and a valid out
        // pointer; the string is freed with LocalFree when `_text` drops.
        let converted = unsafe {
            windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW(
                user.bytes().as_ptr().cast_mut().cast(),
                &mut text,
            )
        };
        assert_ne!(converted, 0, "{}", io::Error::last_os_error());
        let _text = LocalBox(text.cast());
        let mut length = 0;
        // SAFETY: on success the call returns a NUL-terminated string.
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: the `length` units before the NUL are initialized.
        let expected = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) });
        assert_eq!(sid_string(user.bytes()), Some(expected.unwrap()));
        assert_eq!(sid_string(LOCAL_SYSTEM_SID).unwrap(), "S-1-5-18");
        assert_eq!(sid_string(&[1, 0, 0, 0, 0, 0, 0]), None);
    }

    /// An ACL's bytes: the 8-byte header, then `entries`, each a type, flags
    /// and SID for an entry with a zero access mask.
    fn acl(entries: &[(u32, u8, &[u8])]) -> (Vec<u8>, u32) {
        let mut bytes = vec![2, 0, 0, 0, 0, 0, 0, 0];
        for &(kind, flags, sid) in entries {
            let length = u16::try_from(8 + sid.len()).unwrap();
            bytes.extend([u8::try_from(kind).unwrap(), flags]);
            bytes.extend(length.to_le_bytes());
            bytes.extend([0; 4]);
            bytes.extend(sid);
        }
        (bytes, u32::try_from(entries.len()).unwrap())
    }

    #[test]
    fn acl_entries_are_bounds_checked_and_judged() {
        let user = UserSid::current().unwrap();
        let user = user.bytes();
        let everyone: &[u8] = &[1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        let allow = ACCESS_ALLOWED_ACE_TYPE;
        let deny = ACCESS_DENIED_ACE_TYPE;

        let (bytes, count) = acl(&[(allow, 0, user), (allow, 0, LOCAL_SYSTEM_SID)]);
        assert!(entries_are_owner_only(&bytes, count, user).unwrap());
        let (bytes, count) = acl(&[(deny, 0, everyone), (allow, 0, user)]);
        assert!(entries_are_owner_only(&bytes, count, user).unwrap());
        let (bytes, count) = acl(&[]);
        assert!(entries_are_owner_only(&bytes, count, user).unwrap());

        let (bytes, count) = acl(&[(allow, 0, user), (allow, 0, everyone)]);
        assert!(!entries_are_owner_only(&bytes, count, user).unwrap());
        let inherited = u8::try_from(INHERITED_ACE).unwrap();
        let (bytes, count) = acl(&[(allow, inherited, user)]);
        assert!(!entries_are_owner_only(&bytes, count, user).unwrap());

        // More entries claimed than the bytes hold.
        let (bytes, _) = acl(&[(allow, 0, user)]);
        let error = entries_are_owner_only(&bytes, 2, user).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        // An entry whose size runs past the end, or is too small to move on.
        for size in [u16::MAX, 0] {
            let (mut bytes, count) = acl(&[(allow, 0, user)]);
            bytes[10..12].copy_from_slice(&size.to_le_bytes());
            let error = entries_are_owner_only(&bytes, count, user).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        // A SID that declares more subauthorities than its entry holds.
        let (mut bytes, count) = acl(&[(allow, 0, LOCAL_SYSTEM_SID)]);
        bytes[17] = 2;
        let error = entries_are_owner_only(&bytes, count, user).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}

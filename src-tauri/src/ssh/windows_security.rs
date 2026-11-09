//! The owner and access list of a file or directory, where the
//! `ProgramData` folder is, and reading a file through a mapping: minimal
//! FFI into advapi32, shell32, ole32 and kernel32, as for
//! `GetStartupInfoW` in lib.rs, rather than the full `windows-sys` crate.

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use super::Ace;

#[link(name = "advapi32")]
extern "system" {
    fn GetSecurityInfo(
        handle: *mut c_void,
        object_type: i32,
        security_info: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        security_descriptor: *mut *mut c_void,
    ) -> u32;
    fn GetAce(acl: *mut c_void, index: u32, ace: *mut *mut c_void) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, string_sid: *mut *mut u16) -> i32;
}
#[link(name = "shell32")]
extern "system" {
    fn SHGetKnownFolderPath(
        id: *const Guid,
        flags: u32,
        token: *mut c_void,
        path: *mut *mut u16,
    ) -> i32;
}
#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemFree(mem: *mut c_void);
}
extern "system" {
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn CreateFileMappingW(
        file: *mut c_void,
        attributes: *mut c_void,
        protect: u32,
        maximum_size_high: u32,
        maximum_size_low: u32,
        name: *const u16,
    ) -> *mut c_void;
    fn MapViewOfFile(
        mapping: *mut c_void,
        access: u32,
        offset_high: u32,
        offset_low: u32,
        bytes: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(base: *const c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn ReadProcessMemory(
        process: *mut c_void,
        base: *const c_void,
        buffer: *mut c_void,
        size: usize,
        read: *mut usize,
    ) -> i32;
}

/// Another process holds the file open without sharing it as asked.
pub(super) const ERROR_SHARING_VIOLATION: i32 = 32;
/// Another process has locked the part of the file being read or written.
pub(super) const ERROR_LOCK_VIOLATION: i32 = 33;

const SE_FILE_OBJECT: i32 = 1;
const OWNER_SECURITY_INFORMATION: u32 = 0x1;
const DACL_SECURITY_INFORMATION: u32 = 0x4;

/// The header of an access list (`ACL`).
#[repr(C)]
struct Acl {
    revision: u8,
    sbz1: u8,
    size: u16,
    ace_count: u16,
    sbz2: u16,
}

/// An `ACCESS_ALLOWED_ACE`: the header every entry begins with, then the
/// rights allowed and the SID they are allowed to, from `sid_start` on.
#[repr(C)]
struct AllowedAce {
    ace_type: u8,
    flags: u8,
    size: u16,
    mask: u32,
    sid_start: u32,
}

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// Open the directory at `path` to read its attributes, owner and access
/// list: the link itself if it is one, rather than what it leads to.
pub(super) fn open_directory(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const READ_CONTROL: u32 = 0x2_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Open the file at `path`, following a link as [`std::fs::File::open`]
/// does, to read no more than its attributes, owner and access list.
/// Another process holding it without sharing does not stop this, as none
/// of the access that sharing is about is asked for.
pub(super) fn open_for_security(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const READ_CONTROL: u32 = 0x2_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .open(path)
}

/// The owner of the open file or directory `opened`, as a string SID, and
/// its access list (`None`: it has none, which lets everyone do anything).
pub(super) fn security(opened: &std::fs::File) -> Result<(String, Option<Vec<Ace>>), String> {
    let wanted = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let (mut owner, mut dacl, mut descriptor) = (null_mut(), null_mut(), null_mut());
    // SAFETY: the handle stays open while `opened` is borrowed. Only the
    // owner and the DACL are asked for, so the group and SACL out pointers
    // may be null. On success `owner` and `dacl` point into `descriptor`,
    // which the call allocated and which is freed below.
    let status = unsafe {
        GetSecurityInfo(
            opened.as_raw_handle(),
            SE_FILE_OBJECT,
            wanted,
            &raw mut owner,
            null_mut(),
            &raw mut dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        let code = i32::try_from(status).unwrap_or(i32::MAX);
        return Err(std::io::Error::from_raw_os_error(code).to_string());
    }
    // SAFETY: `owner` and `dacl` are null or point into `descriptor`, which
    // is still allocated.
    let (owner, dacl) = unsafe { (sid_string(owner), aces(dacl)) };
    // SAFETY: `descriptor` was allocated by the call above, and neither it
    // nor what points into it is used after this.
    unsafe { LocalFree(descriptor) };
    let owner = owner.ok_or_else(|| "its owner cannot be read".to_string())?;
    Ok((owner, dacl))
}

/// The entries of the access list at `dacl`, or `None` for a null one.
///
/// # Safety
///
/// `dacl` is null or points to a valid access list.
unsafe fn aces(dacl: *mut c_void) -> Option<Vec<Ace>> {
    // ACE types and flags, from winnt.h.
    const ACCESS_ALLOWED: u8 = 0x0;
    const ACCESS_DENIED: u8 = 0x1;
    const ACCESS_DENIED_OBJECT: u8 = 0x6;
    const ACCESS_ALLOWED_CALLBACK: u8 = 0x9;
    const ACCESS_DENIED_CALLBACK: u8 = 0xA;
    const ACCESS_DENIED_CALLBACK_OBJECT: u8 = 0xC;
    const INHERIT_ONLY: u8 = 0x8;
    if dacl.is_null() {
        return None;
    }
    // SAFETY: `dacl` points to a valid access list, which begins with
    // its header.
    let count = unsafe { (*dacl.cast::<Acl>()).ace_count };
    let entry = |index| {
        let mut ace = null_mut();
        // SAFETY: `index` is below the list's count; on success `ace`
        // points to the entry, inside the list.
        if unsafe { GetAce(dacl, index, &raw mut ace) } == 0 {
            return Ace::Other;
        }
        let ace = ace.cast::<AllowedAce>();
        // SAFETY: every entry begins with its type, flags and size.
        let (ace_type, flags, size) = unsafe { ((*ace).ace_type, (*ace).flags, (*ace).size) };
        // Whether the SID fits in the entry. It starts at byte 8 and takes 8
        // bytes, plus 4 for each sub-authority; their count is its second
        // byte, byte 9 of the entry.
        let sid_fits = size >= 16
            // SAFETY: read only from an entry at least 16 bytes long.
            && usize::from(size) >= 16 + 4 * usize::from(unsafe { *ace.cast::<u8>().add(9) });
        match ace_type {
            _ if flags & INHERIT_ONLY != 0 => Ace::Ignored,
            ACCESS_DENIED
            | ACCESS_DENIED_OBJECT
            | ACCESS_DENIED_CALLBACK
            | ACCESS_DENIED_CALLBACK_OBJECT => Ace::Ignored,
            ACCESS_ALLOWED | ACCESS_ALLOWED_CALLBACK if !sid_fits => Ace::Other,
            ACCESS_ALLOWED | ACCESS_ALLOWED_CALLBACK => {
                // SAFETY: both kinds hold the rights allowed, then the SID,
                // as `AllowedAce` lays them out, and the SID fits.
                let (mask, sid) =
                    unsafe { ((*ace).mask, sid_string((&raw mut (*ace).sid_start).cast())) };
                sid.map_or(Ace::Other, |sid| Ace::Allow { sid, mask })
            }
            _ => Ace::Other,
        }
    };
    Some((0..u32::from(count)).map(entry).collect())
}

/// The SID at `sid` as a string, such as `S-1-5-18`, or `None` when it is
/// null or not a valid SID.
///
/// # Safety
///
/// `sid` is null or points to memory holding a SID.
unsafe fn sid_string(sid: *mut c_void) -> Option<String> {
    if sid.is_null() {
        return None;
    }
    let mut string = null_mut();
    // SAFETY: `sid` points to a SID, which the call checks is valid.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut string) } == 0 {
        return None;
    }
    // SAFETY: on success `string` is a NUL-terminated UTF-16 string that the
    // call allocated; it is copied, then freed.
    unsafe {
        let sid = String::from_utf16_lossy(until_nul(string));
        LocalFree(string.cast());
        Some(sid)
    }
}

/// The UTF-16 string at `string`, up to its NUL.
///
/// # Safety
///
/// `string` points to a NUL-terminated UTF-16 string that outlives the
/// slice.
unsafe fn until_nul<'a>(string: *const u16) -> &'a [u16] {
    let mut len = 0;
    // SAFETY: the string is read up to its NUL, and no further.
    unsafe {
        while *string.add(len) != 0 {
            len += 1;
        }
        std::slice::from_raw_parts(string, len)
    }
}

/// The `ProgramData` folder, as Windows knows it, when it is on a drive of
/// this machine's own. Its path may be filled in from `%SystemDrive%`,
/// which whoever starts the app could point at a share that reports any
/// owner.
pub(super) fn program_data() -> Option<PathBuf> {
    // FOLDERID_ProgramData, {62AB5D82-FDC1-4DC3-A9DD-070D1D495D97}.
    const PROGRAM_DATA: Guid = Guid {
        data1: 0x62AB_5D82,
        data2: 0xFDC1,
        data3: 0x4DC3,
        data4: [0xA9, 0xDD, 0x07, 0x0D, 0x1D, 0x49, 0x5D, 0x97],
    };
    let mut path = null_mut();
    // SAFETY: the folder's ID and the out pointer are valid for the call;
    // no flags, and no token, for the current user.
    let result = unsafe { SHGetKnownFolderPath(&PROGRAM_DATA, 0, null_mut(), &raw mut path) };
    let found = (result >= 0 && !path.is_null()).then(|| {
        // SAFETY: on success `path` is a NUL-terminated UTF-16 string that
        // the call allocated, copied here before it is freed below.
        PathBuf::from(OsString::from_wide(unsafe { until_nul(path) }))
    });
    // SAFETY: the path is freed whether or not the call succeeded, as
    // documented, and not used after this; a null one is ignored.
    unsafe { CoTaskMemFree(path.cast()) };
    found.filter(|found| on_fixed_drive(found))
}

/// The first `len` bytes of the open file `file`, read through a read-only
/// mapping of it rather than with `ReadFile`. A byte-range lock that
/// another process holds stops `ReadFile` but not a mapping, whose pages
/// are read with paging I/O, which such locks do not apply to.
///
/// The view is copied with `ReadProcessMemory`, never read directly. Should
/// a page fail to be read, say on an I/O error of the drive, a direct read
/// would raise an exception that ends the process; `ReadProcessMemory`
/// returns an error instead. It also means that no Rust reference to the
/// view is made, as none should be to memory other processes can map.
///
/// `len` may not be more than the file holds when it is mapped, nor 4 GiB
/// or more: either is an error. The caller bounds it, as the copy is
/// allocated whole.
pub(super) fn read_mapped(file: &std::fs::File, len: u64) -> std::io::Result<Vec<u8>> {
    const PAGE_READONLY: u32 = 0x02;
    const FILE_MAP_READ: u32 = 0x04;
    if len == 0 {
        // A mapping of nothing is an error.
        return Ok(Vec::new());
    }
    let too_large = || std::io::Error::other("too large to map");
    let size_low = u32::try_from(len).map_err(|_| too_large())?;
    let size = usize::try_from(len).map_err(|_| too_large())?;
    // SAFETY: the handle stays open while `file` is borrowed; no security
    // attributes and no name. The mapping is `len` bytes, which fails
    // rather than map past the end of a file that holds less.
    let mapping = unsafe {
        CreateFileMappingW(
            file.as_raw_handle(),
            null_mut(),
            PAGE_READONLY,
            0,
            size_low,
            null(),
        )
    };
    if mapping.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `mapping` is a valid mapping of `size` bytes, mapped whole.
    let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, size) };
    let read = if view.is_null() {
        Err(std::io::Error::last_os_error())
    } else {
        let mut copy = vec![0_u8; size];
        let mut copied = 0;
        // SAFETY: the view's `size` bytes are copied into `copy`, which
        // holds as many, by the kernel on this process's own memory: a page
        // that cannot be read fails the call rather than raise an
        // exception. The pseudo handle of the current process needs no
        // closing.
        let ok = unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                view,
                copy.as_mut_ptr().cast(),
                size,
                &raw mut copied,
            )
        };
        let read = if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else if copied == size {
            Ok(copy)
        } else {
            Err(std::io::Error::other("only part of the mapping was read"))
        };
        // SAFETY: `view` was mapped above and is not used after this. A
        // failure would only leak the view, and nothing could be done
        // about it, so it is not checked.
        unsafe { UnmapViewOfFile(view) };
        read
    };
    // SAFETY: `mapping` was created above and is not used after this. As
    // for the view, a failure is not checked.
    unsafe { CloseHandle(mapping) };
    read
}

/// Whether `path` is on a fixed drive of this machine: not a share, nor a
/// drive letter mapped to one.
pub(super) fn on_fixed_drive(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    const DRIVE_FIXED: u32 = 3;
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = prefix.kind() else {
        return false;
    };
    let root = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: `root` is a NUL-terminated UTF-16 string that outlives the
    // call.
    unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_FIXED }
}

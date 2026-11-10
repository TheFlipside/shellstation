//! Creating a file or directory with an access list of its own on Windows:
//! full control for the user the app runs as, and nothing inherited from the
//! directory above. The list is handed to the create call, so what is created
//! is never open to others, not even for a moment, as with the mode on Unix.
//!
//! The standard library has no way to pass a security descriptor to a create,
//! so these call `CreateFileW` and `CreateDirectoryW` themselves: minimal FFI
//! into advapi32 and kernel32, as in `ssh/windows_security.rs`, rather than
//! the full `windows-sys` crate.
//!
//! A file system without access lists (FAT, exFAT) ignores the descriptor,
//! and the create goes ahead as a plain one.
//!
//! On a network share the creates are the plain ones, and what is created
//! gets its access list from the directory above, as the share has it: see
//! [`on_share`].

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Component, Path, Prefix};
use std::ptr::null_mut;

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(
        token: *mut c_void,
        class: i32,
        information: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, string_sid: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share_mode: u32,
        attributes: *const SecurityAttributes,
        disposition: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn CreateDirectoryW(name: *const u16, attributes: *const SecurityAttributes) -> i32;
}

/// `SECURITY_ATTRIBUTES`: the security descriptor a create gives what it
/// creates, and whether child processes inherit the handle.
#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit_handle: i32,
}

/// A security descriptor whose access list allows the current user
/// everything and no one else anything, and takes nothing from the directory
/// above.
struct Descriptor(*mut c_void);

impl Descriptor {
    /// For a file, or for a directory, whose list also goes to whatever is
    /// later created inside it.
    fn for_user_alone(directory: bool) -> io::Result<Self> {
        const SDDL_REVISION_1: u32 = 1;
        let sddl: Vec<u16> = user_alone_sddl(directory)?
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = null_mut();
        // SAFETY: `sddl` is a NUL-terminated UTF-16 string that outlives the
        // call, and the size out pointer may be null. On success `descriptor`
        // is allocated by the call and freed when `self` is dropped.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    /// The descriptor as a create call takes it. The handle is not passed on
    /// to child processes, as with the standard library's own creates.
    fn attributes(&self) -> SecurityAttributes {
        SecurityAttributes {
            length: size_of::<SecurityAttributes>() as u32,
            descriptor: self.0,
            inherit_handle: 0,
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by the call that made it and
        // is not used after this.
        unsafe { LocalFree(self.0) };
    }
}

/// The access list for the current user alone, in SDDL: protected (`P`), so
/// that nothing is inherited from the directory above, with the one entry
/// allowing the user full access (`FA`). A directory's entry is inherited by
/// the files (`OI`) and directories (`CI`) created inside it.
///
/// The user is named by SID, read from the process itself. The owner is left
/// to the system: it is the user, or for an elevated process the
/// administrators, who can take ownership of a file anyway.
fn user_alone_sddl(directory: bool) -> io::Result<String> {
    let sid = current_user_sid()?;
    let inherited = if directory { "OICI" } else { "" };
    Ok(format!("D:P(A;{inherited};FA;;;{sid})"))
}

/// The SID of the user the process runs as, such as `S-1-5-21-…-1001`, from
/// the process's own token.
fn current_user_sid() -> io::Result<String> {
    const TOKEN_QUERY: u32 = 0x8;
    /// `TokenUser`, of `TOKEN_INFORMATION_CLASS`.
    const TOKEN_USER: i32 = 1;
    let mut token = null_mut();
    // SAFETY: the pseudo handle of the current process needs no closing; on
    // success `token` is a handle, closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // A `TOKEN_USER`: a pointer to the SID and its attributes, and then the
    // SID itself, which takes 68 bytes at most. Eight-byte words, so that
    // the pointer is aligned.
    let mut user = [0_u64; 16];
    let mut needed = 0;
    // SAFETY: `token` is open for querying, and `user` is writable for the
    // length given.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TOKEN_USER,
            user.as_mut_ptr().cast(),
            size_of_val(&user) as u32,
            &raw mut needed,
        )
    };
    let sid = if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: on success `user` begins with a pointer to the SID, which
        // lies further on in `user` and is read before `user` goes.
        unsafe { sid_string(user.as_ptr().cast::<*mut c_void>().read()) }
    };
    // SAFETY: `token` was opened above and is not used after this.
    unsafe { CloseHandle(token) };
    sid
}

/// The SID at `sid` as a string.
///
/// # Safety
///
/// `sid` points to memory holding a SID.
unsafe fn sid_string(sid: *mut c_void) -> io::Result<String> {
    let mut string = null_mut();
    // SAFETY: `sid` points to a SID, which the call checks is valid.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut string) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `string` is a NUL-terminated UTF-16 string that the
    // call allocated; it is copied, then freed.
    unsafe {
        let copy = copy_until_nul(string);
        LocalFree(string.cast());
        Ok(copy)
    }
}

/// A copy of the UTF-16 string at `string`, up to its NUL. A copy, so that
/// nothing borrowed from the string is left once it is freed.
///
/// # Safety
///
/// `string` points to a NUL-terminated UTF-16 string.
unsafe fn copy_until_nul(string: *const u16) -> String {
    let mut len = 0;
    // SAFETY: the string is read up to its NUL, and no further.
    unsafe {
        while *string.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(string, len))
    }
}

/// Whether what is created at `path` would be on a network share, going by
/// the directory it is created in: one given by a share's name, on a drive
/// letter mapped to a share, or reached through a link that leads to one.
///
/// A share gets no access list from here. The list names the user by SID,
/// which a server knows only when the account is one of a domain that both
/// machines belong to. A server reached with another account, such as one of
/// its own with the same name, would hold the list against the user: a log
/// could not be opened once written, and a log directory could not be
/// written into. Who may read a share is its server's to say.
///
/// The directory is resolved by the system, which names a share however it
/// was reached. One that cannot be resolved counts as local, so that nothing
/// is left open for not knowing; if it is missing, the create fails anyway.
pub(super) fn on_share(path: &Path) -> bool {
    std::path::absolute(path)
        .ok()
        .and_then(|absolute| std::fs::canonicalize(absolute.parent()?).ok())
        .is_some_and(|directory| names_share(&directory))
}

/// Whether the resolved path `directory` names a share: `\\?\UNC\server\…`,
/// as the system resolves one, or `\\server\…`.
fn names_share(directory: &Path) -> bool {
    matches!(
        directory.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::VerbatimUNC(..) | Prefix::UNC(..))
    )
}

/// `path` as the create calls take it: absolute and NUL-terminated, and in
/// the `\\?\` form when it is too long for them otherwise, as the standard
/// library passes a path.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    /// From here on a create refuses a path that is not in the `\\?\` form:
    /// 248 for a directory, which leaves room for a file name in the 260
    /// that are the limit for a file.
    const LEGACY_MAX_PATH: usize = 248;
    if path.as_os_str().encode_wide().any(|unit| unit == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path holds a NUL",
        ));
    }
    let absolute = std::path::absolute(path)?;
    let units: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    let prefix = match absolute.components().next() {
        Some(Component::Prefix(prefix)) if units.len() >= LEGACY_MAX_PATH => Some(prefix.kind()),
        _ => None,
    };
    // `C:\…` becomes `\\?\C:\…`, and `\\server\share\…` becomes
    // `\\?\UNC\server\share\…`. Anything else is already in such a form, or
    // is a device, and stays as it is.
    let (verbatim, rest) = match prefix {
        Some(Prefix::Disk(_)) => (r"\\?\", &units[..]),
        Some(Prefix::UNC(..)) => (r"\\?\UNC\", &units[2..]),
        _ => ("", &units[..]),
    };
    Ok(verbatim
        .encode_utf16()
        .chain(rest.iter().copied())
        .chain(Some(0))
        .collect())
}

/// Create a new, empty file for the current user alone and open it for
/// writing, or for appending only. Otherwise as the standard library's
/// `create_new`: it fails when anything holds the name, and does not follow
/// a link there. Not for a file on a share: see [`on_share`].
pub(super) fn open_new(path: &Path, append: bool) -> io::Result<File> {
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
    const FILE_WRITE_DATA: u32 = 0x2;
    /// Others may read, write and delete the file while it is open.
    const SHARE_ALL: u32 = 0x7;
    const CREATE_NEW: u32 = 1;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    // The right to write without the right to write anywhere but at the
    // end, which is what makes every write an append.
    let access = if append {
        FILE_GENERIC_WRITE & !FILE_WRITE_DATA
    } else {
        GENERIC_WRITE
    };
    let name = wide(path)?;
    let descriptor = Descriptor::for_user_alone(false)?;
    let attributes = descriptor.attributes();
    // SAFETY: `name` is a NUL-terminated UTF-16 string, and `attributes`
    // points to a descriptor that `descriptor` keeps allocated; both outlive
    // the call. No template file.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            SHARE_ALL,
            &raw const attributes,
            CREATE_NEW,
            FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    // `INVALID_HANDLE_VALUE`.
    if handle == std::ptr::without_provenance_mut(usize::MAX) {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is an open file handle that nothing else owns.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Create the directory at `path`, and any missing parents, for the current
/// user alone; one on a share is created the plain way (see [`on_share`]).
/// An existing directory is fine and left as it is, as with the standard
/// library's `create_dir_all`, whose steps these are.
pub(super) fn create_dir_all(path: &Path) -> io::Result<()> {
    if path == Path::new("") {
        return Ok(());
    }
    match create_dir(path) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) if path.is_dir() => return Ok(()),
        Err(e) => return Err(e),
    }
    match path.parent() {
        Some(parent) => create_dir_all(parent)?,
        None => return Err(io::Error::other("failed to create whole tree")),
    }
    match create_dir(path) {
        Ok(()) => Ok(()),
        Err(_) if path.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Create the one directory at `path`, whose parent is there: for the
/// current user alone, or the plain way on a share, where no descriptor is
/// made, so that none can fail there.
fn create_dir(path: &Path) -> io::Result<()> {
    if on_share(path) {
        return std::fs::create_dir(path);
    }
    let name = wide(path)?;
    let descriptor = Descriptor::for_user_alone(true)?;
    let attributes = descriptor.attributes();
    // SAFETY: `name` is a NUL-terminated UTF-16 string, and `attributes`
    // points to a descriptor that `descriptor` keeps allocated; both outlive
    // the call.
    if unsafe { CreateDirectoryW(name.as_ptr(), &raw const attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The access list of the file or directory at `path`, in SDDL.
#[cfg(test)]
fn access_list(path: &Path) -> String {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

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
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor: *mut c_void,
            revision: u32,
            security_info: u32,
            string: *mut *mut u16,
            len: *mut u32,
        ) -> i32;
    }
    const SE_FILE_OBJECT: i32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 0x4;
    const READ_CONTROL: u32 = 0x2_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    // Backup semantics, so that a directory can be opened as well.
    let opened = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap();
    let mut descriptor = null_mut();
    // SAFETY: the handle is open; only the access list is asked for, so the
    // other out pointers may be null. `descriptor` is freed below.
    let status = unsafe {
        GetSecurityInfo(
            opened.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &raw mut descriptor,
        )
    };
    assert_eq!(status, 0, "{}", path.display());
    let mut string = null_mut();
    // SAFETY: `descriptor` is the valid descriptor just read; on success
    // `string` is a NUL-terminated UTF-16 string, copied and then freed
    // along with the descriptor.
    unsafe {
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            1,
            DACL_SECURITY_INFORMATION,
            &raw mut string,
            null_mut(),
        );
        assert_ne!(ok, 0, "{}", path.display());
        let sddl = copy_until_nul(string);
        LocalFree(string.cast());
        LocalFree(descriptor);
        sddl
    }
}

/// Assert that the access list of the file or directory at `path` is one
/// these creates give: protected from inheriting, with full access for the
/// current user as its only entry. One that was inherited fails this too,
/// from a directory created here included.
#[cfg(test)]
pub(crate) fn assert_for_user_alone(path: &Path) {
    let directory = std::fs::symlink_metadata(path).unwrap().is_dir();
    assert_eq!(
        access_list(path),
        user_alone_sddl(directory).unwrap(),
        "{}",
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ss-private-win-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn the_current_user_sid_is_a_sid() {
        // S-1-5-21-… for local and domain accounts, S-1-12-1-… for Entra ID.
        let sid = current_user_sid().unwrap();
        let parts = sid.strip_prefix("S-1-").expect(&sid);
        assert!(
            parts
                .split('-')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())),
            "{sid}"
        );
    }

    #[test]
    fn what_a_private_directory_holds_is_for_the_user_alone_too() {
        let root = scratch();
        create_dir_all(&root).unwrap();
        // Created by a plain create, as another program would: the list
        // comes from the directory, and is the user's alone as well.
        let plain = root.join("plain.txt");
        std::fs::write(&plain, b"x").unwrap();
        let list = access_list(&plain);
        let sid = current_user_sid().unwrap();
        assert_eq!(list.matches('(').count(), 1, "{list}");
        assert!(list.ends_with(&format!("(A;ID;FA;;;{sid})")), "{list}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_path_too_long_for_a_plain_create_still_works() {
        let root = scratch();
        let mut deep = root.clone();
        while deep.as_os_str().len() < 300 {
            deep.push("a-directory-with-a-rather-long-name");
        }
        create_dir_all(&deep).unwrap();
        assert_for_user_alone(&deep);
        let file = deep.join("session.log");
        drop(open_new(&file, true).unwrap());
        assert_for_user_alone(&file);
        let again = open_new(&file, true).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_share_is_told_by_its_resolved_name() {
        for share in [
            r"\\?\UNC\server\share",
            r"\\?\UNC\server\share\logs",
            r"\\?\UNC\wsl.localhost\Ubuntu\home",
            r"\\server\share\logs",
        ] {
            assert!(names_share(Path::new(share)), "{share}");
        }
        for local in [
            r"\\?\C:\",
            r"\\?\C:\logs",
            r"C:\logs",
            "logs",
            r"\\.\COM1",
            "",
        ] {
            assert!(!names_share(Path::new(local)), "{local}");
        }
        // The temporary directory is local. So is a directory in it that is
        // not there, in which a create fails anyway.
        let root = scratch();
        assert!(!on_share(&root));
        assert!(!on_share(&root.join("missing").join("session.log")));
        assert!(!on_share(Path::new("")));
    }

    #[test]
    fn a_long_path_is_passed_in_the_verbatim_form() {
        let text = |path: &str| {
            let units = wide(Path::new(path)).unwrap();
            assert_eq!(units.last(), Some(&0));
            String::from_utf16_lossy(&units[..units.len() - 1])
        };
        let long = "x".repeat(250);
        assert_eq!(text(r"C:\logs\a.log"), r"C:\logs\a.log");
        assert_eq!(text(&format!(r"C:\{long}")), format!(r"\\?\C:\{long}"));
        assert_eq!(
            text(&format!(r"\\server\share\{long}")),
            format!(r"\\?\UNC\server\share\{long}")
        );
        assert_eq!(text(&format!(r"\\?\C:\{long}")), format!(r"\\?\C:\{long}"));
        // The last length a create takes as it is, and the first it does not.
        let (fits, too_long) = ("x".repeat(244), "x".repeat(245));
        assert_eq!(text(&format!(r"C:\{fits}")), format!(r"C:\{fits}"));
        assert_eq!(
            text(&format!(r"C:\{too_long}")),
            format!(r"\\?\C:\{too_long}")
        );
        // Made absolute as a create would, before anything is put in front.
        assert_eq!(text(r"C:/logs/../a.log"), r"C:\a.log");
        assert_eq!(
            text(&format!(r"C:/logs/../{long}")),
            format!(r"\\?\C:\{long}")
        );
        assert_eq!(text(r"\\.\COM1"), r"\\.\COM1");
        assert!(wide(Path::new("a\0b")).is_err());
    }

    #[test]
    fn a_file_in_the_way_of_a_directory_is_an_error() {
        let root = scratch();
        create_dir_all(&root).unwrap();
        let file = root.join("taken");
        std::fs::write(&file, b"x").unwrap();
        assert!(create_dir_all(&file).is_err());
        assert!(create_dir_all(&file.join("below")).is_err());
        assert!(file.is_file());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

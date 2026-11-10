//! Creating files and directories that only their owner can open.
//!
//! The restriction is part of the create call, so a new file or directory is
//! never open to others, not even between creating it and setting its
//! permissions. On Unix it is the mode. On Windows it is an access list of
//! the file's or directory's own, which allows the current user everything
//! and takes nothing from the directory above: see [`windows`]. A network
//! share is the exception there: what is created on one gets its access list
//! from the directory above. An existing directory is restricted on Unix
//! only. One that may sit where others can write is restricted there
//! without following a link at its name.

use std::io;
use std::path::Path;

#[cfg(windows)]
mod windows;

/// Create a directory, and any missing parents, as 0700 on Unix; the umask
/// can only take bits away. On Windows each directory created gets an access
/// list for the current user alone, which what is later created inside it
/// inherits; one created on a network share gets the list of the directory
/// above instead. An existing directory is left as it is, so a caller that
/// needs it owner-only still restricts it.
pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::create_dir_all(path)
    }
    #[cfg(not(windows))]
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)
    }
}

/// Restrict an existing directory to 0700 on Unix; nothing on Windows, where
/// a directory that is already there keeps its access list. For a directory
/// that is the app's own and sits where only the user can put anything: the
/// config directory. A link at the path is followed, on purpose, so that a
/// config directory the user keeps elsewhere and links to is restricted
/// there; for a
/// directory that may sit where others can write, there is
/// [`restrict_dir_to_owner_nofollow`]. The mode is replaced whole. Separate
/// from creating the directory, so that a file system without Unix modes can
/// fail this and still be used.
// It can only fail on Unix, where it changes the mode.
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
pub(crate) fn restrict_dir_to_owner(
    #[cfg_attr(not(unix), allow(unused_variables))] path: &Path,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// [`restrict_dir_to_owner`] for a directory that may sit where others can
/// write, such as a log directory the user chose. The caller takes the
/// directory to be for the app: the mode is replaced whole, so others who
/// shared it lose it.
///
/// The directory is opened first, by a call that refuses a link at its name,
/// and restricted through that handle. Someone able to put a link there, or
/// to swap one in after the directory was created, therefore cannot have a
/// directory elsewhere restricted: the link is an error, and so is anything
/// but a directory. Links in the path above the name are followed as ever.
/// A directory that belongs to another user is an error as well, also for
/// root, who could change its mode. A directory the user cannot read cannot
/// be opened, and is an error too; restricting it by its path instead would
/// follow a link swapped in there.
// It can only fail on Unix, where it changes the mode.
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
pub(crate) fn restrict_dir_to_owner_nofollow(
    #[cfg_attr(not(unix), allow(unused_variables))] path: &Path,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        // Without a separator at its end, as a path from the config file
        // can have: with one, the system resolves a link at the name before
        // it looks at the flags, and follows it after all.
        let path = &path.components().collect::<std::path::PathBuf>();
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| match std::fs::symlink_metadata(path) {
                // What the system says of a link differs between systems.
                Ok(at_name) if at_name.file_type().is_symlink() => io::Error::new(
                    e.kind(),
                    "a symbolic link is at its name, which is not followed",
                ),
                _ => e,
            })?;
        // SAFETY: `geteuid` takes no arguments, touches no memory and
        // cannot fail.
        let user = unsafe { libc::geteuid() };
        if dir.metadata()?.uid() != user {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the directory belongs to another user",
            ));
        }
        dir.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Replace `path` with `contents` atomically: they go into a new temporary
/// file next to it, created as [`create_private_file`] creates one, which is
/// then renamed over it. A failed or interrupted write leaves the old file
/// whole, the new one is never open to others, and a symlink at `path` is
/// replaced rather than written through.
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".shellstation-{}.tmp", uuid::Uuid::new_v4()));
    let written = create_private_file(&tmp)
        .and_then(|mut file| file.write_all(contents).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        // Best effort: the error that matters is the one returned.
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Create a new, empty file as 0600 on Unix, and on Windows with an access
/// list for the current user alone, unless it is on a network share. Fails with
/// [`io::ErrorKind::AlreadyExists`] when the name is taken, so an existing
/// file is never truncated or followed through a symlink.
pub(crate) fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    create_new_private(path, false)
}

/// [`create_private_file`] for a file that is written to for as long as it
/// is open, such as a log: every write goes to the end of the file, wherever
/// that is by then, so one that something else has truncated in the meantime
/// carries on from its new end instead of leaving a gap. Appending is all the
/// handle is good for: on Windows it carries the right to append only, so it
/// cannot truncate the file or write anywhere but at its end.
pub(crate) fn create_private_append_file(path: &Path) -> io::Result<std::fs::File> {
    create_new_private(path, true)
}

/// `create_new` is what refuses a name already taken, whatever holds it: a
/// file, a directory, or a symlink, dangling or not, which is not followed
/// on either platform.
fn create_new_private(path: &Path, append: bool) -> io::Result<std::fs::File> {
    let created = open_new_private(path, append);
    // Windows refuses a name that a directory holds with "access denied"
    // rather than "already exists". Looking at the name, without following
    // a link, tells that from a directory that cannot be written to: there,
    // a free name has nothing to look at and stays "access denied". Whatever
    // holds the name counts, not only a directory, since a caller that moves
    // on to another name is right to do so for any of them.
    #[cfg(windows)]
    if let Err(e) = &created {
        if e.kind() == io::ErrorKind::PermissionDenied && std::fs::symlink_metadata(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the name is already taken",
            ));
        }
    }
    created
}

fn open_new_private(path: &Path, append: bool) -> io::Result<std::fs::File> {
    // On Windows the create is the module's own, with the access list. A
    // file on a network share gets the plain create below instead.
    #[cfg(windows)]
    if !windows::on_share(path) {
        return windows::open_new(path, append);
    }
    let mut options = std::fs::OpenOptions::new();
    if append {
        options.append(true);
    } else {
        options.write(true);
    }
    options.create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Whether what these creates make at `path` is left with the access list of
/// the directory above it, because that directory is on a network share, on
/// Windows. For a caller to say so; never the case elsewhere.
pub(crate) fn on_network_share(
    #[cfg_attr(not(windows), allow(unused_variables))] path: &Path,
) -> bool {
    #[cfg(windows)]
    {
        windows::on_share(path)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Assert that the file or directory at `path` is its owner's alone: no
/// group or other bits on Unix, and on Windows the access list a create
/// here gives. On Windows the tests therefore need a temporary directory
/// that is not on a network share.
#[cfg(test)]
pub(crate) fn assert_private(
    #[cfg_attr(not(any(unix, windows)), allow(unused_variables))] path: &Path,
) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "{}: group/other bits set: {mode:o}",
            path.display()
        );
    }
    #[cfg(windows)]
    windows::assert_for_user_alone(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ss-private-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn create_private_dir_creates_missing_parents() {
        let root = scratch();
        let nested = root.join("a").join("b");
        create_private_dir(&nested).unwrap();
        assert!(nested.is_dir());
        // Like `create_dir_all`, an existing directory is fine.
        create_private_dir(&nested).unwrap();
        // Every directory it had to create, not only the last.
        for dir in [root.clone(), root.join("a"), nested] {
            assert_private(&dir);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restrict_dir_to_owner_restricts_an_existing_directory() {
        let root = scratch();
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        restrict_dir_to_owner(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&root).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "group/other bits set: {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restrict_dir_to_owner_nofollow_restricts_an_existing_directory() {
        let root = scratch();
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // With the bits a shared directory has, which go as well.
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o2775)).unwrap();
        }
        // A separator at the end of the path makes no difference.
        let with_separator = root.join("");
        assert!(with_separator
            .to_string_lossy()
            .ends_with(std::path::MAIN_SEPARATOR));
        restrict_dir_to_owner_nofollow(&with_separator).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&root).unwrap().permissions().mode();
            assert_eq!(mode & 0o7777, 0o700, "{mode:o}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A link at the directory's name is refused, whether it leads to a
    /// directory or a file, and what it leads to keeps its mode; a file at
    /// the name is refused as well. `restrict_dir_to_owner` follows the link.
    #[cfg(unix)]
    #[test]
    fn restrict_dir_to_owner_nofollow_refuses_a_link_and_a_file() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        let root = scratch();
        create_private_dir(&root).unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = root.join("file");
        std::fs::write(&file, b"").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let to_dir = root.join("to-dir");
        std::os::unix::fs::symlink(&elsewhere, &to_dir).unwrap();
        let to_file = root.join("to-file");
        std::os::unix::fs::symlink(&file, &to_file).unwrap();
        let dangling = root.join("dangling");
        std::os::unix::fs::symlink(root.join("missing"), &dangling).unwrap();
        // With a separator at its end too, which would have the system
        // follow the link whatever the flags say.
        let to_dir_slash = std::path::PathBuf::from(format!("{}/", to_dir.display()));
        for link in [&to_dir, &to_dir_slash, &to_file, &dangling] {
            let refused = restrict_dir_to_owner_nofollow(link).unwrap_err();
            assert!(refused.to_string().contains("symbolic link"), "{refused}");
        }
        assert!(restrict_dir_to_owner_nofollow(&file).is_err());
        assert_eq!(mode(&elsewhere), 0o755);
        assert_eq!(mode(&file), 0o644);

        // The other one goes through the link.
        restrict_dir_to_owner(&to_dir).unwrap();
        assert_eq!(mode(&elsewhere), 0o700);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_private_file_replaces_and_restricts() {
        let root = scratch();
        create_private_dir(&root).unwrap();
        let file = root.join("config.json");
        std::fs::write(&file, b"a much longer old value").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_private_file(&file, b"new").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        // The temporary file was renamed into place, not left beside it.
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert_private(&file);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_private_file_refuses_an_existing_file() {
        let root = scratch();
        create_private_dir(&root).unwrap();
        let file = root.join("known_hosts");
        create_private_file(&file).unwrap();
        let again = create_private_file(&file).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
        assert_private(&file);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_private_append_file_writes_at_the_end_and_refuses_a_taken_name() {
        use std::io::Write;
        let root = scratch();
        create_private_dir(&root).unwrap();
        let file = root.join("session.log");
        let mut log = create_private_append_file(&file).unwrap();
        log.write_all(b"first\n").unwrap();
        // Something else empties the file; the next write starts at its new
        // end rather than at the old offset.
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&file)
            .unwrap();
        log.write_all(b"second\n").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"second\n");
        // On Windows the handle itself cannot cut the file short.
        #[cfg(windows)]
        assert!(log.set_len(0).is_err());
        let again = create_private_append_file(&file).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
        // A directory holds a name as a file does.
        let taken = root.join("taken.log");
        std::fs::create_dir(&taken).unwrap();
        let by_directory = create_private_append_file(&taken).unwrap_err();
        assert_eq!(by_directory.kind(), io::ErrorKind::AlreadyExists);
        assert_private(&file);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_symlink_holds_a_name_and_is_not_followed() {
        let root = scratch();
        create_private_dir(&root).unwrap();
        let target = root.join("target");
        std::fs::write(&target, b"untouched").unwrap();
        let missing = root.join("missing");
        let links = [
            (root.join("to-file"), &target),
            (root.join("dangling"), &missing),
        ];
        for (link, to) in &links {
            #[cfg(unix)]
            std::os::unix::fs::symlink(to, link).unwrap();
            #[cfg(windows)]
            if std::os::windows::fs::symlink_file(to, link).is_err() {
                // Creating a symlink takes Developer Mode or an elevated process.
                eprintln!("skipped: this process may not create symbolic links");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        }
        for (link, _) in &links {
            for create in [create_private_file, create_private_append_file] {
                let refused = create(link).unwrap_err();
                assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
            }
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        assert!(!missing.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}

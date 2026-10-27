//! Creating files and directories that only their owner can open.
//!
//! On Unix the mode is part of the create call, so a new file or directory
//! is never open to others, not even between creating it and setting its
//! permissions. On Windows these are plain creates that inherit the parent's
//! ACL, which under the user's profile also lets SYSTEM and administrators
//! in; a caller that needs owner-only access there restricts it afterwards.

use std::io;
use std::path::Path;

/// Create a directory, and any missing parents, as 0700 on Unix; the umask
/// can only take bits away. An existing directory is left as it is, so a
/// caller that needs it owner-only still restricts it.
pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Restrict an existing directory to 0700 on Unix, for one that belongs to
/// the app alone; nothing on Windows. Separate from creating it, so that a
/// file system without Unix modes can fail this and still be used.
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

/// Replace `path` with `contents` atomically: they go into a new 0600 (on
/// Unix) temporary file next to it, which is then renamed over it. A failed
/// or interrupted write leaves the old file whole, the new one is never open
/// to others, and a symlink at `path` is replaced rather than written
/// through.
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

/// Create a new, empty file as 0600 on Unix. Fails with
/// [`io::ErrorKind::AlreadyExists`] when there is one already, so an existing
/// file is never truncated or followed through a symlink.
pub(crate) fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
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
        #[cfg(unix)]
        for dir in [root.join("a"), nested] {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "{}: group/other bits set: {mode:o}",
                dir.display()
            );
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "group/other bits set: {mode:o}");
        }
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "group/other bits set: {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

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

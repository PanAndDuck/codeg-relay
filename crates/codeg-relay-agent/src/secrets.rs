use anyhow::{ensure, Context};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

pub fn private_dir(path: &Path) -> anyhow::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "private directory must not be a symlink"
        );
    } else {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(path)
            .context("cannot create private directory")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn write_private(path: &Path, data: &[u8], replace: bool) -> anyhow::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "private file must be a regular file"
        );
    }
    let parent = path
        .parent()
        .context("private file needs a parent directory")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(data)?;
    temp.as_file().sync_all()?;
    if replace {
        temp.persist(path)?;
    } else {
        temp.persist_noclobber(path)?;
    }
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn open_private(path: &Path, create: bool) -> anyhow::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(create).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).context("cannot open private file")?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "private file must be a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            metadata.permissions().mode() & 0o777 == 0o600,
            "private file permissions must be 0600"
        );
        // This process only reads credentials owned by its effective user.
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "private file must be owned by the current user"
        );
    }
    Ok(file)
}

pub fn read_private(path: &Path) -> anyhow::Result<String> {
    let file = open_private(path, false)?;
    ensure!(
        file.metadata()?.len() <= 64 * 1024,
        "private file exceeds 64 KiB"
    );
    let mut value = String::new();
    file.take(64 * 1024 + 1)
        .read_to_string(&mut value)
        .context("cannot read private file")?;
    ensure!(value.len() <= 64 * 1024, "private file exceeds 64 KiB");
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_files_are_atomic_and_never_overwritten_without_permission() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("private");
        private_dir(&dir).unwrap();
        let path = dir.join("token");
        write_private(&path, b"example-one", false).unwrap();
        assert!(read_private(&path).is_ok());
        assert!(write_private(&path, b"example-two", false).is_err());
        write_private(&path, b"example-two", true).unwrap();
        assert!(read_private(&path).unwrap().ends_with("two"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read_private(&path).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn symlink_secrets_and_directories_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("value");
        write_private(&target, b"example-value", false).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_private(&link).is_err());
        assert!(write_private(&link, b"replacement", true).is_err());
        let linkdir = dir.path().join("dir");
        std::os::unix::fs::symlink(outside.path(), &linkdir).unwrap();
        assert!(private_dir(&linkdir).is_err());
    }
}

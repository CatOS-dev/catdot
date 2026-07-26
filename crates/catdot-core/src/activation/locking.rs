use crate::{Error, Result, error::io};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};
pub fn lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        io(parent, fs::create_dir_all(parent))?;
        io(
            parent,
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)),
        )?;
    }
    let file = io(
        path,
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path),
    )?;
    io(
        path,
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)),
    )?;
    file.lock_exclusive().map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(file)
}

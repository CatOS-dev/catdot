use crate::{Error, Result, error::io};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};
pub fn lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        io(parent, fs::create_dir_all(parent))?
    }
    let file = io(
        path,
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path),
    )?;
    file.lock_exclusive().map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(file)
}

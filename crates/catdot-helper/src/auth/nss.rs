use anyhow::{Context, Result, bail};
use std::{
    ffi::CStr,
    fs,
    io::Read,
    os::unix::fs::MetadataExt,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub fn user_home(uid: u32) -> Result<PathBuf> {
    let mut size = 16 * 1024;
    loop {
        let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0_u8; size];
        let code = unsafe {
            libc::getpwuid_r(
                uid,
                passwd.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code == libc::ERANGE {
            size *= 2;
            continue;
        }
        if code != 0 || result.is_null() {
            bail!("uid {uid} does not exist in NSS")
        }
        let passwd = unsafe { passwd.assume_init() };
        let home = unsafe { CStr::from_ptr(passwd.pw_dir) }
            .to_str()?
            .to_owned();
        return Ok(PathBuf::from(home));
    }
}

pub fn read_trusted_user_state(uid: u32, path: &Path) -> Result<catdot_core::UserState> {
    if !path.is_absolute() {
        bail!("state path must be absolute")
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open state file {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect state file {}", path.display()))?;
    if !metadata.is_file() || metadata.uid() != uid {
        bail!("state path must be a regular file owned by uid {uid}")
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("read state file {}", path.display()))?;
    catdot_core::parse_state_text(&text)
        .with_context(|| format!("parse state file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::read_trusted_user_state;
    use std::{fs, os::unix::fs::MetadataExt};
    use tempfile::tempdir;

    #[test]
    fn state_file_must_be_owned_regular_and_absolute() {
        let directory = tempdir().unwrap();
        let state = directory.path().join("state.toml");
        fs::write(&state, "generation = 1").unwrap();
        let uid = fs::metadata(&state).unwrap().uid();
        assert!(read_trusted_user_state(uid, &state).is_ok());
        assert!(read_trusted_user_state(uid, directory.path()).is_err());
        assert!(read_trusted_user_state(uid, std::path::Path::new("state.toml")).is_err());
        let symlink = directory.path().join("state-link.toml");
        std::os::unix::fs::symlink(&state, &symlink).unwrap();
        assert!(read_trusted_user_state(uid, &symlink).is_err());
    }
}

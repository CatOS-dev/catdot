use anyhow::{Context, Result, bail};
use catdot_core::UserRecord;
use std::{
    fs,
    io::Read,
    os::unix::{fs::MetadataExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

pub fn user_record_path(database: &Path, uid: u32) -> PathBuf {
    database.join("users").join(format!("{uid}.toml"))
}
pub fn load_records(database: &Path) -> Result<Vec<UserRecord>> {
    load_records_for_owner(database, 0)
}

fn load_records_for_owner(database: &Path, owner: u32) -> Result<Vec<UserRecord>> {
    let directory = database.join("users");
    if !directory.exists() {
        return Ok(vec![]);
    }
    let directory_metadata = fs::symlink_metadata(&directory)
        .with_context(|| format!("inspect {}", directory.display()))?;
    if !directory_metadata.is_dir()
        || directory_metadata.uid() != owner
        || directory_metadata.mode() & 0o002 != 0
    {
        bail!(
            "unsafe Catdot user record directory {}",
            directory.display()
        )
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&directory).context("read Catdot user records")? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let file_name_uid = path
            .file_stem()
            .and_then(|name| name.to_str())
            .and_then(|name| name.parse::<u32>().ok())
            .with_context(|| format!("invalid user record name {}", path.display()))?;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("inspect {}", path.display()))?;
        if !metadata.is_file() || metadata.uid() != owner || metadata.mode() & 0o002 != 0 {
            bail!("unsafe Catdot user record {}", path.display())
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .with_context(|| format!("read {}", path.display()))?;
        let record: UserRecord =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        if record.uid != file_name_uid {
            bail!(
                "user record UID does not match its filename: {}",
                path.display()
            )
        }
        records.push(record);
    }
    Ok(records)
}
pub fn replace_record(records: &mut Vec<UserRecord>, record: UserRecord) {
    records.retain(|current| current.uid != record.uid);
    records.push(record);
}

pub fn valid_records<F>(records: Vec<UserRecord>, mut exists: F) -> Vec<UserRecord>
where
    F: FnMut(u32) -> bool,
{
    records
        .into_iter()
        .filter(|record| exists(record.uid))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{load_records_for_owner, valid_records};
    use catdot_core::UserRecord;
    use std::{collections::BTreeMap, fs, os::unix::fs::MetadataExt, path::PathBuf};
    use tempfile::tempdir;

    #[test]
    fn invalid_uid_records_are_not_aggregated() {
        let records = [1000, 2999]
            .into_iter()
            .map(|uid| UserRecord {
                uid,
                pending_generation: 1,
                active_generation: 1,
                state_path: PathBuf::from("/home/test/.local/state/catdot/state.toml"),
                components: BTreeMap::new(),
                active_components: BTreeMap::new(),
                active_requirements: BTreeMap::new(),
                pending_requirements: BTreeMap::new(),
            })
            .collect();
        let active = valid_records(records, |uid| uid == 1000);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].uid, 1000);
    }

    #[test]
    fn record_loading_rejects_symlinks_and_uid_filename_mismatches() {
        let directory = tempdir().unwrap();
        let users = directory.path().join("users");
        fs::create_dir(&users).unwrap();
        let owner = fs::metadata(&users).unwrap().uid();
        let record = UserRecord {
            uid: 1000,
            pending_generation: 1,
            active_generation: 1,
            state_path: PathBuf::from("/home/test/.local/state/catdot/state.toml"),
            components: BTreeMap::new(),
            active_components: BTreeMap::new(),
            active_requirements: BTreeMap::new(),
            pending_requirements: BTreeMap::new(),
        };
        let source = directory.path().join("source.toml");
        fs::write(&source, toml::to_string(&record).unwrap()).unwrap();
        std::os::unix::fs::symlink(&source, users.join("1000.toml")).unwrap();
        assert!(load_records_for_owner(directory.path(), owner).is_err());
        fs::remove_file(users.join("1000.toml")).unwrap();
        fs::write(users.join("1001.toml"), toml::to_string(&record).unwrap()).unwrap();
        assert!(load_records_for_owner(directory.path(), owner).is_err());
    }
}

use anyhow::{Context, Result};
use catdot_core::UserRecord;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn user_record_path(database: &Path, uid: u32) -> PathBuf {
    database.join("users").join(format!("{uid}.toml"))
}
pub fn load_records(database: &Path) -> Result<Vec<UserRecord>> {
    let directory = database.join("users");
    if !directory.exists() {
        return Ok(vec![]);
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&directory).context("read Catdot user records")? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        records.push(toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?);
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
    use super::valid_records;
    use catdot_core::UserRecord;
    use std::collections::BTreeMap;

    #[test]
    fn invalid_uid_records_are_not_aggregated() {
        let records = [1000, 2999]
            .into_iter()
            .map(|uid| UserRecord {
                uid,
                generation: 1,
                components: BTreeMap::new(),
                requirements: BTreeMap::new(),
            })
            .collect();
        let active = valid_records(records, |uid| uid == 1000);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].uid, 1000);
    }
}

use crate::{Error, Result};
use configparser::ini::Ini;

pub fn merge_ini(existing: &str, section: &str, changes: &[(&str, &str)]) -> Result<String> {
    let mut config = Ini::new_cs();
    config
        .read(existing.to_owned())
        .map_err(|error| Error::Message(format!("parse INI: {error}")))?;
    for (key, value) in changes {
        config.setstr(section, key, Some(value));
    }
    Ok(config.writes())
}

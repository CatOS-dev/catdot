use crate::{Error, Profile, Result, UserState};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// The root-owned marker changed by the pacman hook.  Tests may redirect it
/// without granting a desktop process permission to write /var.
pub fn system_generation_path() -> PathBuf {
    std::env::var_os("CATDOT_SYSTEM_GENERATION")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/catdot/generation"))
}

pub fn read_system_generation() -> Result<u64> {
    let path = system_generation_path();
    match fs::read_to_string(&path) {
        Ok(value) => value
            .trim()
            .parse()
            .map_err(|_| Error::Message(format!("{}: invalid system generation", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(source) => Err(Error::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

/// Digest the activation inputs that require a per-user write.  Symlink
/// content is intentionally excluded: a correct link sees package updates
/// directly, while a changed link path remains part of this digest.
pub fn activation_digests(
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for (role, reference) in &state.components {
        let (profile_id, component_id) = reference
            .split_once('/')
            .ok_or_else(|| Error::Message(format!("invalid component reference {reference}")))?;
        let profile = profiles
            .get(profile_id)
            .ok_or_else(|| Error::Message(format!("profile {profile_id} is not installed")))?;
        let component = profile
            .components
            .get(component_id)
            .ok_or_else(|| Error::Message(format!("component {reference} is not installed")))?;
        let mut digest = Sha256::new();
        digest.update(profile.id.as_bytes());
        digest.update(reference.as_bytes());
        digest.update(component.role.as_bytes());
        for package in &component.packages {
            digest.update(package.as_bytes());
            digest.update([0]);
        }
        for argument in &component.exec {
            digest.update(argument.as_bytes());
            digest.update([0]);
        }
        if let Some(command) = &component.xdg.command {
            digest.update(command.as_bytes());
        }
        for key in &component.xdg.environment {
            digest.update(key.as_bytes());
            digest.update([0]);
        }
        if let Some(desktop) = &component.xdg.desktop_entry {
            digest.update(desktop.as_bytes());
        }
        for association in component
            .xdg
            .mime_types
            .iter()
            .chain(component.xdg.uri_schemes.iter())
        {
            digest.update(association.as_bytes());
            digest.update([0]);
        }
        if let Some(wm) = &component.wm {
            digest.update(b"wm");
            digest.update(wm.autostart_target.as_os_str().as_encoded_bytes());
            digest.update([0]);
            digest.update(wm.autostart_template.as_bytes());
            digest.update([0]);
            for entry in &wm.autostart {
                digest.update(entry.role.as_bytes());
                digest.update([0]);
                for role in &entry.before {
                    digest.update(b"before");
                    digest.update(role.as_bytes());
                    digest.update([0]);
                }
                for role in &entry.after {
                    digest.update(b"after");
                    digest.update(role.as_bytes());
                    digest.update([0]);
                }
            }
        }
        for entry in &component.configuration {
            digest.update(entry.target.as_os_str().as_encoded_bytes());
            match &entry.lifecycle {
                crate::Lifecycle::Generate => {
                    digest.update(b"generate");
                    if let Some(template) = &entry.template {
                        digest.update(template.as_bytes());
                    } else {
                        let source = profile
                            .source_root
                            .join(entry.source.as_ref().expect("validated source"));
                        digest_file(&mut digest, &source)?;
                    }
                }
                crate::Lifecycle::Symlink => {
                    digest.update(b"symlink");
                    digest.update(
                        profile
                            .source_root
                            .join(entry.source.as_ref().expect("validated source"))
                            .as_os_str()
                            .as_encoded_bytes(),
                    );
                }
                crate::Lifecycle::Merge => {
                    digest.update(b"merge");
                    let source = profile
                        .source_root
                        .join(entry.source.as_ref().expect("validated source"));
                    digest_file(&mut digest, &source)?;
                }
                crate::Lifecycle::User => digest.update(b"user"),
            }
        }
        values.insert(role.clone(), format!("{:x}", digest.finalize()));
    }
    Ok(values)
}

/// Separate dependency declarations from activation inputs.  A package that
/// happens to be installed must not let a changed profile declaration bypass
/// the explicit resolve confirmation boundary.
pub fn package_digests(
    profiles: &BTreeMap<String, Profile>,
    state: &UserState,
) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for (role, reference) in &state.components {
        let (profile_id, component_id) = reference
            .split_once('/')
            .ok_or_else(|| Error::Message(format!("invalid component reference {reference}")))?;
        let component = profiles
            .get(profile_id)
            .and_then(|profile| profile.components.get(component_id))
            .ok_or_else(|| Error::Message(format!("component {reference} is not installed")))?;
        let mut digest = Sha256::new();
        for package in &component.packages {
            digest.update(package.as_bytes());
            digest.update([0]);
        }
        values.insert(role.clone(), format!("{:x}", digest.finalize()));
    }
    Ok(values)
}

fn digest_file(digest: &mut Sha256, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    if metadata.is_file() {
        digest.update(fs::read(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?);
        return Ok(());
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            digest.update(entry.file_name().as_encoded_bytes());
            digest_file(digest, &entry.path())?;
        }
        return Ok(());
    }
    if metadata.file_type().is_symlink() {
        digest.update(
            fs::read_link(path)
                .map_err(|source| Error::Io {
                    path: path.display().to_string(),
                    source,
                })?
                .as_os_str()
                .as_encoded_bytes(),
        );
        return Ok(());
    }
    Err(Error::Message(format!(
        "unsupported activation source {}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ComponentDef, ConfigurationEntry, Lifecycle, XdgProvider};
    use std::{collections::BTreeMap, fs};
    use tempfile::tempdir;

    // A package upgrade changes the content behind a managed symlink.  The
    // user service must not rewrite HOME solely for that change, but it must
    // repair the link when the declared source path changes.
    #[test]
    fn symlink_digest_uses_its_path_not_its_contents() {
        let root = tempdir().unwrap();
        let source = root.path().join("share/default.kdl");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "first").unwrap();
        let component = ComponentDef {
            role: "wm".into(),
            packages: vec![],
            exec: vec![],
            xdg: XdgProvider::default(),
            wm: None,
            configuration: vec![ConfigurationEntry {
                target: ".config/niri/default.kdl".into(),
                lifecycle: Lifecycle::Symlink,
                source: Some("default.kdl".into()),
                template: None,
                seed: None,
            }],
        };
        let mut profile = Profile {
            id: "demo".into(),
            name: "Demo".into(),
            description: "".into(),
            source_root: root.path().join("share"),
            defaults: BTreeMap::new(),
            components: BTreeMap::new(),
        };
        profile.components.insert("wm".into(), component);
        let mut profiles = BTreeMap::new();
        profiles.insert("demo".into(), profile);
        let mut state = UserState::default();
        state.components.insert("wm".into(), "demo/wm".into());
        let first = activation_digests(&profiles, &state).unwrap();
        fs::write(&source, "second").unwrap();
        assert_eq!(first, activation_digests(&profiles, &state).unwrap());
    }
}

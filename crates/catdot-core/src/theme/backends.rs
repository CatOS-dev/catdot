use crate::{ComponentDef, Error, Result, atomic_write, merge_ini};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn setting<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    settings
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| Error::Message(format!("theme backend requires settings.{key}")))
}
struct ThemeWrite {
    path: PathBuf,
    previous: Option<String>,
    contents: String,
}

fn merged_file(path: &Path, section: &str, changes: &[(&str, &str)]) -> Result<ThemeWrite> {
    let previous = if path.exists() {
        Some(fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?)
    } else {
        None
    };
    let existing = previous.as_deref().unwrap_or_default();
    let contents = merge_ini(existing, section, changes)?;
    Ok(ThemeWrite {
        path: path.to_owned(),
        previous,
        contents,
    })
}

fn commit_theme_writes(writes: &[ThemeWrite]) -> Result<()> {
    let mut completed = Vec::new();
    for write in writes {
        if let Err(error) = atomic_write(&write.path, &write.contents) {
            for previous in completed.into_iter().rev() {
                let _ = restore_theme_write(previous);
            }
            return Err(error);
        }
        completed.push(write);
    }
    Ok(())
}

fn restore_theme_write(write: &ThemeWrite) -> Result<()> {
    match &write.previous {
        Some(contents) => atomic_write(&write.path, contents),
        None if write.path.exists() => fs::remove_file(&write.path).map_err(|source| Error::Io {
            path: write.path.display().to_string(),
            source,
        }),
        None => Ok(()),
    }
}
pub fn apply_theme(component: &ComponentDef, config_home: &Path, plasma: bool) -> Result<()> {
    match component.backend.as_deref() {
        Some("gtk") => apply_gtk(&component.settings, config_home),
        Some("qtct-kvantum") => apply_qtct_kvantum(&component.settings, config_home, plasma),
        Some(other) => Err(Error::Message(format!("unsupported theme backend {other}"))),
        None => Ok(()),
    }
}
fn apply_gtk(settings: &BTreeMap<String, String>, config_home: &Path) -> Result<()> {
    let dark = if setting(settings, "color_scheme")? == "prefer-dark" {
        "1"
    } else {
        "0"
    };
    let changes = [
        ("gtk-theme-name", setting(settings, "theme")?),
        ("gtk-icon-theme-name", setting(settings, "icon_theme")?),
        ("gtk-cursor-theme-name", setting(settings, "cursor_theme")?),
        ("gtk-font-name", setting(settings, "font")?),
        ("gtk-application-prefer-dark-theme", dark),
    ];
    let writes = [
        merged_file(
            &config_home.join("gtk-3.0/settings.ini"),
            "Settings",
            &changes,
        )?,
        merged_file(
            &config_home.join("gtk-4.0/settings.ini"),
            "Settings",
            &changes,
        )?,
    ];
    commit_theme_writes(&writes)
}
fn apply_qtct_kvantum(
    settings: &BTreeMap<String, String>,
    config_home: &Path,
    plasma: bool,
) -> Result<()> {
    if plasma {
        return Err(Error::Message(
            "qtct-kvantum is unsupported in a Plasma session; select a KDE-specific backend".into(),
        ));
    }
    let appearance = [
        ("style", setting(settings, "qt5_style")?),
        ("icon_theme", setting(settings, "icon_theme")?),
    ];
    let qt5 = merged_file(
        &config_home.join("qt5ct/qt5ct.conf"),
        "Appearance",
        &appearance,
    )?;
    let appearance = [
        ("style", setting(settings, "qt6_style")?),
        ("icon_theme", setting(settings, "icon_theme")?),
    ];
    let qt6 = merged_file(
        &config_home.join("qt6ct/qt6ct.conf"),
        "Appearance",
        &appearance,
    )?;
    let kvantum = merged_file(
        &config_home.join("Kvantum/kvantum.kvconfig"),
        "General",
        &[("theme", setting(settings, "kvantum_theme")?)],
    )?;
    commit_theme_writes(&[qt5, qt6, kvantum])
}

#[cfg(test)]
mod tests {
    use super::{ThemeWrite, commit_theme_writes};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn failed_later_theme_write_restores_earlier_file() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("first.ini");
        let blocked_parent = directory.path().join("blocked");
        fs::write(&first, "old").unwrap();
        fs::write(&blocked_parent, "not a directory").unwrap();
        let writes = [
            ThemeWrite {
                path: first.clone(),
                previous: Some("old".into()),
                contents: "new".into(),
            },
            ThemeWrite {
                path: blocked_parent.join("second.ini"),
                previous: None,
                contents: "new".into(),
            },
        ];
        assert!(commit_theme_writes(&writes).is_err());
        assert_eq!(fs::read_to_string(first).unwrap(), "old");
    }
}

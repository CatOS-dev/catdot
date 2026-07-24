use catdot_core::ComponentDef;
use std::process::{Command, Stdio};

pub fn sync_gtk_settings(component: &ComponentDef) {
    if component.backend.as_deref() != Some("gtk") {
        return;
    }
    match schema_available() {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("warning: gsettings schema org.gnome.desktop.interface is unavailable");
            return;
        }
        Err(error) => {
            eprintln!("warning: cannot inspect gsettings schemas: {error}");
            return;
        }
    }
    let Some(theme) = component.settings.get("theme") else {
        return;
    };
    let Some(icon_theme) = component.settings.get("icon_theme") else {
        return;
    };
    let Some(cursor_theme) = component.settings.get("cursor_theme") else {
        return;
    };
    let Some(font) = component.settings.get("font") else {
        return;
    };
    let Some(color_scheme) = component.settings.get("color_scheme") else {
        return;
    };
    for (key, value) in [
        ("gtk-theme", theme),
        ("icon-theme", icon_theme),
        ("cursor-theme", cursor_theme),
        ("font-name", font),
        ("color-scheme", color_scheme),
    ] {
        let status = Command::new("gsettings")
            .args([
                "set",
                "org.gnome.desktop.interface",
                key,
                &gvariant_string(value),
            ])
            .status();
        if !status.is_ok_and(|status| status.success()) {
            eprintln!("warning: could not synchronize GTK setting {key} through gsettings")
        }
    }
}

fn gvariant_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn schema_available() -> Result<bool, String> {
    let output = Command::new("gsettings")
        .arg("list-schemas")
        .stdout(Stdio::piped())
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line == "org.gnome.desktop.interface"))
}

#[cfg(test)]
mod tests {
    use super::gvariant_string;

    #[test]
    fn gvariant_strings_are_quoted_and_escaped() {
        assert_eq!(gvariant_string("Graphite Dark"), "'Graphite Dark'");
        assert_eq!(gvariant_string("A'B\\C"), "'A\\'B\\\\C'");
    }
}

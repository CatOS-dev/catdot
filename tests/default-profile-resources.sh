#!/usr/bin/env bash
set -euxo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
profile="$repo_root/profiles/catos-default/profile.toml"
temp_root=$(mktemp -d)
trap 'rm -rf "$temp_root"' EXIT

while IFS=$'\t' read -r kind first second; do
  case "$kind" in
    package)
      pacman -Si "$first" > /dev/null
      ;;
    resource)
      test -d "$first"
      pacman -Qo "$first" | grep -F "is owned by $second "
      ;;
  esac
done < <(python3 - "$profile" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as profile_file:
    profile = tomllib.load(profile_file)

components = profile["components"]
gtk = next(component for component in components.values() if component.get("backend") == "gtk")
qt = next(component for component in components.values() if component.get("backend") == "qtct-kvantum")

for package in gtk["packages"] + qt["packages"]:
    print("package", package, "", sep="\t")

for directory, package in zip(
    (
        f"/usr/share/themes/{gtk['settings']['theme']}",
        f"/usr/share/icons/{gtk['settings']['icon_theme']}",
        f"/usr/share/icons/{gtk['settings']['cursor_theme']}",
    ),
    gtk["packages"],
):
    print("resource", directory, package, sep="\t")
print("resource", f"/usr/share/Kvantum/{qt['settings']['kvantum_theme']}", "kvantum", sep="\t")
PY
)

cargo build --release --locked --manifest-path "$repo_root/Cargo.toml"
mkdir -p "$temp_root/home/.local/state/catdot"
mkdir -p "$temp_root/bin"
printf '%s\n' '#!/bin/sh' 'if test "$1" = "-Q"; then exit 0; fi' 'exec /usr/bin/pacman "$@"' \
  > "$temp_root/bin/pacman"
chmod 755 "$temp_root/bin/pacman"
python3 - "$profile" "$temp_root/home/.local/state/catdot/state.toml" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as profile_file:
    profile = tomllib.load(profile_file)
components = profile["defaults"]
references = {role: f"{profile['profile']['id']}/{component}" for role, component in components.items()}
with open(sys.argv[2], "w", encoding="utf-8") as state_file:
    state_file.write("schema = 1\ngeneration = 1\nactive_generation = 1\n")
    for table in ("components", "active_components"):
        state_file.write(f"[{table}]\n")
        for role, reference in references.items():
            state_file.write(f'{role} = "{reference}"\n')
PY

PATH="$temp_root/bin:$PATH" HOME="$temp_root/home" XDG_CONFIG_HOME="$temp_root/home/.config" \
  CATDOT_PROFILE_ROOT="$repo_root/profiles" \
  "$repo_root/target/release/catdot" apply
python3 - "$profile" "$temp_root/home/.config" <<'PY'
import configparser
import sys
import tomllib

with open(sys.argv[1], "rb") as profile_file:
    profile = tomllib.load(profile_file)
config_home = sys.argv[2]
components = profile["components"]
gtk = next(component for component in components.values() if component.get("backend") == "gtk")
qt = next(component for component in components.values() if component.get("backend") == "qtct-kvantum")

def read(path):
    parser = configparser.ConfigParser()
    parser.optionxform = str
    parser.read(path)
    return parser

gtk_config = read(f"{config_home}/gtk-3.0/settings.ini")["Settings"]
assert gtk_config["gtk-theme-name"] == gtk["settings"]["theme"]
assert gtk_config["gtk-icon-theme-name"] == gtk["settings"]["icon_theme"]
assert gtk_config["gtk-cursor-theme-name"] == gtk["settings"]["cursor_theme"]
qt5_config = read(f"{config_home}/qt5ct/qt5ct.conf")["Appearance"]
assert qt5_config["style"] == qt["settings"]["qt5_style"]
assert qt5_config["icon_theme"] == qt["settings"]["icon_theme"]
kvantum_config = read(f"{config_home}/Kvantum/kvantum.kvconfig")["General"]
assert kvantum_config["theme"] == qt["settings"]["kvantum_theme"]
PY

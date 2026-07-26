use anyhow::{Context, Result};
use catdot_core::{ProfileRegistry, discover_profile_registry, profile_root, state_path};
use std::{env, path::PathBuf};

pub(super) fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(env::var("HOME").context("HOME is not set")?))
}

pub(super) fn profiles() -> Result<ProfileRegistry> {
    discover_profile_registry(&profile_root()).map_err(Into::into)
}

pub(super) fn state_file() -> Result<PathBuf> {
    Ok(state_path(&home()?))
}

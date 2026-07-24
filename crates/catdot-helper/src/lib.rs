mod auth;
mod backend;
mod commands;
mod system;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HelperMode {
    Manage,
    Query,
}

pub fn run_manage() -> anyhow::Result<()> {
    commands::run(HelperMode::Manage)
}

pub fn run_query() -> anyhow::Result<()> {
    commands::run(HelperMode::Query)
}

mod auth;
mod backend;
mod commands;
mod system;

fn main() -> anyhow::Result<()> {
    commands::run()
}

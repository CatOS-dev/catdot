mod commands;
mod services;

fn main() -> anyhow::Result<()> {
    commands::run()
}

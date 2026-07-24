use anyhow::{Context, Result, bail};

pub fn caller_uid(claimed: u32) -> Result<u32> {
    let value = std::env::var("PKEXEC_UID")
        .context("missing PKEXEC_UID; helper must be launched by pkexec")?;
    let uid = value.parse::<u32>().context("invalid PKEXEC_UID")?;
    if uid != claimed {
        bail!("claimed uid does not match pkexec caller")
    }
    Ok(uid)
}

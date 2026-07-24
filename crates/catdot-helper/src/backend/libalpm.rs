use alpm::{Alpm, PackageReason, SigLevel, TransFlag};
use anyhow::{Context, Result, bail};
use catdot_core::{PackageAvailability, PackageBackend, PackagePlan, install_plan};
use std::{collections::BTreeSet, process::Command};

pub fn open_handle() -> Result<Alpm> {
    let root = pacman_conf_value("RootDir")?;
    let database = pacman_conf_value("DBPath")?;
    let mut handle =
        Alpm::new(root.as_str(), database.as_str()).context("open libalpm database")?;
    handle
        .set_default_siglevel(signature_level(&pacman_conf_values("SigLevel")?))
        .context("configure default pacman signature policy")?;
    let cache_dirs = pacman_conf_values("CacheDir")?;
    handle
        .set_cachedirs(cache_dirs.iter().map(String::as_str))
        .context("configure libalpm cache")?;
    for repo in pacman_conf_values("--repo-list")? {
        let db = handle
            .register_syncdb_mut(
                repo.as_str(),
                repository_signature_level(&pacman_repo_raw_values(&repo, "SigLevel")?),
            )
            .with_context(|| format!("register {repo} sync database"))?;
        for server in pacman_repo_values(&repo, "Server")? {
            db.add_server(server)
                .with_context(|| format!("configure server for {repo}"))?;
        }
    }
    Ok(handle)
}

fn pacman_conf_values(directive: &str) -> Result<Vec<String>> {
    let output = Command::new("pacman-conf")
        .arg(directive)
        .output()
        .with_context(|| format!("run pacman-conf {directive}"))?;
    if !output.status.success() {
        bail!(
            "pacman-conf {directive} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    let values: Vec<_> = String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.strip_prefix("CacheDir = ").unwrap_or(line).to_owned())
        .collect();
    if values.is_empty() {
        bail!("pacman-conf {directive} returned no values")
    }
    Ok(values)
}

fn pacman_conf_value(directive: &str) -> Result<String> {
    let values = pacman_conf_values(directive)?;
    values
        .into_iter()
        .next()
        .context("pacman-conf returned no primary value")
}

fn pacman_repo_values(repo: &str, directive: &str) -> Result<Vec<String>> {
    Ok(pacman_repo_raw_values(repo, directive)?
        .into_iter()
        .filter_map(|line| line.strip_prefix("Server = ").map(str::to_owned))
        .collect())
}

fn pacman_repo_raw_values(repo: &str, directive: &str) -> Result<Vec<String>> {
    let output = Command::new("pacman-conf")
        .args(["--repo", repo, directive])
        .output()
        .with_context(|| format!("run pacman-conf for repository {repo}"))?;
    if !output.status.success() {
        bail!(
            "pacman-conf repository {repo} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn signature_level(values: &[String]) -> SigLevel {
    let mut level = SigLevel::NONE;
    for value in values {
        match value.as_str() {
            "PackageRequired" => level |= SigLevel::PACKAGE,
            "PackageOptional" => level |= SigLevel::PACKAGE | SigLevel::PACKAGE_OPTIONAL,
            "PackageTrustAll" => {
                level |= SigLevel::PACKAGE_MARGINAL_OK | SigLevel::PACKAGE_UNKNOWN_OK
            }
            "DatabaseRequired" => level |= SigLevel::DATABASE,
            "DatabaseOptional" => level |= SigLevel::DATABASE | SigLevel::DATABASE_OPTIONAL,
            "DatabaseTrustAll" => {
                level |= SigLevel::DATABASE_MARGINAL_OK | SigLevel::DATABASE_UNKNOWN_OK
            }
            "PackageNever" | "DatabaseNever" | "PackageTrustedOnly" | "DatabaseTrustedOnly" => {}
            _ => {}
        }
    }
    level
}

fn repository_signature_level(values: &[String]) -> SigLevel {
    if values.is_empty() {
        SigLevel::USE_DEFAULT
    } else {
        signature_level(values)
    }
}
pub fn package_plan(handle: &Alpm, packages: &BTreeSet<String>) -> Result<PackagePlan> {
    install_plan(&AlpmQuery(handle), packages).map_err(|error| anyhow::anyhow!(error.to_string()))
}

pub fn satisfier_name(handle: &Alpm, dependency: &str) -> Result<String> {
    handle
        .localdb()
        .pkgs()
        .find_satisfier(dependency)
        .or_else(|| handle.syncdbs().find_satisfier(dependency))
        .map(|package| package.name().to_owned())
        .with_context(|| format!("package {dependency} is unavailable in configured repositories"))
}

pub fn prepared_install_plan(
    handle: &mut Alpm,
    packages: &BTreeSet<String>,
) -> Result<PackagePlan> {
    let mut plan = package_plan(handle, packages)?;
    if plan.install.is_empty() {
        return Ok(plan);
    }
    handle
        .trans_init(TransFlag::NO_LOCK)
        .context("initialize libalpm planning transaction")?;
    for name in &plan.install {
        let package = sync_package(handle, name)?;
        handle.trans_add_pkg(package).map_err(|error| {
            anyhow::anyhow!("add package {name} to planning transaction: {error}")
        })?;
    }
    let result = handle
        .trans_prepare()
        .map_err(|error| anyhow::anyhow!("libalpm dependency/conflict validation failed: {error}"));
    if result.is_ok() {
        plan.install = handle
            .trans_add()
            .iter()
            .map(|package| package.name().to_owned())
            .collect();
        plan.install.sort();
    }
    handle
        .trans_release()
        .context("release libalpm planning transaction")?;
    result?;
    Ok(plan)
}

struct AlpmQuery<'a>(&'a Alpm);

impl PackageBackend for AlpmQuery<'_> {
    fn availability(&self, package: &str) -> catdot_core::Result<PackageAvailability> {
        if self.0.localdb().pkgs().find_satisfier(package).is_some() {
            Ok(PackageAvailability::Installed)
        } else if self.0.syncdbs().find_satisfier(package).is_some() {
            Ok(PackageAvailability::Available)
        } else {
            Ok(PackageAvailability::Unavailable)
        }
    }

    fn can_remove(&self, package: &str) -> catdot_core::Result<bool> {
        Ok(removable_with_alpm(self.0, package))
    }
}
pub fn install_with_alpm(handle: &mut Alpm, names: &[String]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    handle
        .trans_init(TransFlag::NONE)
        .context("initialize libalpm transaction")?;
    for name in names {
        let package = sync_package(handle, name)?;
        handle
            .trans_add_pkg(package)
            .map_err(|error| anyhow::anyhow!("add package {name} to transaction: {error}"))?;
    }
    let prepare_error = handle.trans_prepare().err().map(|error| error.to_string());
    if let Some(error) = prepare_error {
        let _ = handle.trans_release();
        bail!("libalpm dependency/conflict validation failed: {error}")
    }
    let commit_error = handle.trans_commit().err().map(|error| error.to_string());
    if let Some(error) = commit_error {
        let _ = handle.trans_release();
        bail!("libalpm transaction failed: {error}")
    }
    for name in names {
        handle
            .localdb()
            .pkg(name.as_str())
            .with_context(|| format!("locate newly installed package {name}"))?
            .set_reason(PackageReason::Depend)
            .with_context(|| format!("mark newly installed package {name} as dependency"))?;
    }
    handle
        .trans_release()
        .context("release libalpm transaction")?;
    Ok(())
}

fn sync_package<'a>(handle: &'a Alpm, name: &str) -> Result<&'a alpm::Package> {
    handle
        .syncdbs()
        .find_satisfier(name)
        .with_context(|| format!("locate package {name} in sync database"))
}

pub fn removable_with_alpm(handle: &Alpm, name: &str) -> bool {
    handle
        .localdb()
        .pkg(name)
        .is_ok_and(|package| package.reason() == PackageReason::Depend)
}

pub fn prepared_removal_plan(handle: &mut Alpm, names: &[String]) -> Result<Vec<String>> {
    if names.is_empty() {
        return Ok(vec![]);
    }
    handle
        .trans_init(TransFlag::NO_LOCK)
        .context("initialize libalpm prune planning transaction")?;
    for name in names {
        let package = handle
            .localdb()
            .pkg(name.as_str())
            .with_context(|| format!("locate installed package {name}"))?;
        handle
            .trans_remove_pkg(package)
            .with_context(|| format!("mark {name} for prune planning"))?;
    }
    let result = handle
        .trans_prepare()
        .map_err(|error| anyhow::anyhow!("libalpm rejected prune: {error}"));
    let planned = if result.is_ok() {
        let mut planned: Vec<_> = handle
            .trans_remove()
            .iter()
            .map(|package| package.name().to_owned())
            .collect();
        planned.sort();
        planned
    } else {
        vec![]
    };
    handle
        .trans_release()
        .context("release libalpm prune planning transaction")?;
    result?;
    Ok(planned)
}

pub fn remove_with_alpm(handle: &mut Alpm, names: &[String]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    handle
        .trans_init(TransFlag::NONE)
        .context("initialize libalpm removal transaction")?;
    for name in names {
        let package = handle
            .localdb()
            .pkg(name.as_str())
            .with_context(|| format!("locate installed package {name}"))?;
        handle
            .trans_remove_pkg(package)
            .with_context(|| format!("mark {name} for removal"))?;
    }
    let prepare_error = handle.trans_prepare().err().map(|error| error.to_string());
    if let Some(error) = prepare_error {
        let _ = handle.trans_release();
        bail!("libalpm rejected prune because another package depends on it: {error}")
    }
    let commit_error = handle.trans_commit().err().map(|error| error.to_string());
    if let Some(error) = commit_error {
        let _ = handle.trans_release();
        bail!("libalpm prune transaction failed: {error}")
    }
    handle
        .trans_release()
        .context("release libalpm removal transaction")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{open_handle, repository_signature_level, signature_level};
    use alpm::SigLevel;

    #[test]
    fn signature_levels_follow_pacman_conf_tokens() {
        let values = ["PackageRequired", "DatabaseOptional"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(
            signature_level(&values),
            SigLevel::PACKAGE | SigLevel::DATABASE | SigLevel::DATABASE_OPTIONAL
        );
        assert_eq!(repository_signature_level(&[]), SigLevel::USE_DEFAULT);
    }

    #[test]
    #[ignore = "reads the host pacman configuration and databases"]
    fn opens_host_alpm_configuration_read_only() {
        let handle = open_handle().unwrap();
        assert!(!handle.syncdbs().is_empty());
    }
}

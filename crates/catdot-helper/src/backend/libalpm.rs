use alpm::{
    Alpm, AnyQuestion, CommitError, PackageFrom, PackageReason, PrepareError, Question, SigLevel,
    TransFlag, Usage,
};
use anyhow::{Context, Result, bail};
use catdot_core::{
    PackageAvailability, PackageBackend, PackagePlan, PackageReplacement, install_plan,
};
use std::{
    collections::BTreeSet,
    process::Command,
    sync::{Arc, Mutex},
};

pub fn open_handle() -> Result<Alpm> {
    let root = pacman_conf_value("RootDir")?;
    let database = pacman_conf_value("DBPath")?;
    let mut handle =
        Alpm::new(root.as_str(), database.as_str()).context("open libalpm database")?;
    let cache_dirs = pacman_conf_required_values("CacheDir")?;
    let hook_dirs = pacman_conf_values("HookDir")?;
    let architectures = pacman_conf_required_values("Architecture")?;
    let log_file = pacman_conf_value("LogFile")?;
    let gpg_dir = pacman_conf_value("GPGDir")?;
    handle
        .set_default_siglevel(signature_level(&pacman_conf_values("SigLevel")?))
        .context("configure default pacman signature policy")?;
    handle
        .set_cachedirs(cache_dirs.iter().map(String::as_str))
        .context("configure libalpm cache")?;
    handle
        .set_hookdirs(hook_dirs.iter().map(String::as_str))
        .context("configure libalpm hooks")?;
    handle
        .set_architectures(architectures.iter().map(String::as_str))
        .context("configure libalpm architectures")?;
    handle
        .set_logfile(log_file)
        .context("configure libalpm log file")?;
    handle
        .set_gpgdir(gpg_dir)
        .context("configure libalpm GPG directory")?;
    handle.set_check_space(check_space(&pacman_conf_values("CheckSpace")?));
    handle.set_parallel_downloads(
        pacman_conf_value("ParallelDownloads")?
            .parse()
            .context("parse pacman ParallelDownloads as an unsigned integer")?,
    );
    handle
        .set_ignorepkgs(pacman_conf_values("IgnorePkg")?.iter().map(String::as_str))
        .context("configure pacman IgnorePkg")?;
    handle
        .set_ignoregroups(
            pacman_conf_values("IgnoreGroup")?
                .iter()
                .map(String::as_str),
        )
        .context("configure pacman IgnoreGroup")?;
    handle
        .set_noupgrades(pacman_conf_values("NoUpgrade")?.iter().map(String::as_str))
        .context("configure pacman NoUpgrade")?;
    handle
        .set_noextracts(pacman_conf_values("NoExtract")?.iter().map(String::as_str))
        .context("configure pacman NoExtract")?;
    if let Some(user) = pacman_conf_values("DownloadUser")?.into_iter().next() {
        handle
            .set_sandbox_user(Some(user))
            .context("configure pacman DownloadUser")?;
    }
    handle.set_disable_dl_timeout(!pacman_conf_values("DisableDownloadTimeout")?.is_empty());
    let disable_sandbox = !pacman_conf_values("DisableSandbox")?.is_empty();
    handle.set_disable_sandbox_filesystem(disable_sandbox);
    handle.set_disable_sandbox_syscalls(disable_sandbox);
    handle
        .set_local_file_siglevel(signature_level(&pacman_conf_values("LocalFileSigLevel")?))
        .context("configure LocalFileSigLevel")?;
    handle
        .set_remote_file_siglevel(signature_level(&pacman_conf_values("RemoteFileSigLevel")?))
        .context("configure RemoteFileSigLevel")?;
    for repo in pacman_conf_values("--repo-list")? {
        let servers = pacman_repo_values(&repo, "Server")?
            .into_iter()
            .map(|server| expand_server(&server, &repo, &architectures))
            .collect::<Result<Vec<_>>>()?;
        if servers.is_empty() {
            bail!("enabled pacman repository {repo} has no servers")
        }
        let db = handle
            .register_syncdb_mut(
                repo.as_str(),
                repository_signature_level(&pacman_repo_raw_values(&repo, "SigLevel")?),
            )
            .with_context(|| format!("register {repo} sync database"))?;
        db.set_usage(repository_usage(&pacman_repo_raw_values(&repo, "Usage")?)?)
            .with_context(|| format!("configure usage for {repo}"))?;
        for server in servers {
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
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(normalize_pacman_conf_value)
        .collect())
}

fn normalize_pacman_conf_value(line: &str) -> String {
    let Some((key, value)) = line.split_once('=') else {
        return line.to_owned();
    };
    if !key.trim().is_empty()
        && key
            .trim()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        value.trim().to_owned()
    } else {
        line.to_owned()
    }
}

fn pacman_conf_value(directive: &str) -> Result<String> {
    let values = pacman_conf_required_values(directive)?;
    values
        .into_iter()
        .next()
        .context("pacman-conf returned no primary value")
}

fn pacman_conf_required_values(directive: &str) -> Result<Vec<String>> {
    let values = pacman_conf_values(directive)?;
    if values.is_empty() {
        bail!("pacman-conf {directive} returned no values")
    }
    Ok(values)
}

fn pacman_repo_values(repo: &str, directive: &str) -> Result<Vec<String>> {
    Ok(pacman_repo_raw_values(repo, directive)?
        .into_iter()
        .map(|line| normalize_pacman_conf_value(&line))
        .collect())
}

fn expand_server(server: &str, repo: &str, architectures: &[String]) -> Result<String> {
    let architecture = architectures
        .first()
        .context("pacman-conf Architecture returned no values")?;
    Ok(server.replace("$repo", repo).replace("$arch", architecture))
}

fn check_space(values: &[String]) -> bool {
    values.iter().any(|value| value == "CheckSpace")
        && !values.iter().any(|value| value == "NoCheckSpace")
}

fn repository_usage(values: &[String]) -> Result<Usage> {
    if values.is_empty() {
        return Ok(Usage::ALL);
    }
    let mut usage = Usage::NONE;
    for value in values.iter().flat_map(|value| value.split_whitespace()) {
        usage |= match value {
            "All" => Usage::ALL,
            "Sync" => Usage::SYNC,
            "Search" => Usage::SEARCH,
            "Install" => Usage::INSTALL,
            "Upgrade" => Usage::UPGRADE,
            _ => bail!("unsupported pacman repository Usage value {value}"),
        };
    }
    Ok(usage)
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

#[derive(Default)]
struct InstallQuestionState {
    replacements: BTreeSet<PackageReplacement>,
}

fn configure_install_questions(handle: &Alpm) -> Arc<Mutex<InstallQuestionState>> {
    let state = Arc::new(Mutex::new(InstallQuestionState::default()));
    let callback_state = Arc::clone(&state);
    handle.set_question_cb(callback_state, |question: AnyQuestion<'_>, state| {
        match question.question() {
            Question::Replace(replace) => {
                let replacement = PackageReplacement {
                    remove: replace.oldpkg().name().to_owned(),
                    install: replace.newpkg().name().to_owned(),
                    reason: "repository replacement".to_owned(),
                };
                state.lock().expect("question state lock").replacements.insert(replacement);
                replace.set_replace(true);
            }
            Question::Conflict(mut conflict) => {
                let details = conflict.conflict();
                let first = details.package1();
                let second = details.package2();
                let replacement = match (first.origin(), second.origin()) {
                    (PackageFrom::LocalDb, PackageFrom::SyncDb) => Some(PackageReplacement {
                        remove: first.name().to_owned(),
                        install: second.name().to_owned(),
                        reason: details.reason().to_string(),
                    }),
                    (PackageFrom::SyncDb, PackageFrom::LocalDb) => Some(PackageReplacement {
                        remove: second.name().to_owned(),
                        install: first.name().to_owned(),
                        reason: details.reason().to_string(),
                    }),
                    _ => None,
                };
                if let Some(replacement) = replacement {
                    state.lock().expect("question state lock").replacements.insert(replacement);
                    conflict.set_remove(true);
                } else {
                    conflict.set_remove(false);
                }
            }
            Question::SelectProvider(mut provider) => provider.set_index(0),
            Question::InstallIgnorepkg(mut ignored) => ignored.set_install(false),
            Question::RemovePkgs(mut packages) => packages.set_skip(false),
            Question::ImportKey(mut key) => key.set_import(true),
            Question::Corrupted(mut corrupted) => corrupted.set_remove(false),
        }
    });
    state
}

fn prepared_install_transaction(
    handle: &mut Alpm,
    names: &[String],
    flags: TransFlag,
) -> Result<(Vec<String>, Vec<String>, Vec<PackageReplacement>)> {
    let questions = configure_install_questions(handle);
    handle
        .trans_init(flags)
        .context("initialize libalpm install transaction")?;
    for name in names {
        let package = sync_package(handle, name)?;
        handle.trans_add_pkg(package).map_err(|error| {
            anyhow::anyhow!("add package {name} to install transaction: {error}")
        })?;
    }
    let prepare_failure = handle.trans_prepare().err().map(prepare_error);
    if let Some(error) = prepare_failure {
        let _ = handle.trans_release();
        return Err(error);
    }
    let mut install = handle
        .trans_add()
        .iter()
        .map(|package| package.name().to_owned())
        .collect::<Vec<_>>();
    let mut remove = handle
        .trans_remove()
        .iter()
        .map(|package| package.name().to_owned())
        .collect::<Vec<_>>();
    let mut replacements = questions
        .lock()
        .expect("question state lock")
        .replacements
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    install.sort();
    install.dedup();
    remove.sort();
    remove.dedup();
    replacements.sort();
    Ok((install, remove, replacements))
}

pub fn prepared_install_plan(
    handle: &mut Alpm,
    packages: &BTreeSet<String>,
) -> Result<PackagePlan> {
    let mut plan = package_plan(handle, packages)?;
    if plan.install.is_empty() {
        return Ok(plan);
    }
    let requested = plan.install.clone();
    let result = prepared_install_transaction(handle, &requested, TransFlag::NO_LOCK);
    match result {
        Ok((install, remove, replacements)) => {
            plan.install = install;
            plan.remove = remove;
            plan.replacements = replacements;
            handle
                .trans_release()
                .context("release libalpm planning transaction")?;
            Ok(plan)
        }
        Err(error) => Err(error),
    }
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
pub fn install_with_alpm(handle: &mut Alpm, plan: &PackagePlan) -> Result<()> {
    if plan.install.is_empty() {
        return Ok(());
    }
    let (install, remove, replacements) =
        prepared_install_transaction(handle, &plan.install, TransFlag::NONE)?;
    if install != plan.install || remove != plan.remove || replacements != plan.replacements {
        let _ = handle.trans_release();
        bail!("libalpm transaction changed after confirmation; run catdot resolve again")
    }
    if let Err(error) = handle.trans_commit() {
        let _ = handle.trans_release();
        return Err(commit_error(error));
    }
    for name in &plan.install {
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

fn prepare_error(error: PrepareError<'_>) -> anyhow::Error {
    anyhow::anyhow!(
        "libalpm prepare failed ({:?}): {}; data: {:?}",
        error.error(),
        error,
        error.data()
    )
}

fn commit_error(error: CommitError) -> anyhow::Error {
    anyhow::anyhow!(
        "libalpm commit failed ({:?}): {}; data: {:?}",
        error.error(),
        error,
        error.data()
    )
}

fn sync_package<'a>(handle: &'a Alpm, name: &str) -> Result<&'a alpm::Package> {
    handle
        .syncdbs()
        .find_satisfier(name)
        .with_context(|| format!("locate package {name} in sync database"))
}

pub fn hold_packages() -> Result<BTreeSet<String>> {
    Ok(pacman_conf_values("HoldPkg")?.into_iter().collect())
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
    use super::{
        open_handle, pacman_conf_values, repository_signature_level, signature_level,
    };
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
        let download_user = pacman_conf_values("DownloadUser").unwrap();
        assert_eq!(
            handle.sandbox_user(),
            download_user.first().map(String::as_str)
        );
        assert_eq!(
            handle.ignorepkgs().iter().collect::<Vec<_>>(),
            pacman_conf_values("IgnorePkg")
                .unwrap()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            handle.noupgrades().iter().collect::<Vec<_>>(),
            pacman_conf_values("NoUpgrade")
                .unwrap()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
    }
}

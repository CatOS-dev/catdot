use anyhow::{Context, Result, bail};
use catdot_core::*;
use clap::{Parser, Subcommand};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

mod runtime;

const MANAGE_HELPER: &str = "/usr/lib/catdot/catdot-helper";
const QUERY_HELPER: &str = "/usr/lib/catdot/catdot-query-helper";

#[derive(Parser)]
#[command(
    name = "catdot",
    about = "Switch complete CatOS configuration profiles safely",
    long_about = "Install packages, initialize seed files, and transactionally switch complete managed configuration profiles."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate profile declarations under an explicit metadata root.
    Validate {
        #[arg(value_name = "PROFILE_ROOT")]
        profile_root: PathBuf,
    },
    /// List installed profiles.
    List,
    /// Show one installed profile.
    Show { profile: String },
    /// Show the active and retained profiles.
    Current,
    /// Select and activate a complete profile.
    Select {
        profile: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Explicitly refresh a retained profile's packages and managed files.
    Update {
        profile: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Reinstall an active profile's managed and seed files.
    Reset {
        profile: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Forget an inactive retained profile. Packages remain until prune.
    Remove {
        profile: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Remove unreferenced packages originally introduced by Catdot.
    Prune {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Inspect or resolve interrupted package transactions.
    Recover {
        #[command(subcommand)]
        command: RecoverCmd,
    },
    /// Diagnose user configuration and privileged package records.
    Doctor,
}

#[derive(Subcommand)]
enum RecoverCmd {
    List,
    Accept {
        transaction: String,
        #[arg(long)]
        yes: bool,
    },
    Discard {
        transaction: String,
        #[arg(long)]
        yes: bool,
    },
}

fn validate_profile_root(root: &Path) -> Result<i32> {
    let registry = discover_profile_registry(root)?;
    for profile in registry.valid_profiles.values() {
        println!("validated profile {}", profile.id);
    }
    for diagnostic in &registry.diagnostics {
        eprintln!(
            "invalid profile {}: {}",
            diagnostic.manifest_path.display(),
            diagnostic.message
        );
    }
    if registry.valid_profiles.is_empty() && registry.diagnostics.is_empty() {
        eprintln!("no profiles found under {}", root.display());
        return Ok(2);
    }
    Ok(if registry.diagnostics.is_empty() {
        0
    } else {
        2
    })
}

fn installed_profiles() -> Result<std::collections::BTreeMap<String, Profile>> {
    let registry = runtime::profiles()?;
    if !registry.diagnostics.is_empty() {
        for diagnostic in &registry.diagnostics {
            eprintln!(
                "invalid profile {}: {}",
                diagnostic.manifest_path.display(),
                diagnostic.message
            );
        }
        bail!("installed profile declarations are invalid")
    }
    Ok(registry.valid_profiles)
}

fn confirm(yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!("confirmation requires a terminal; pass --yes")
    }
    print!("Proceed? [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
        bail!("cancelled")
    }
    Ok(())
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn pkexec(helper: &str, arguments: &[String], context: &str) -> Result<Output> {
    let output = Command::new("pkexec")
        .arg(helper)
        .args(arguments)
        .output()
        .with_context(|| format!("launch {context}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        bail!("{context} failed: {}", message.trim())
    }
    Ok(output)
}

struct PreviewState {
    path: PathBuf,
}

impl PreviewState {
    fn create(state: &UserState) -> Result<Self> {
        let directory = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_nanos();
        let contents = toml::to_string_pretty(state)?;
        for attempt in 0..32 {
            let path = directory.join(format!(
                "catdot-preview-{}-{stamp}-{attempt}.toml",
                std::process::id()
            ));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(mut file) => {
                    file.write_all(contents.as_bytes())?;
                    file.sync_all()?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        bail!("cannot create a unique package-plan preview file")
    }
}

impl Drop for PreviewState {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn package_preview(
    state_file: &Path,
    prospective: &UserState,
    preview: &PreviewState,
) -> Result<PackagePlanPreview> {
    let arguments = vec![
        "resolve-plan".into(),
        "--uid".into(),
        uid().to_string(),
        "--generation".into(),
        prospective.generation.to_string(),
        "--state-path".into(),
        state_file.display().to_string(),
        "--preview-path".into(),
        preview.path.display().to_string(),
    ];
    let output = pkexec(QUERY_HELPER, &arguments, "package plan")?;
    toml::from_str(&String::from_utf8(output.stdout)?).context("parse package plan")
}

fn apply_package_plan(
    state_file: &Path,
    generation: u64,
    plan: &PackagePlan,
    preview: &PreviewState,
) -> Result<()> {
    let arguments = vec![
        "resolve".into(),
        "--uid".into(),
        uid().to_string(),
        "--generation".into(),
        generation.to_string(),
        "--digest".into(),
        plan.digest(),
        "--state-path".into(),
        state_file.display().to_string(),
        "--preview-path".into(),
        preview.path.display().to_string(),
    ];
    pkexec(MANAGE_HELPER, &arguments, "package synchronization")?;
    Ok(())
}

fn synchronize_package_record(state_file: &Path, state: &UserState) -> Result<()> {
    let preview_state = PreviewState::create(state)?;
    let package = package_preview(state_file, state, &preview_state)?;
    apply_package_plan(state_file, state.generation, &package.plan, &preview_state)
}

fn print_package_plan(preview: &PackagePlanPreview) {
    if preview.plan.install.is_empty() && preview.plan.replacements.is_empty() {
        println!("Packages: no installation required");
    } else {
        if !preview.plan.install.is_empty() {
            println!("Install packages:");
            for package in &preview.plan.install {
                println!("  {package}");
            }
        }
        if !preview.plan.replacements.is_empty() {
            println!("Replace packages:");
            for replacement in &preview.plan.replacements {
                println!("  {} -> {}", replacement.remove, replacement.install);
            }
        }
    }
    if preview.system_update_required {
        println!("Update Catdot package ownership records.");
    }
}

fn print_activation_plan(plan: &ActivationPlan) {
    let mut printed = false;
    for removal in &plan.removals {
        if removal.exists() {
            if !printed {
                println!("Configuration:");
                printed = true;
            }
            println!("  remove managed {}", removal.display());
        }
    }
    for entry in &plan.entries {
        if entry.cache {
            println!("  refresh managed snapshot for {}", plan.target_profile);
            printed = true;
            continue;
        }
        if !printed {
            println!("Configuration:");
            printed = true;
        }
        match &entry.materialization {
            Materialization::Managed { .. } => {
                if entry.target.exists() {
                    println!("  BACKUP AND OVERWRITE managed {}", entry.target.display());
                } else {
                    println!("  install managed {}", entry.target.display());
                }
            }
            Materialization::Seed { overwrite, .. } => {
                if *overwrite {
                    println!("  BACKUP AND RESET seed {}", entry.target.display());
                } else if entry.target.exists() {
                    println!("  preserve existing seed {}", entry.target.display());
                } else {
                    println!("  initialize seed {}", entry.target.display());
                }
            }
            Materialization::ManagedCache { .. } => unreachable!("cache entry handled above"),
        }
    }
    if !printed {
        println!("Configuration: no file changes required");
    }
}

fn run_profile_operation(
    profiles: &std::collections::BTreeMap<String, Profile>,
    target: &str,
    mode: ActivationMode,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    let profile = profiles
        .get(target)
        .with_context(|| format!("unknown profile {target}"))?;
    let home = runtime::home()?;
    let state_file = runtime::state_file()?;
    recover_activation_journals(&state_file)?;
    let _lock = lock(&state_lock_path(&state_file)?)?;
    let old = read_state(&state_file)?;
    validate_user_state(&old, profiles)?;
    let mut prospective = old.clone();

    match mode {
        ActivationMode::Select => {
            let inserted = retain_profile(&mut prospective, profile)?;
            if !inserted && old.active_profile.as_deref() != Some(target) {
                prospective.generation += 1;
            }
        }
        ActivationMode::Update => {
            if !old.profiles.contains_key(target) {
                bail!("profile {target} is not retained")
            }
            prospective.generation += 1;
        }
        ActivationMode::Reset => {
            if old.active_profile.as_deref() != Some(target) {
                bail!("profile {target} is not active")
            }
            prospective.generation += 1;
        }
    }
    prepare_profile_state(&mut prospective, profile, mode)?;

    let registry_path = managed_targets_path(&state_file)?;
    let plan =
        build_activation_preview(profiles, &prospective, target, &home, &registry_path, mode)?;
    let preview_state = PreviewState::create(&prospective)?;
    let package = package_preview(&state_file, &prospective, &preview_state)?;

    println!("Profile: {target}");
    print_package_plan(&package);
    print_activation_plan(&plan);
    let package_changes = !package.plan.install.is_empty()
        || !package.plan.remove.is_empty()
        || !package.plan.replacements.is_empty()
        || package.system_update_required;
    if !plan.has_changes() && !package_changes {
        println!("No changes are required.");
        return Ok(());
    }
    if dry_run {
        return Ok(());
    }
    confirm(yes)?;

    apply_package_plan(
        &state_file,
        prospective.generation,
        &package.plan,
        &preview_state,
    )?;

    let verified = match build_activation_plan(
        profiles,
        &prospective,
        target,
        &home,
        &registry_path,
        mode,
    ) {
        Ok(plan) => plan,
        Err(error) => {
            let reconcile = synchronize_package_record(&state_file, &old);
            return match reconcile {
                Ok(()) => Err(error.into()),
                Err(reconcile) => Err(anyhow::anyhow!(
                    "configuration planning failed: {error}; package ownership rollback also failed: {reconcile:#}"
                )),
            };
        }
    };
    if verified.identity_digest() != plan.identity_digest() {
        let reconcile = synchronize_package_record(&state_file, &old);
        return match reconcile {
            Ok(()) => Err(anyhow::anyhow!(
                "configuration plan changed after confirmation; run the command again"
            )),
            Err(reconcile) => Err(anyhow::anyhow!(
                "configuration plan changed after confirmation; package ownership rollback also failed: {reconcile:#}"
            )),
        };
    }

    let mut final_state = prospective.clone();
    verified.record_applied_state(&mut final_state)?;
    if mode != ActivationMode::Update {
        final_state.active_profile = Some(target.to_owned());
    }
    let mut journal = match ActivationJournal::begin(&state_file, old.clone(), final_state.clone())
    {
        Ok(journal) => journal,
        Err(error) => {
            let reconcile = synchronize_package_record(&state_file, &old);
            return match reconcile {
                Ok(()) => Err(error.into()),
                Err(reconcile) => Err(anyhow::anyhow!(
                    "cannot create activation journal: {error}; package ownership rollback also failed: {reconcile:#}"
                )),
            };
        }
    };
    let activation = journal
        .mark_applying()
        .and_then(|_| activate_configuration(&verified, &registry_path, &mut journal))
        .and_then(|_| write_state(&state_file, &final_state))
        .and_then(|_| journal.mark_state_written());
    if let Err(error) = activation {
        let rollback = journal.rollback();
        if let Err(rollback) = rollback {
            return Err(anyhow::anyhow!(
                "activation failed: {error}; rollback also failed: {rollback}"
            ));
        }
        let reconcile = synchronize_package_record(&state_file, &old);
        return match reconcile {
            Ok(()) => Err(error.into()),
            Err(reconcile) => Err(anyhow::anyhow!(
                "activation failed: {error}; package ownership rollback also failed: {reconcile:#}"
            )),
        };
    }
    journal.complete()?;
    if final_state.active_profile.as_deref() == Some(target) {
        println!("Profile {target} is active.");
    } else {
        println!("Profile {target} was updated without changing the active profile.");
    }
    Ok(())
}

fn remove_profile_command(
    profiles: &std::collections::BTreeMap<String, Profile>,
    target: &str,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    let state_file = runtime::state_file()?;
    recover_activation_journals(&state_file)?;
    let _lock = lock(&state_lock_path(&state_file)?)?;
    let old = read_state(&state_file)?;
    validate_user_state(&old, profiles)?;
    if !old.profiles.contains_key(target) {
        bail!("profile {target} is not retained")
    }
    let mut prospective = old.clone();
    remove_profile(&mut prospective, target)?;
    let preview_state = PreviewState::create(&prospective)?;
    let package = package_preview(&state_file, &prospective, &preview_state)?;
    println!("Forget retained profile {target}.");
    print_package_plan(&package);
    println!("Seed files in HOME are preserved. Package removal requires catdot prune.");
    if dry_run {
        return Ok(());
    }
    confirm(yes)?;
    apply_package_plan(
        &state_file,
        prospective.generation,
        &package.plan,
        &preview_state,
    )?;
    if let Err(error) = write_state(&state_file, &prospective) {
        let reconcile = synchronize_package_record(&state_file, &old);
        return match reconcile {
            Ok(()) => Err(error.into()),
            Err(reconcile) => Err(anyhow::anyhow!(
                "cannot commit profile removal: {error}; package ownership rollback also failed: {reconcile:#}"
            )),
        };
    }
    let cache = profile_managed_cache(&managed_targets_path(&state_file)?, target)?;
    match fs::remove_dir_all(&cache) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => eprintln!(
            "warning: could not remove cached managed revision {}: {error}",
            cache.display()
        ),
    }
    println!("Profile {target} was forgotten.");
    Ok(())
}

fn prune(dry_run: bool, yes: bool) -> Result<()> {
    let query = vec!["prune-plan".into(), "--uid".into(), uid().to_string()];
    let output = pkexec(QUERY_HELPER, &query, "prune plan")?;
    let preview: PackagePlanPreview = toml::from_str(&String::from_utf8(output.stdout)?)?;
    if preview.plan.remove.is_empty() {
        println!("No packages are eligible for Catdot prune.");
        if !preview.system_update_required {
            return Ok(());
        }
        println!("Pending package transaction metadata will be recovered.");
    } else {
        println!("Remove packages:");
        for package in &preview.plan.remove {
            println!("  {package}");
        }
    }
    if dry_run {
        return Ok(());
    }
    confirm(yes)?;
    let state = read_state(&runtime::state_file()?)?;
    let arguments = vec![
        "prune".into(),
        "--uid".into(),
        uid().to_string(),
        "--generation".into(),
        state.generation.to_string(),
        "--digest".into(),
        preview.plan.digest(),
    ];
    pkexec(MANAGE_HELPER, &arguments, "package prune")?;
    println!("Unused Catdot packages were removed.");
    Ok(())
}

fn recover(command: RecoverCmd) -> Result<()> {
    match command {
        RecoverCmd::List => {
            let args = vec!["recovery-list".into(), "--uid".into(), uid().to_string()];
            let output = pkexec(QUERY_HELPER, &args, "package recovery list")?;
            print!("{}", String::from_utf8_lossy(&output.stdout));
        }
        RecoverCmd::Accept { transaction, yes } => {
            confirm(yes)?;
            let args = vec![
                "recovery-accept".into(),
                "--uid".into(),
                uid().to_string(),
                "--transaction".into(),
                transaction,
            ];
            pkexec(MANAGE_HELPER, &args, "package recovery")?;
        }
        RecoverCmd::Discard { transaction, yes } => {
            confirm(yes)?;
            let args = vec![
                "recovery-discard".into(),
                "--uid".into(),
                uid().to_string(),
                "--transaction".into(),
                transaction,
            ];
            pkexec(MANAGE_HELPER, &args, "package recovery")?;
        }
    }
    Ok(())
}

fn doctor(profiles: &std::collections::BTreeMap<String, Profile>) -> Result<i32> {
    let state_file = runtime::state_file()?;
    let mut errors = 0;
    if let Err(error) = recover_activation_journals(&state_file) {
        eprintln!("error: activation recovery: {error}");
        errors += 1;
    }
    match read_state(&state_file).and_then(|state| validate_user_state(&state, profiles)) {
        Ok(()) => println!("user state: valid"),
        Err(error) => {
            eprintln!("error: user state: {error}");
            errors += 1;
        }
    }
    let args = vec!["doctor-system".into(), "--uid".into(), uid().to_string()];
    match pkexec(QUERY_HELPER, &args, "system doctor").and_then(|output| {
        let text = String::from_utf8(output.stdout)?;
        toml::from_str::<SystemDoctorReport>(&text).map_err(Into::into)
    }) {
        Ok(report) => {
            for line in report.lines {
                println!("{line}");
            }
            errors += report.errors;
        }
        Err(error) => {
            eprintln!("error: system doctor: {error:#}");
            errors += 1;
        }
    }
    Ok(if errors == 0 { 0 } else { 2 })
}

fn print_profile(profile: &Profile, retained: bool, active: bool) {
    let marker = if active {
        "active"
    } else if retained {
        "retained"
    } else {
        "available"
    };
    println!("{} ({marker})", profile.id);
    println!("  {}", profile.name);
    if !profile.description.is_empty() {
        println!("  {}", profile.description);
    }
    if !profile.packages.is_empty() {
        println!("  packages: {}", profile.packages.join(", "));
    }
    if !profile.manage.is_empty() {
        println!(
            "  managed: {}",
            profile
                .manage
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

pub fn run() -> Result<i32> {
    let command = Cli::parse().command;
    if let Cmd::Validate { profile_root } = command {
        return validate_profile_root(&profile_root);
    }

    let profiles = installed_profiles()?;
    match command {
        Cmd::Validate { .. } => unreachable!(),
        Cmd::List => {
            let state = read_state(&runtime::state_file()?)?;
            for profile in profiles.values() {
                print_profile(
                    profile,
                    state.profiles.contains_key(&profile.id),
                    state.active_profile.as_deref() == Some(&profile.id),
                );
            }
        }
        Cmd::Show { profile } => {
            let state = read_state(&runtime::state_file()?)?;
            let profile = profiles
                .get(&profile)
                .with_context(|| format!("unknown profile {profile}"))?;
            print_profile(
                profile,
                state.profiles.contains_key(&profile.id),
                state.active_profile.as_deref() == Some(&profile.id),
            );
        }
        Cmd::Current => {
            let state = read_state(&runtime::state_file()?)?;
            println!(
                "Active profile: {}",
                state.active_profile.as_deref().unwrap_or("none")
            );
            if state.profiles.is_empty() {
                println!("Retained profiles: none");
            } else {
                println!(
                    "Retained profiles: {}",
                    state
                        .profiles
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        Cmd::Select {
            profile,
            dry_run,
            yes,
        } => run_profile_operation(&profiles, &profile, ActivationMode::Select, dry_run, yes)?,
        Cmd::Update {
            profile,
            dry_run,
            yes,
        } => {
            let selected = match profile {
                Some(profile) => profile,
                None => read_state(&runtime::state_file()?)?
                    .active_profile
                    .context("no active profile")?,
            };
            run_profile_operation(&profiles, &selected, ActivationMode::Update, dry_run, yes)?;
        }
        Cmd::Reset {
            profile,
            dry_run,
            yes,
        } => run_profile_operation(&profiles, &profile, ActivationMode::Reset, dry_run, yes)?,
        Cmd::Remove {
            profile,
            dry_run,
            yes,
        } => remove_profile_command(&profiles, &profile, dry_run, yes)?,
        Cmd::Prune { dry_run, yes } => prune(dry_run, yes)?,
        Cmd::Recover { command } => recover(command)?,
        Cmd::Doctor => return doctor(&profiles),
    }
    Ok(0)
}

use crate::system::{user_record_path, write_system_file};
use anyhow::{Context, Result, bail};
use catdot_core::{PackagePlan, SystemPackageState, UserRecord};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum JournalStage {
    Prepared,
    AlpmCommitted,
    RecordsPrepared,
    RecordsCommitted,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum PruneJournalStage {
    Prepared,
    AlpmCommitted,
    RecordsCommitted,
    Complete,
}

#[derive(Debug, Deserialize)]
struct JournalHeader {
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PruneJournal {
    kind: String,
    id: String,
    plan_digest: String,
    removed_packages: BTreeSet<String>,
    expected_packages: SystemPackageState,
    stage: PruneJournalStage,
    #[serde(skip)]
    path: PathBuf,
}

impl PruneJournal {
    pub(super) fn prepared(
        database: &Path,
        plan: &PackagePlan,
        expected_packages: SystemPackageState,
    ) -> Result<Self> {
        let id = journal_id(0, 0)?;
        let path = journal_directory(database)?.join(format!("prune-{id}.toml"));
        let journal = Self {
            kind: "prune".into(),
            id,
            plan_digest: plan.digest(),
            removed_packages: plan.remove.iter().cloned().collect(),
            expected_packages,
            stage: PruneJournalStage::Prepared,
            path,
        };
        journal.persist()?;
        Ok(journal)
    }

    pub(super) fn verify(&self, digest: &str) -> Result<()> {
        if self.plan_digest != digest {
            bail!("prune journal does not match the confirmed plan")
        }
        Ok(())
    }

    pub(super) fn mark_alpm_committed(&mut self) -> Result<()> {
        self.stage = PruneJournalStage::AlpmCommitted;
        self.persist()
    }

    pub(super) fn mark_records_committed(&mut self) -> Result<()> {
        self.stage = PruneJournalStage::RecordsCommitted;
        self.persist()
    }

    pub(super) fn expected_packages(&self) -> &SystemPackageState {
        &self.expected_packages
    }

    pub(super) fn complete(mut self) -> Result<()> {
        self.stage = PruneJournalStage::Complete;
        self.persist()?;
        fs::remove_file(&self.path).with_context(|| format!("remove {}", self.path.display()))?;
        sync_directory(
            self.path
                .parent()
                .context("prune journal has no parent directory")?,
        )
    }

    fn persist(&self) -> Result<()> {
        write_system_file(&self.path, &toml::to_string_pretty(self)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PackageJournal {
    id: String,
    plan_digest: String,
    #[serde(default)]
    recovery_schema: u32,
    uid: u32,
    generation: u64,
    direct_requirements: BTreeMap<String, BTreeSet<String>>,
    transaction_packages: BTreeSet<String>,
    previously_present: BTreeSet<String>,
    #[serde(default)]
    removed_packages: BTreeSet<String>,
    expected_record: UserRecord,
    expected_packages: SystemPackageState,
    stage: JournalStage,
    #[serde(skip)]
    path: PathBuf,
}

pub(super) struct PreparedTransaction {
    pub uid: u32,
    pub generation: u64,
    pub direct_requirements: BTreeMap<String, BTreeSet<String>>,
    pub transaction_packages: BTreeSet<String>,
    pub previously_present: BTreeSet<String>,
    pub expected_record: UserRecord,
    pub expected_packages: SystemPackageState,
}

impl PackageJournal {
    pub(super) fn prepared(
        database: &Path,
        plan: &PackagePlan,
        input: PreparedTransaction,
    ) -> Result<Self> {
        let id = journal_id(input.uid, input.generation)?;
        let mut expected_packages = input.expected_packages;
        for package in &input.transaction_packages {
            if !input.previously_present.contains(package)
                && let Some(managed) = expected_packages.packages.get_mut(package)
            {
                managed.introduced_by_transaction = Some(id.clone());
            }
        }
        let path = journal_directory(database)?.join(format!("{id}.toml"));
        let journal = Self {
            id,
            plan_digest: plan.digest(),
            recovery_schema: 1,
            uid: input.uid,
            generation: input.generation,
            direct_requirements: input.direct_requirements,
            transaction_packages: input.transaction_packages,
            previously_present: input.previously_present,
            removed_packages: plan.remove.iter().cloned().collect(),
            expected_record: input.expected_record,
            expected_packages,
            stage: JournalStage::Prepared,
            path,
        };
        journal.persist()?;
        Ok(journal)
    }

    pub(super) fn verify(&self, uid: u32, generation: u64, digest: &str) -> Result<()> {
        if self.uid != uid || self.generation != generation || self.plan_digest != digest {
            bail!("package transaction journal does not match the requested plan")
        }
        Ok(())
    }

    pub(super) fn mark_alpm_committed(&mut self) -> Result<()> {
        self.stage = JournalStage::AlpmCommitted;
        self.persist()
    }

    pub(super) fn mark_records_committed(&mut self) -> Result<()> {
        self.stage = JournalStage::RecordsCommitted;
        self.persist()
    }

    pub(super) fn expected_record(&self) -> &UserRecord {
        &self.expected_record
    }

    pub(super) fn expected_packages(&self) -> &SystemPackageState {
        &self.expected_packages
    }

    pub(super) fn complete(mut self) -> Result<()> {
        self.stage = JournalStage::Complete;
        self.persist()?;
        fs::remove_file(&self.path).with_context(|| format!("remove {}", self.path.display()))?;
        sync_directory(
            self.path
                .parent()
                .context("package journal has no parent directory")?,
        )
    }

    fn persist(&self) -> Result<()> {
        write_system_file(&self.path, &toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryDecision {
    Accept,
    Discard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecoveryStatus {
    pub id: String,
    pub uid: u32,
    pub generation: u64,
    pub metadata_complete: bool,
    pub accept_safe: bool,
    pub discard_safe: bool,
    pub installed: BTreeSet<String>,
    pub missing: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    pub still_present: BTreeSet<String>,
}

fn read_package_journals(database: &Path) -> Result<Vec<PackageJournal>> {
    let directory = journal_directory(database)?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut journals = Vec::new();
    for entry in
        fs::read_dir(&directory).with_context(|| format!("read {}", directory.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
            continue;
        }
        let contents =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let header: JournalHeader =
            toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;
        if header.kind.as_deref() == Some("prune") {
            continue;
        }
        let mut journal: PackageJournal =
            toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;
        journal.path = path;
        journals.push(journal);
    }
    Ok(journals)
}

fn status_for_journal<F>(journal: &PackageJournal, package_present: &mut F) -> RecoveryStatus
where
    F: FnMut(&str) -> bool,
{
    let mut installed = BTreeSet::new();
    let mut missing = BTreeSet::new();
    for package in &journal.transaction_packages {
        if package_present(package) {
            installed.insert(package.clone());
        } else {
            missing.insert(package.clone());
        }
    }
    let mut removed = BTreeSet::new();
    let mut still_present = BTreeSet::new();
    for package in &journal.removed_packages {
        if package_present(package) {
            still_present.insert(package.clone());
        } else {
            removed.insert(package.clone());
        }
    }
    let newly_expected = journal
        .transaction_packages
        .difference(&journal.previously_present)
        .cloned()
        .collect::<BTreeSet<_>>();
    let metadata_complete = journal.recovery_schema == 1;
    let accept_safe = metadata_complete && missing.is_empty() && still_present.is_empty();
    let discard_safe =
        metadata_complete && newly_expected.is_disjoint(&installed) && removed.is_empty();
    RecoveryStatus {
        id: journal.id.clone(),
        uid: journal.uid,
        generation: journal.generation,
        metadata_complete,
        accept_safe,
        discard_safe,
        installed,
        missing,
        removed,
        still_present,
    }
}

#[cfg(test)]
pub(super) fn recovery_status<F>(
    database: &Path,
    id: &str,
    mut package_present: F,
) -> Result<RecoveryStatus>
where
    F: FnMut(&str) -> bool,
{
    let journal = read_package_journals(database)?
        .into_iter()
        .find(|journal| journal.id == id)
        .ok_or_else(|| anyhow::anyhow!("unknown package transaction {id}"))?;
    if journal.stage != JournalStage::Prepared {
        bail!("package transaction {id} does not require an explicit recovery decision")
    }
    Ok(status_for_journal(&journal, &mut package_present))
}

pub(super) fn recovery_statuses<F>(
    database: &Path,
    mut package_present: F,
) -> Result<Vec<RecoveryStatus>>
where
    F: FnMut(&str) -> bool,
{
    let mut statuses = read_package_journals(database)?
        .into_iter()
        .filter(|journal| journal.stage == JournalStage::Prepared)
        .map(|journal| status_for_journal(&journal, &mut package_present))
        .collect::<Vec<_>>();
    statuses.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(statuses)
}

pub(super) fn recover_transaction<F>(
    database: &Path,
    id: &str,
    decision: RecoveryDecision,
    mut package_present: F,
) -> Result<()>
where
    F: FnMut(&str) -> bool,
{
    let mut journal = read_package_journals(database)?
        .into_iter()
        .find(|journal| journal.id == id)
        .ok_or_else(|| anyhow::anyhow!("unknown package transaction {id}"))?;
    if journal.stage != JournalStage::Prepared {
        bail!("package transaction {id} does not require an explicit recovery decision")
    }
    let status = status_for_journal(&journal, &mut package_present);
    match decision {
        RecoveryDecision::Accept if status.accept_safe => {
            journal.mark_alpm_committed()?;
            commit_records(
                database,
                &journal.expected_packages,
                &journal.expected_record,
            )?;
            journal.mark_records_committed()?;
            journal.complete()
        }
        RecoveryDecision::Discard if status.discard_safe => journal.complete(),
        RecoveryDecision::Accept => bail!(
            "cannot accept transaction {id}: installed/missing or replacement state is incomplete"
        ),
        RecoveryDecision::Discard => bail!(
            "cannot discard transaction {id}: package state shows that the transaction may have committed"
        ),
    }
}

pub(super) fn recover_pending<F>(database: &Path, mut package_present: F) -> Result<()>
where
    F: FnMut(&str) -> bool,
{
    let directory = journal_directory(database)?;
    if !directory.exists() {
        return Ok(());
    }
    for entry in
        fs::read_dir(&directory).with_context(|| format!("read {}", directory.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
            continue;
        }
        let contents =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let header: JournalHeader =
            toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;
        if header.kind.as_deref() == Some("prune") {
            let mut journal: PruneJournal =
                toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;
            journal.path = path;
            recover_prune_journal(journal, &mut package_present)?;
            continue;
        }
        let mut journal: PackageJournal =
            toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;
        journal.path = path;
        match journal.stage {
            JournalStage::Prepared => {
                let status = status_for_journal(&journal, &mut package_present);
                if status.discard_safe {
                    journal.complete()?;
                } else {
                    bail!(
                        "prepared package journal {} has an uncertain ALPM result; run: catdot recover list",
                        journal.id
                    )
                }
            }
            JournalStage::AlpmCommitted | JournalStage::RecordsPrepared => {
                if journal
                    .transaction_packages
                    .iter()
                    .any(|package| !package_present(package))
                {
                    bail!(
                        "committed package journal {} is missing installed packages",
                        journal.id
                    )
                }
                commit_records(
                    database,
                    &journal.expected_packages,
                    &journal.expected_record,
                )?;
                journal.mark_records_committed()?;
                journal.complete()?;
            }
            JournalStage::RecordsCommitted | JournalStage::Complete => journal.complete()?,
        }
    }
    Ok(())
}

fn recover_prune_journal<F>(mut journal: PruneJournal, package_present: &mut F) -> Result<()>
where
    F: FnMut(&str) -> bool,
{
    let present = journal
        .removed_packages
        .iter()
        .filter(|package| package_present(package))
        .count();
    match journal.stage {
        PruneJournalStage::Prepared if present == journal.removed_packages.len() => {
            journal.complete()?;
        }
        PruneJournalStage::Prepared | PruneJournalStage::AlpmCommitted if present == 0 => {
            let database = journal
                .path
                .parent()
                .and_then(Path::parent)
                .context("prune journal is outside the transaction directory")?;
            write_system_file(
                &database.join("packages.toml"),
                &toml::to_string_pretty(&journal.expected_packages)?,
            )?;
            journal.mark_records_committed()?;
            journal.complete()?;
        }
        PruneJournalStage::Prepared | PruneJournalStage::AlpmCommitted => {
            bail!(
                "prune journal {} has a partial or uncertain ALPM result",
                journal.id
            )
        }
        PruneJournalStage::RecordsCommitted | PruneJournalStage::Complete => {
            journal.complete()?;
        }
    }
    Ok(())
}

fn journal_id(uid: u32, generation: u64) -> Result<String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("read system clock for package transaction journal")?
        .as_nanos();
    Ok(format!("{uid}-{generation}-{stamp}"))
}

pub(super) fn commit_records(
    database: &Path,
    packages: &SystemPackageState,
    record: &UserRecord,
) -> Result<()> {
    let package_path = database.join("packages.toml");
    let record_path = user_record_path(database, record.uid);
    let previous_packages = fs::read_to_string(&package_path).ok();
    write_system_file(&package_path, &toml::to_string_pretty(packages)?)?;
    if let Err(error) = write_system_file(&record_path, &toml::to_string_pretty(record)?) {
        match previous_packages {
            Some(contents) => write_system_file(&package_path, &contents)?,
            None if package_path.exists() => fs::remove_file(&package_path)?,
            None => {}
        }
        return Err(error);
    }
    Ok(())
}

fn journal_directory(database: &Path) -> Result<PathBuf> {
    Ok(database.join("transactions"))
}

fn sync_directory(directory: &Path) -> Result<()> {
    fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("sync {}", directory.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        PackageJournal, PreparedTransaction, PruneJournal, RecoveryDecision, recover_pending,
        recover_transaction, recovery_status,
    };
    use catdot_core::{InstallReason, ManagedPackage, PackagePlan, SystemPackageState, UserRecord};
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        path::PathBuf,
    };
    use tempfile::tempdir;

    fn record() -> UserRecord {
        UserRecord {
            uid: 1000,
            generation: 2,
            state_path: PathBuf::from("/home/test/.local/state/catdot/state.toml"),
            profiles: BTreeSet::new(),
            requirements: BTreeMap::new(),
        }
    }

    fn packages() -> SystemPackageState {
        SystemPackageState {
            packages: [(
                "dependency".into(),
                ManagedPackage {
                    name: "dependency".into(),
                    catdot_installed: true,
                    was_missing_before_catdot: true,
                    install_reason: InstallReason::Dependency,
                    introduced_by_transaction: Some("tx".into()),
                    references: vec![],
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    fn plan() -> PackagePlan {
        PackagePlan {
            install: vec!["dependency".into()],
            remove: vec![],
            replacements: vec![],
            satisfied: vec![],
        }
    }

    #[test]
    fn committed_journal_restores_records_after_a_write_failure() {
        let directory = tempdir().unwrap();
        let mut journal = PackageJournal::prepared(
            directory.path(),
            &plan(),
            PreparedTransaction {
                uid: 1000,
                generation: 2,
                direct_requirements: BTreeMap::new(),
                transaction_packages: ["dependency".into()].into_iter().collect(),
                previously_present: BTreeSet::new(),
                expected_record: record(),
                expected_packages: packages(),
            },
        )
        .unwrap();
        journal.mark_alpm_committed().unwrap();

        recover_pending(directory.path(), |name| name == "dependency").unwrap();

        assert!(directory.path().join("packages.toml").exists());
        assert!(directory.path().join("users/1000.toml").exists());
        assert_eq!(
            fs::read_dir(directory.path().join("transactions"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn uncommitted_journal_is_cancelled_only_when_no_new_package_exists() {
        let directory = tempdir().unwrap();
        PackageJournal::prepared(
            directory.path(),
            &plan(),
            PreparedTransaction {
                uid: 1000,
                generation: 2,
                direct_requirements: BTreeMap::new(),
                transaction_packages: ["dependency".into()].into_iter().collect(),
                previously_present: BTreeSet::new(),
                expected_record: record(),
                expected_packages: packages(),
            },
        )
        .unwrap();

        recover_pending(directory.path(), |_| false).unwrap();

        assert_eq!(
            fs::read_dir(directory.path().join("transactions"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn journal_rejects_a_different_plan_identity() {
        let directory = tempdir().unwrap();
        let journal = PackageJournal::prepared(
            directory.path(),
            &plan(),
            PreparedTransaction {
                uid: 1000,
                generation: 2,
                direct_requirements: BTreeMap::new(),
                transaction_packages: ["dependency".into()].into_iter().collect(),
                previously_present: BTreeSet::new(),
                expected_record: record(),
                expected_packages: packages(),
            },
        )
        .unwrap();

        assert!(journal.verify(1000, 3, &plan().digest()).is_err());
        assert!(journal.verify(1000, 2, "different digest").is_err());
    }

    #[test]
    fn interrupted_prune_replays_the_post_removal_package_state() {
        let directory = tempdir().unwrap();
        let database = directory.path();
        let initial = packages();
        fs::write(
            database.join("packages.toml"),
            toml::to_string_pretty(&initial).unwrap(),
        )
        .unwrap();
        let expected = SystemPackageState::default();
        let prune_plan = PackagePlan {
            install: vec![],
            remove: vec!["dependency".into()],
            replacements: vec![],
            satisfied: vec![],
        };

        PruneJournal::prepared(database, &prune_plan, expected.clone()).unwrap();
        recover_pending(database, |name| name != "dependency").unwrap();

        let recovered: SystemPackageState =
            toml::from_str(&fs::read_to_string(database.join("packages.toml")).unwrap()).unwrap();
        assert!(recovered.packages.is_empty());
        assert_eq!(
            fs::read_dir(database.join("transactions")).unwrap().count(),
            0
        );
    }

    #[test]
    fn transaction_only_dependency_keeps_its_ownership_provenance() {
        let directory = tempdir().unwrap();
        let mut expected = packages();
        expected.packages.insert(
            "transitive".into(),
            ManagedPackage {
                name: "transitive".into(),
                catdot_installed: true,
                was_missing_before_catdot: true,
                install_reason: InstallReason::Dependency,
                introduced_by_transaction: None,
                references: vec![],
            },
        );
        let journal = PackageJournal::prepared(
            directory.path(),
            &plan(),
            PreparedTransaction {
                uid: 1000,
                generation: 2,
                direct_requirements: BTreeMap::new(),
                transaction_packages: ["dependency".into(), "transitive".into()]
                    .into_iter()
                    .collect(),
                previously_present: BTreeSet::new(),
                expected_record: record(),
                expected_packages: expected,
            },
        )
        .unwrap();

        let transitive = &journal.expected_packages().packages["transitive"];
        assert!(transitive.references.is_empty());
        assert!(transitive.introduced_by_transaction.is_some());
    }

    #[test]
    fn explicit_recovery_accepts_only_a_fully_observed_install_transaction() {
        let directory = tempdir().unwrap();
        let database = directory.path();
        let recovery_plan = PackagePlan {
            install: vec!["dependency".into(), "transitive".into()],
            remove: vec!["conflicting".into()],
            replacements: vec![],
            satisfied: vec![],
        };
        let mut expected = packages();
        expected.packages.insert(
            "transitive".into(),
            ManagedPackage {
                name: "transitive".into(),
                catdot_installed: true,
                was_missing_before_catdot: true,
                install_reason: InstallReason::Dependency,
                introduced_by_transaction: None,
                references: vec![],
            },
        );
        let journal = PackageJournal::prepared(
            database,
            &recovery_plan,
            PreparedTransaction {
                uid: 1000,
                generation: 2,
                direct_requirements: BTreeMap::new(),
                transaction_packages: ["dependency".into(), "transitive".into()]
                    .into_iter()
                    .collect(),
                previously_present: BTreeSet::new(),
                expected_record: record(),
                expected_packages: expected,
            },
        )
        .unwrap();
        let id = journal.id.clone();

        let partial = recovery_status(database, &id, |name| name == "dependency").unwrap();
        assert!(!partial.accept_safe);
        assert!(!partial.discard_safe);
        assert!(
            recover_transaction(database, &id, RecoveryDecision::Accept, |name| {
                name == "dependency"
            })
            .is_err()
        );

        let complete = recovery_status(database, &id, |name| {
            matches!(name, "dependency" | "transitive")
        })
        .unwrap();
        assert!(complete.accept_safe);
        assert!(!complete.discard_safe);
        recover_transaction(database, &id, RecoveryDecision::Accept, |name| {
            matches!(name, "dependency" | "transitive")
        })
        .unwrap();
        assert!(database.join("packages.toml").is_file());
        assert!(database.join("users/1000.toml").is_file());
        assert_eq!(
            fs::read_dir(database.join("transactions")).unwrap().count(),
            0
        );
    }

    #[test]
    fn explicit_recovery_discards_only_an_uncommitted_transaction() {
        let directory = tempdir().unwrap();
        let database = directory.path();
        let recovery_plan = PackagePlan {
            install: vec!["dependency".into()],
            remove: vec!["conflicting".into()],
            replacements: vec![],
            satisfied: vec![],
        };
        let journal = PackageJournal::prepared(
            database,
            &recovery_plan,
            PreparedTransaction {
                uid: 1000,
                generation: 2,
                direct_requirements: BTreeMap::new(),
                transaction_packages: ["dependency".into()].into_iter().collect(),
                previously_present: BTreeSet::new(),
                expected_record: record(),
                expected_packages: packages(),
            },
        )
        .unwrap();
        let id = journal.id.clone();
        let status = recovery_status(database, &id, |name| name == "conflicting").unwrap();
        assert!(!status.accept_safe);
        assert!(status.discard_safe);
        recover_transaction(database, &id, RecoveryDecision::Discard, |name| {
            name == "conflicting"
        })
        .unwrap();
        assert!(!database.join("packages.toml").exists());
        assert_eq!(
            fs::read_dir(database.join("transactions")).unwrap().count(),
            0
        );
    }

    #[test]
    fn automatic_recovery_refuses_a_partial_replacement_result() {
        let directory = tempdir().unwrap();
        let database = directory.path();
        let replacement_plan = PackagePlan {
            install: vec!["dependency".into()],
            remove: vec!["conflicting".into()],
            replacements: vec![],
            satisfied: vec![],
        };
        PackageJournal::prepared(
            database,
            &replacement_plan,
            PreparedTransaction {
                uid: 1000,
                generation: 2,
                direct_requirements: BTreeMap::new(),
                transaction_packages: ["dependency".into()].into_iter().collect(),
                previously_present: BTreeSet::new(),
                expected_record: record(),
                expected_packages: packages(),
            },
        )
        .unwrap();

        let error = recover_pending(database, |_| false).unwrap_err();
        assert!(error.to_string().contains("catdot recover list"));
        assert_eq!(
            fs::read_dir(database.join("transactions")).unwrap().count(),
            1
        );
    }
}

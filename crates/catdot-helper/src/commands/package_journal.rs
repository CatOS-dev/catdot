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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PackageJournal {
    id: String,
    plan_digest: String,
    uid: u32,
    generation: u64,
    direct_requirements: BTreeMap<String, BTreeSet<String>>,
    transaction_packages: BTreeSet<String>,
    previously_present: BTreeSet<String>,
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
            uid: input.uid,
            generation: input.generation,
            direct_requirements: input.direct_requirements,
            transaction_packages: input.transaction_packages,
            previously_present: input.previously_present,
            expected_record: input.expected_record,
            expected_packages,
            stage: JournalStage::Prepared,
            path,
        };
        journal.persist()?;
        Ok(journal)
    }

    pub(super) fn records_prepared(
        database: &Path,
        expected_packages: SystemPackageState,
        expected_record: UserRecord,
    ) -> Result<Self> {
        let id = journal_id(expected_record.uid, expected_record.active_generation)?;
        let path = journal_directory(database)?.join(format!("finalize-{id}.toml"));
        let journal = Self {
            id,
            plan_digest: "finalize".into(),
            uid: expected_record.uid,
            generation: expected_record.active_generation,
            direct_requirements: expected_record.active_requirements.clone(),
            transaction_packages: BTreeSet::new(),
            previously_present: BTreeSet::new(),
            expected_record,
            expected_packages,
            stage: JournalStage::RecordsPrepared,
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
        let mut journal: PackageJournal = toml::from_str(
            &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        journal.path = path;
        match journal.stage {
            JournalStage::Prepared => {
                if journal.transaction_packages.iter().any(|package| {
                    !journal.previously_present.contains(package) && package_present(package)
                }) {
                    bail!(
                        "prepared package journal {} has an uncertain ALPM result",
                        journal.id
                    )
                }
                journal.complete()?;
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
    use super::{PackageJournal, PreparedTransaction, recover_pending};
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
            pending_generation: 2,
            active_generation: 1,
            state_path: PathBuf::from("/home/test/.local/state/catdot/state.toml"),
            components: BTreeMap::new(),
            active_components: BTreeMap::new(),
            active_requirements: BTreeMap::new(),
            pending_requirements: BTreeMap::new(),
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
    fn interrupted_finalize_replays_the_matching_user_and_package_records() {
        let directory = tempdir().unwrap();
        let database = directory.path();
        let expected_record = record();
        let expected_packages = packages();

        PackageJournal::records_prepared(
            database,
            expected_packages.clone(),
            expected_record.clone(),
        )
        .unwrap();
        recover_pending(database, |_| false).unwrap();

        assert_eq!(
            fs::read_to_string(database.join("users/1000.toml")).unwrap(),
            toml::to_string_pretty(&expected_record).unwrap()
        );
        assert_eq!(
            fs::read_to_string(database.join("packages.toml")).unwrap(),
            toml::to_string_pretty(&expected_packages).unwrap()
        );
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
}

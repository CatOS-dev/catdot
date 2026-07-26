//! Pure domain and filesystem operations for Catdot.

pub mod activation;
mod error;
pub mod manifest;
pub mod planning;
pub mod state;

pub use activation::{
    ActivationJournal, ActivationMode, ActivationPlan, ManagedRegistry, ManagedTarget,
    Materialization, PlannedTarget, activate_configuration, activation_transactions_path,
    build_activation_plan, build_activation_preview, lock, managed_targets_path,
    profile_managed_cache, read_managed_registry, recover_activation_journals,
};
pub use error::{Error, Result};
pub use manifest::{
    DEFAULT_PROFILE_ROOT, PROFILE_SCHEMA, Profile, ProfileDiagnostic, ProfileDiagnosticKind,
    ProfileRegistry, discover_profile_registry, discover_profiles, profile_root,
};
pub use planning::{
    InstallReason, ManagedPackage, PackageAvailability, PackageBackend, PackagePlan,
    PackagePlanPreview, PackageReference, PackageReplacement, Requirement, SystemDoctorReport,
    SystemPackageState, UserRecord, aggregate_packages, aggregate_requirements, install_plan,
    packages_for_state, prunable,
};
pub use state::{
    ProfileState, USER_STATE_SCHEMA, UserState, atomic_write, parse_state_text,
    prepare_profile_state, read_state, read_system_packages, remove_profile, retain_profile,
    state_lock_path, state_path, validate_user_state, write_state,
};

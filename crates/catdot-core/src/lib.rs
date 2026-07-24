//! Pure domain and filesystem operations for Catdot.

pub mod activation;
mod error;
pub mod manifest;
pub mod planning;
pub mod state;
pub mod theme;

pub use activation::{
    ActivationJournal, LinkRegistry, LinkTransaction, activate_managed_link,
    activation_transactions_path, deactivate_managed_link, lock, read_link_registry,
    reconcile_managed_links, recover_activation_journals,
};
pub use error::{Error, Result};
pub use manifest::{
    ComponentDef, DEFAULT_PROFILE_ROOT, Link, Profile, ProfileDiagnostic, ProfileDiagnosticKind,
    ProfileRegistry, discover_profile_registry, discover_profiles, profile_root,
};
pub use planning::{
    InstallReason, ManagedPackage, PackageAvailability, PackageBackend, PackagePlan,
    PackagePlanPreview, PackageReference, PackageReplacement, Requirement, SystemDoctorReport,
    SystemPackageState, UserRecord, aggregate_packages, aggregate_requirements, expand_exec,
    install_plan, packages_for_state, prunable,
};
pub use state::{
    UserState, atomic_write, managed_links_path, parse_state_text, read_state,
    read_system_packages, read_user_records, select_component, select_profile, state_lock_path,
    state_path, validate_user_state, write_state, write_system_packages,
};
pub use theme::{apply_theme, merge_ini, theme_expected_files};

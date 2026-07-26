//! Pure domain and filesystem operations for Catdot.

pub mod activation;
mod error;
pub mod manifest;
pub mod state;

pub use activation::{
    ActivationMode, ActivationPlan, PlannedWrite, apply_activation_plan, build_activation_plan,
    cache_profile_content, lock, profile_cache_path,
};
pub use error::{Error, Result};
pub use manifest::{
    DEFAULT_PROFILE_ROOT, PROFILE_SCHEMA, Profile, ProfileDiagnostic, ProfileDiagnosticKind,
    ProfileRegistry, discover_profile_registry, discover_profiles, profile_root,
};
pub use state::{
    ProfileState, USER_STATE_SCHEMA, UserState, atomic_write, parse_state_text, prune_candidates,
    read_state, remove_profile, retained_packages, state_lock_path, state_path,
    validate_user_state, write_state,
};

mod schema;

pub use schema::{
    DEFAULT_PROFILE_ROOT, PROFILE_SCHEMA, Profile, ProfileDiagnostic, ProfileDiagnosticKind,
    ProfileRegistry, discover_profile_registry, discover_profiles, profile_root,
};

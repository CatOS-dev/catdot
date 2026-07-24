mod schema;

pub use schema::{
    ComponentDef, DEFAULT_PROFILE_ROOT, Link, Profile, ProfileDiagnostic, ProfileDiagnosticKind,
    ProfileRegistry, discover_profile_registry, discover_profiles, profile_root,
};

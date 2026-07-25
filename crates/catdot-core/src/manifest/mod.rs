mod schema;

pub use schema::{
    ComponentDef, ConfigurationEntry, DEFAULT_PROFILE_ROOT, Lifecycle, Profile, ProfileDiagnostic,
    ProfileDiagnosticKind, ProfileRegistry, XdgProvider, discover_profile_registry,
    discover_profiles, profile_root,
};

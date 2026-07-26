mod locking;
mod materialize;

pub use locking::lock;
pub use materialize::{
    ActivationMode, ActivationPlan, PlannedWrite, apply_activation_plan, build_activation_plan,
    cache_profile_content, profile_cache_path,
};

mod libalpm;

pub use libalpm::{
    install_with_alpm, open_handle, prepared_install_plan, prepared_removal_plan,
    removable_with_alpm, remove_with_alpm, satisfier_name,
};

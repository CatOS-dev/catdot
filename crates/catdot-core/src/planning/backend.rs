use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageAvailability {
    Installed,
    Available,
    Unavailable,
}

pub trait PackageBackend {
    fn availability(&self, package: &str) -> Result<PackageAvailability>;
    fn can_remove(&self, package: &str) -> Result<bool>;
}

pub fn require_available(availability: PackageAvailability, package: &str) -> Result<bool> {
    match availability {
        PackageAvailability::Installed => Ok(false),
        PackageAvailability::Available => Ok(true),
        PackageAvailability::Unavailable => Err(Error::Message(format!(
            "package {package} is unavailable in configured repositories"
        ))),
    }
}

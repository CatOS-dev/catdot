use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error("I/O at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid manifest {path}: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io<T>(path: &Path, result: std::io::Result<T>) -> Result<T> {
    result.map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })
}

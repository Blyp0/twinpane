use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("destination already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("not found: {0}")]
    NotFound(PathBuf),
    #[error("refusing to copy or move {0} into itself")]
    IntoSelf(PathBuf),
    #[error("invalid name {0}")]
    InvalidName(String),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("undo failed: {0}")]
    Undo(String),
    #[error("invalid pattern: {0}")]
    Pattern(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io_err(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

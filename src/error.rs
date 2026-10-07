use std::io;
use std::path::PathBuf;

use joule_profiler_core::error::BoxError;
use pyo3::PyErr;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use thiserror::Error;

use crate::ipc::IpcError;

#[derive(Debug, Error)]
pub enum Error {
    #[error("reading {0}: {1}")]
    Read(PathBuf, io::Error),

    #[error("parsing {0}: {1}")]
    Parse(PathBuf, toml::de::Error),

    #[error("`{0}` is not a key")]
    Key(String),

    #[error("`{0}` holds a value, not a table")]
    NotATable(String),

    #[error("values are booleans, numbers, strings, lists or dicts: {0}")]
    Values(serde_json::Error),

    #[error("invalid configuration: {0}")]
    Config(toml::de::Error),

    #[error("unknown source `{name}`, the sources are: {known}")]
    UnknownSource { name: String, known: String },

    #[error("invalid [sources.{0}] table: {1}")]
    SourceTable(&'static str, toml::de::Error),

    #[error("source `{0}` is not available: {1}")]
    Unavailable(&'static str, BoxError),

    #[error("the session is over")]
    SessionOver,

    #[error("the profiler stopped without a summary")]
    Stopped,

    #[error("no command to start the profiler with")]
    NoCommand,

    #[error(transparent)]
    Profiler(#[from] joule_profiler_core::error::Error),

    #[error(transparent)]
    Command(#[from] joule_profiler_injector_stdout::StdoutError),

    #[error(transparent)]
    Session(#[from] IpcError),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<Error> for PyErr {
    fn from(error: Error) -> Self {
        let message = error.to_string();
        match error {
            Error::Read(..)
            | Error::Parse(..)
            | Error::Key(_)
            | Error::NotATable(_)
            | Error::Values(_)
            | Error::Config(_)
            | Error::UnknownSource { .. } => PyValueError::new_err(message),
            Error::SourceTable(..)
            | Error::Unavailable(..)
            | Error::SessionOver
            | Error::Stopped
            | Error::NoCommand
            | Error::Profiler(_)
            | Error::Command(_)
            | Error::Session(_) => PyRuntimeError::new_err(message),
        }
    }
}

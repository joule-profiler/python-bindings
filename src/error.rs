use joule_profiler_config::ConfigError;
use pyo3::PyErr;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use thiserror::Error;

use crate::ipc::IpcError;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("values are booleans, numbers, strings, lists or dicts")]
    Values(#[source] serde_json::Error),

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error("the session is over")]
    SessionOver,

    #[error("the profiler stopped without a summary")]
    Stopped,

    #[error("no command to start the profiler with")]
    NoCommand,

    #[error(transparent)]
    Profiler(#[from] joule_profiler_core::Error),

    #[error(transparent)]
    Command(#[from] joule_profiler_injector_stdout::StdoutError),

    #[error(transparent)]
    Session(#[from] IpcError),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<Error> for PyErr {
    fn from(error: Error) -> Self {
        let message = chain(&error);
        match error {
            Error::Values(_)
            | Error::Config(
                ConfigError::Read(..)
                | ConfigError::Parse(..)
                | ConfigError::UnknownFormat(_)
                | ConfigError::Definition(_)
                | ConfigError::Key(_)
                | ConfigError::NotATable(_)
                | ConfigError::Invalid(_)
                | ConfigError::UnknownSource { .. },
            ) => PyValueError::new_err(message),
            Error::Config(ConfigError::SourceTable(..) | ConfigError::Unavailable(..))
            | Error::SessionOver
            | Error::Stopped
            | Error::NoCommand
            | Error::Profiler(_)
            | Error::Command(_)
            | Error::Session(_) => PyRuntimeError::new_err(message),
        }
    }
}

/// Returns the message of an error followed by its causes, as `error: cause: cause`.
pub(crate) fn chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut cause = error.source();
    while let Some(error) = cause {
        message.push_str(": ");
        message.push_str(&error.to_string());
        cause = error.source();
    }
    message
}

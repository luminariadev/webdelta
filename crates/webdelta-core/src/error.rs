use thiserror::Error;

/// Unified error type for `webdelta-core`.
#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("http error: {0}")]
    Http(String),

    #[error("extraction error: {0}")]
    Extract(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("notification error: {0}")]
    Notify(String),

    #[error("rule evaluation error: {0}")]
    Rule(String),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience result alias using `webdelta_core::Error`.
pub type Result<T> = std::result::Result<T, Error>;

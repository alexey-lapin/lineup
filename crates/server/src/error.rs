use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("{0}")]
    Internal(String),
    /// The database file belongs to another lineup release (or is not a lineup database).
    #[error("{0}")]
    UnsupportedDatabase(String),
    /// A route's gateway could not be reached (connection, timeout).
    #[error("{0}")]
    GatewayUnreachable(String),
    /// A route's gateway answered with an error or something that is not a contract answer.
    #[error("{0}")]
    GatewayError(String),
    /// A route's gateway speaks another contract version.
    #[error("{0}")]
    ContractMismatch(String),
}

impl Error {
    /// Stable code used in API error bodies and mapped to CLI exit codes.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::Conflict(_) => "conflict",
            Error::Invalid(_) => "invalid",
            Error::Db(_) | Error::Migrate(_) | Error::Internal(_) | Error::UnsupportedDatabase(_) => "internal",
            Error::GatewayUnreachable(_) => "gateway_unreachable",
            Error::GatewayError(_) => "gateway_error",
            Error::ContractMismatch(_) => "contract_mismatch",
        }
    }

    /// A gateway failure (as opposed to a problem with the request or with lineup itself).
    pub fn is_gateway(&self) -> bool {
        matches!(
            self,
            Error::GatewayUnreachable(_) | Error::GatewayError(_) | Error::ContractMismatch(_)
        )
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Internal(format!("json: {e}"))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

pub(crate) fn not_found(msg: impl Into<String>) -> Error {
    Error::NotFound(msg.into())
}

pub(crate) fn conflict(msg: impl Into<String>) -> Error {
    Error::Conflict(msg.into())
}

/// True when a sqlx error is a UNIQUE constraint violation.
pub(crate) fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.is_unique_violation())
}

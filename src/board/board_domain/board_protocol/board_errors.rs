//! Typed board failures and their classification of crate-wide error codes.
use serde::{Deserialize, Serialize};
use tp_core::coded_error::{CodedError, ErrorCode};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardErrorCode {
    StaleRevision,
    ClaimConflict,
    BoardUnavailable,
    SchemaNewer,
    BoardInitializationRequired,
    InvalidReference,
    InvalidKind,
    InvalidWorker,
    InvalidBody,
    InvalidActor,
    InvalidState,
    InvalidOptions,
    Usage,
    BoardApiMismatch,
    BoardRemoteUnsupported,
    DatabaseLocked,
    DaemonBusy,
}

impl BoardErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StaleRevision => "stale_revision",
            Self::ClaimConflict => "claim_conflict",
            Self::BoardUnavailable => "board_unavailable",
            Self::SchemaNewer => "schema_newer",
            Self::BoardInitializationRequired => "board_initialization_required",
            Self::InvalidReference => "invalid_reference",
            Self::InvalidKind => "invalid_kind",
            Self::InvalidWorker => "invalid_worker",
            Self::InvalidBody => "invalid_body",
            Self::InvalidActor => "invalid_actor",
            Self::InvalidState => "invalid_state",
            Self::InvalidOptions => "invalid_options",
            Self::Usage => "usage",
            Self::BoardApiMismatch => "board_api_mismatch",
            Self::BoardRemoteUnsupported => "board_remote_unsupported",
            Self::DatabaseLocked => "database is locked",
            Self::DaemonBusy => "daemon_busy",
        }
    }

    /// The board classification of a crate-wide code; other `invalid_*`
    /// vocabulary codes are validation answers, not storage faults.
    pub fn from_code(code: ErrorCode) -> Option<Self> {
        Some(match code {
            ErrorCode::StaleRevision => Self::StaleRevision,
            ErrorCode::ClaimConflict => Self::ClaimConflict,
            ErrorCode::BoardUnavailable => Self::BoardUnavailable,
            ErrorCode::SchemaNewer => Self::SchemaNewer,
            ErrorCode::BoardInitializationRequired => Self::BoardInitializationRequired,
            ErrorCode::InvalidReference => Self::InvalidReference,
            ErrorCode::InvalidKind => Self::InvalidKind,
            ErrorCode::InvalidWorker => Self::InvalidWorker,
            ErrorCode::InvalidBody => Self::InvalidBody,
            ErrorCode::InvalidActor => Self::InvalidActor,
            ErrorCode::InvalidState => Self::InvalidState,
            ErrorCode::InvalidOptions => Self::InvalidOptions,
            ErrorCode::Usage => Self::Usage,
            ErrorCode::BoardApiMismatch => Self::BoardApiMismatch,
            ErrorCode::BoardRemoteUnsupported => Self::BoardRemoteUnsupported,
            ErrorCode::DatabaseLocked => Self::DatabaseLocked,
            ErrorCode::DaemonBusy => Self::DaemonBusy,
            code if code.as_str().starts_with("invalid_") => Self::InvalidOptions,
            _ => return None,
        })
    }

    /// Classifies typed errors: a [`BoardError`], then SQLite's typed code,
    /// then the first crate-wide code with a board classification.
    pub fn from_error(error: &anyhow::Error) -> Option<Self> {
        if let Some(typed) = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<BoardError>())
        {
            return Some(typed.code);
        }
        if let Some(sqlite) = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<rusqlite::Error>())
        {
            return Some(sqlite_board_code(sqlite));
        }
        board_coded_error(error).map(|(code, _)| code)
    }

    pub fn is_domain_answer(self) -> bool {
        !matches!(
            self,
            Self::BoardUnavailable | Self::SchemaNewer | Self::DatabaseLocked | Self::DaemonBusy
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BoardError {
    pub code: BoardErrorCode,
    pub message: String,
}

impl BoardError {
    pub fn new(code: BoardErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BoardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}
impl std::error::Error for BoardError {}
/// The first [`CodedError`] in `error`'s chain that has a board classification.
fn board_coded_error(error: &anyhow::Error) -> Option<(BoardErrorCode, &CodedError)> {
    error.chain().find_map(|cause| {
        let coded = cause.downcast_ref::<CodedError>()?;
        BoardErrorCode::from_code(coded.code()).map(|code| (code, coded))
    })
}

/// Constraint violations and datatype mismatches classify as invalid state;
/// too-big storage keeps its distinct body code for CLI callers. Only
/// contention maps to the locked code; corrupt, not-a-database, and other
/// storage failures stay transient so the outbox keeps them pending.
fn sqlite_board_code(error: &rusqlite::Error) -> BoardErrorCode {
    match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            BoardErrorCode::DatabaseLocked
        }
        Some(rusqlite::ErrorCode::ConstraintViolation) => BoardErrorCode::InvalidState,
        Some(rusqlite::ErrorCode::TooBig) => BoardErrorCode::InvalidBody,
        Some(rusqlite::ErrorCode::TypeMismatch) => BoardErrorCode::InvalidState,
        _ => BoardErrorCode::BoardUnavailable,
    }
}

impl From<anyhow::Error> for BoardError {
    fn from(error: anyhow::Error) -> Self {
        if let Some(typed) = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<BoardError>())
        {
            return typed.clone();
        }
        if let Some(sqlite) = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<rusqlite::Error>())
        {
            return Self::new(sqlite_board_code(sqlite), format!("{error:#}"));
        }
        if let Some((code, coded)) = board_coded_error(&error) {
            return Self::new(code, coded.message());
        }
        Self::new(BoardErrorCode::BoardUnavailable, format!("{error:#}"))
    }
}

//! Typed board failures and transport error classification.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardErrorCode {
    StaleRevision,
    ClaimConflict,
    BoardUnavailable,
    BoardInitializationRequired,
    InvalidReference,
    InvalidKind,
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
            Self::BoardInitializationRequired => "board_initialization_required",
            Self::InvalidReference => "invalid_reference",
            Self::InvalidKind => "invalid_kind",
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

    /// Reads only a leading code after recognized transport wrappers.
    pub fn leading(message: &str) -> Option<(Self, &str)> {
        let (label, detail) = leading_error_code(message)?;
        let code = match label {
            "stale_revision" => Self::StaleRevision,
            "claim_conflict" => Self::ClaimConflict,
            "board_unavailable" => Self::BoardUnavailable,
            "board_initialization_required" => Self::BoardInitializationRequired,
            "invalid_reference" => Self::InvalidReference,
            "invalid_kind" => Self::InvalidKind,
            "invalid_body" => Self::InvalidBody,
            "invalid_actor" => Self::InvalidActor,
            "invalid_state" => Self::InvalidState,
            "invalid_options" => Self::InvalidOptions,
            "usage" => Self::Usage,
            "board_api_mismatch" => Self::BoardApiMismatch,
            "board_remote_unsupported" => Self::BoardRemoteUnsupported,
            "database is locked" => Self::DatabaseLocked,
            "daemon_busy" => Self::DaemonBusy,
            _ => return None,
        };
        Some((code, detail))
    }

    /// Preserves typed errors and inspects each actual error-chain cause.
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
        error
            .chain()
            .find_map(|cause| Self::leading(&cause.to_string()).map(|(code, _)| code))
    }

    pub fn is_domain_answer(self) -> bool {
        !matches!(
            self,
            Self::BoardUnavailable | Self::DatabaseLocked | Self::DaemonBusy
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
/// Explanations remain free text; only repeated `daemon:` wrappers are removed.
pub fn leading_error_code(mut message: &str) -> Option<(&str, &str)> {
    loop {
        message = message.trim_start();
        if message.trim_end() == BoardErrorCode::DatabaseLocked.as_str() {
            return Some((BoardErrorCode::DatabaseLocked.as_str(), ""));
        }
        let (code, detail) = message.split_once(':')?;
        if code == "daemon" {
            message = detail;
        } else {
            return Some((code, detail.trim_start()));
        }
    }
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
        for cause in error.chain() {
            if let Some((code, detail)) = BoardErrorCode::leading(&cause.to_string()) {
                return Self::new(code, detail);
            }
        }
        Self::new(BoardErrorCode::BoardUnavailable, format!("{error:#}"))
    }
}

//! Request identity carried from the client through daemons, the router, and
//! diagnostics: request ID, creation time, claimed session/client labels, and
//! the optional actor origin a harness asserts.
mod actor_origin;

pub use actor_origin::{ActorOrigin, HarnessLabel, validate_actor_component};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(from = "RequestContextRecord")]
pub struct RequestContext {
    pub request_id: String,
    pub created_unix_micros: u64,
    pub session: Option<String>,
    pub client: Option<String>,
    // Missing in historical records means `session` was the diagnostic baseline.
    // New records serialize `null` when no baseline exists, preserving that distinction.
    #[serde(default)]
    pub diagnostic_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ActorOrigin>,
}

#[derive(Deserialize)]
struct RequestContextRecord {
    request_id: String,
    created_unix_micros: u64,
    session: Option<String>,
    client: Option<String>,
    #[serde(default)]
    diagnostic_session: StoredDiagnosticSession,
    #[serde(default)]
    origin: Option<ActorOrigin>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredDiagnosticSession {
    Present(Option<String>),
    Missing,
}

impl Default for StoredDiagnosticSession {
    fn default() -> Self {
        Self::Missing
    }
}

impl From<RequestContextRecord> for RequestContext {
    fn from(record: RequestContextRecord) -> Self {
        let diagnostic_session = match record.diagnostic_session {
            StoredDiagnosticSession::Present(session) => session,
            StoredDiagnosticSession::Missing => record.session.clone(),
        };
        Self {
            request_id: record.request_id,
            created_unix_micros: record.created_unix_micros,
            session: record.session,
            client: record.client,
            diagnostic_session,
            origin: record.origin,
        }
    }
}

impl RequestContext {
    pub fn new(session: Option<String>, client: Option<String>) -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            created_unix_micros: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros()
                .min(i64::MAX as u128) as u64,
            diagnostic_session: session.clone(),
            session,
            client,
            origin: None,
        }
    }

    pub fn with_origin(
        session: Option<String>,
        client: Option<String>,
        origin: Option<ActorOrigin>,
    ) -> Self {
        let mut context = Self::new(session, client);
        context.origin = origin;
        context
    }

    pub fn with_diagnostic_session(
        session: Option<String>,
        client: Option<String>,
        origin: Option<ActorOrigin>,
        diagnostic_session: Option<String>,
    ) -> Self {
        let mut context = Self::new(session, client);
        context.origin = origin;
        context.diagnostic_session = diagnostic_session;
        context
    }

    /// Starts a retry attempt while preserving the client-observed identity.
    pub fn next_attempt(&self) -> Self {
        let mut context = self.clone();
        context.request_id = uuid::Uuid::new_v4().to_string();
        context.created_unix_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros()
            .min(i64::MAX as u128) as u64;
        context
    }
}

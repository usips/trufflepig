use super::DeliveryReceipt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsMode {
    Off,
    #[default]
    Metadata,
    Detailed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestContext {
    pub request_id: String,
    pub created_unix_micros: u64,
    pub session: Option<String>,
    pub client: Option<String>,
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
            session,
            client,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    More,
    References,
    Map,
    Search,
    Show,
    Context,
    HistoryIndex,
    HistoryStatus,
    History,
    Since,
    Diff,
    Blame,
    Index,
    Status,
    Doctor,
    SessionStart,
    SessionEnd,
    Audit,
    ForgetLogs,
    Help,
    Version,
    Usage,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Empty,
    BudgetTooSmall,
    InvalidInput,
    Unavailable,
    Stale,
    Failure,
}

/// Identity is captured when emitted, independently of the expiring result set.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EmittedIdentity {
    pub repository: String,
    pub path: String,
    pub content_revision: String,
    pub commit: Option<String>,
    pub blob: Option<String>,
    pub start_byte: usize,
    pub end_byte: usize,
    pub original_rank: Option<usize>,
    pub source_body: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestEvent {
    pub schema_version: u32,
    pub context: RequestContext,
    pub stage: EventStage,
    pub timestamp: u64,
    pub operation: Operation,
    pub outcome: Outcome,
    pub elapsed_micros: u64,
    pub stderr_accepted_bytes: usize,
    pub receipt: Option<DeliveryReceipt>,
    pub retrieval: Option<crate::search::telemetry::RetrievalTrace>,
    pub residency: Option<crate::semantic::ResidencySample>,
    pub probes: Vec<crate::probes::ProbeResult>,
    pub coverage: Option<crate::store::Coverage>,
    pub emitted: Vec<EmittedIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_query: Option<String>,
    pub truncated: bool,
}

impl RequestEvent {
    pub fn new(context: RequestContext, operation: Operation, outcome: Outcome) -> Self {
        Self {
            schema_version: 1,
            context,
            stage: EventStage::Delivery,
            timestamp: now(),
            operation,
            outcome,
            elapsed_micros: 0,
            stderr_accepted_bytes: 0,
            receipt: None,
            retrieval: None,
            residency: None,
            probes: Vec::new(),
            coverage: None,
            emitted: Vec::new(),
            raw_query: None,
            truncated: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventStage {
    Server,
    Delivery,
    Maintenance,
}

pub(super) fn sanitize(event: &mut RequestEvent, mode: DiagnosticsMode) {
    if event.probes.len() > 6 {
        event.probes.truncate(6);
        event.truncated = true;
    }
    if mode != DiagnosticsMode::Detailed {
        event.raw_query = None;
    }
    fn bound(value: &mut String, maximum: usize, truncated: &mut bool) {
        if value.len() > maximum {
            let mut end = maximum;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            value.truncate(end);
            *truncated = true;
        }
    }
    bound(&mut event.context.request_id, 64, &mut event.truncated);
    for value in [&mut event.context.session, &mut event.context.client]
        .into_iter()
        .flatten()
    {
        bound(value, 128, &mut event.truncated);
    }
    if let Some(query) = &mut event.raw_query {
        bound(query, 4096, &mut event.truncated);
    }
    if event.retrieval.as_ref().is_some_and(|trace| {
        trace
            .lanes
            .iter()
            .any(|lane| lane.candidate_records_omitted > 0)
    }) {
        event.truncated = true;
    }
    if event.emitted.len() > 128 {
        event.emitted.truncate(128);
        event.truncated = true;
    }
    for identity in &mut event.emitted {
        bound(&mut identity.repository, 1024, &mut event.truncated);
        bound(&mut identity.path, 4096, &mut event.truncated);
        bound(&mut identity.content_revision, 128, &mut event.truncated);
        for oid in [&mut identity.commit, &mut identity.blob]
            .into_iter()
            .flatten()
        {
            bound(oid, 64, &mut event.truncated);
        }
    }
}

pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

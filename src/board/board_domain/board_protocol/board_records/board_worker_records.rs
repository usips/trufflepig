//! Worker lifetimes attached to task claims.
use crate::board::{
    board_actor::BoardActor,
    board_ids::EntryId,
    board_protocol::{ProgressEvidence, ReviewVerdict, WorkerAddress},
    board_vocabulary::{EntryText, WorkerEndReason, WorkerRole},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRecord {
    pub address: WorkerAddress,
    pub claim: EntryId,
    pub entry: EntryId,
    pub actor: Option<BoardActor>,
    pub role: WorkerRole,
    pub model: String,
    pub effort: Option<String>,
    pub scope: EntryText,
    pub files: Vec<String>,
    pub state: Option<WorkerEndReason>,
    pub added_at: i64,
    pub last_active: i64,
    pub ended_at: Option<i64>,
    pub note: Option<EntryText>,
    pub progress: Option<ProgressEvidence>,
    pub verdict: Option<ReviewVerdict>,
}

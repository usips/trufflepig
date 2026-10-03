//! Bounded search results retain canonical targets and plain text snippets.
use crate::board::board_ids::{BoardRef, PlanId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardSearchSource {
    Entry,
    Revision,
    Proposal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardSearchHit {
    pub target: BoardRef,
    pub plan: Option<PlanId>,
    pub source: BoardSearchSource,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardSearchReply {
    pub hits: Vec<BoardSearchHit>,
    pub truncated: bool,
}

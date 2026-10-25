//! Bounded notices returned by the read-only pending operation.
use super::{EntryRecord, EventRecord};
use crate::board_ids::EventSeq;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingNoticeKind {
    Entry,
    Event,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingNoticeReason {
    Mention,
    Question,
    Claim,
    Worker,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingNotice {
    pub seq: EventSeq,
    pub kind: PendingNoticeKind,
    pub reason: PendingNoticeReason,
    pub entry: Option<EntryRecord>,
    pub event: Option<EventRecord>,
}

impl PendingNotice {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.entry.is_some() == self.event.is_some() {
            anyhow::bail!(coded!(
                InvalidState,
                "pending notice must contain exactly one entry or event"
            ));
        }
        if (self.kind == PendingNoticeKind::Entry) != self.entry.is_some() {
            anyhow::bail!(coded!(
                InvalidState,
                "pending notice payload does not match its kind"
            ));
        }
        if let Some(entry) = &self.entry
            && entry.seq != self.seq
        {
            anyhow::bail!(coded!(
                InvalidState,
                "pending entry sequence does not match notice"
            ));
        }
        if let Some(event) = &self.event
            && event.seq != self.seq
        {
            anyhow::bail!(coded!(
                InvalidState,
                "pending event sequence does not match notice"
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingReply {
    pub notices: Vec<PendingNotice>,
    pub after: Option<EventSeq>,
    pub through: EventSeq,
    pub server_now: i64,
    pub generation: String,
    pub omitted: usize,
}

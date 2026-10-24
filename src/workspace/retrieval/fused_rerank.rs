//! One cross-encoder request reorders the top of the interleaved workspace
//! ranking. Scores from one model over every candidate compare across
//! members; owners and member ranks travel with their entries, and fused
//! position breaks ties. Its diagnostics event names the workspace, no member.
use super::{WorkspaceQuery, member_fusion::FusedMembers, member_search::MemberCommand};
use crate::{
    diagnostics::{EventStage, Operation, Outcome, RequestEvent},
    results::ResultEntry,
    search::{MergedRerank, RerankOutcome, RerankScorer, telemetry::RetrievalTrace},
};
use std::time::Instant;

#[cfg(test)]
mod tests;

/// Reranks the merged window once and records a workspace server event.
pub(super) fn rerank_once(
    fused: &mut FusedMembers,
    request: &WorkspaceQuery,
    command: &MemberCommand,
    scorer: &dyn RerankScorer,
) {
    if fused.set.owners.is_empty() {
        return;
    }
    let started = Instant::now();
    let mut trace = command.trace();
    let rerank = MergedRerank {
        query: &command.query,
        cache: request.cache,
        scorer,
        deadline: &request.deadline,
    };
    let outcome = rerank_merged_window(fused, &rerank, &mut trace);
    let event_outcome = match outcome {
        RerankOutcome::Unavailable(_) => Outcome::Unavailable,
        RerankOutcome::Ready { .. } | RerankOutcome::Skipped => Outcome::Success,
    };
    let mut event = RequestEvent::new(request.context.clone(), Operation::Search, event_outcome);
    event.stage = EventStage::Server;
    event.elapsed_micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    event.retrieval = Some(trace);
    event.workspace = Some(request.config.id.clone());
    let log_cache = command.log_cache.as_deref().unwrap_or(request.cache);
    crate::diagnostics::best_effort_record(log_cache, command.mode, event);
}

/// Reranks `fused.set.hits` in place, releases the member stores, and records
/// each owner's view of the outcome: the shared status and reason, and as
/// `rerank_window` the count of its own hits that scored.
fn rerank_merged_window(
    fused: &mut FusedMembers,
    rerank: &MergedRerank,
    trace: &mut RetrievalTrace,
) -> RerankOutcome {
    let stores = std::mem::take(&mut fused.stores);
    let set = &mut fused.set;
    let (outcome, scored) = rerank.run(
        &mut set.hits,
        |entry| match &entry.entry {
            ResultEntry::LiveSource(hit) => Some(hit),
            ResultEntry::Commit(_) | ResultEntry::Change(_) => None,
        },
        |entry| stores.get(entry.owner).and_then(Option::as_ref),
        trace,
    );
    drop(stores);
    let mut scored_by_owner = vec![0; set.owners.len()];
    for (entry, _) in set.hits.iter().zip(&scored).filter(|(_, scored)| **scored) {
        scored_by_owner[entry.owner] += 1;
    }
    for (owner_index, owner) in set.owners.iter_mut().enumerate() {
        let member_outcome = match &outcome {
            RerankOutcome::Ready { .. } => RerankOutcome::Ready {
                scored: scored_by_owner[owner_index],
            },
            shared => shared.clone(),
        };
        member_outcome.record(&mut owner.coverage);
        let issues = &mut set.coverage[fused.owner_rows[owner_index]]["issues"];
        for key in ["rerank_status", "rerank_reason"] {
            if let Some(value) = owner.coverage.get(key) {
                issues[key] = value.clone();
            }
        }
    }
    outcome
}

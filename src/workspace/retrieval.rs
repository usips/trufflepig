//! Fair rank merging of independent published member snapshots. Phases:
//! [member search](member_search), round-robin [fusion](member_fusion), one
//! opt-in [rerank](fused_rerank) of the merged window, then page assembly.
mod fused_rerank;
mod inline_member_index;
mod member_coverage;
mod member_fusion;
pub(super) mod member_scope;
mod member_search;
#[cfg(test)]
mod tests;

use super::{WorkspaceConfig, home_index::HomeIndexPolicy, result_cache::WorkspaceResults};
use crate::cli::Arguments;
use crate::workspace::MemberRoot;
use crate::workspace::workspace_discovery::WorkspaceMembers;
use anyhow::Result;
use member_fusion::MemberFusion;
use member_scope::{Scope, scope};
use member_search::{MemberCommand, SearchedMember, search_member};
use std::path::Path;
use tp_core::{
    coded_error::{CodedError, ErrorCode},
    output::OutputBudget,
    query_deadline::QueryDeadline,
    request_context::RequestContext,
};
use tp_semantic::SemanticSession;

/// One workspace retrieval command (`search`, `refs`, or `map`) and where it
/// reads and persists results.
pub(super) struct WorkspaceQuery<'a> {
    pub config: &'a WorkspaceConfig,
    /// Workspace cache: result sets, query vectors, and the rerank identity.
    pub cache: &'a Path,
    pub results: &'a WorkspaceResults,
    pub options: &'a Arguments,
    pub context: &'a RequestContext,
    pub deadline: QueryDeadline,
    pub policy: HomeIndexPolicy,
}

/// Runs a workspace query: `--no-daemon` indexes members inline; otherwise
/// members answer from published indexes ([`HomeIndexPolicy::for_search`]).
pub(super) fn search(
    config: &WorkspaceConfig,
    cache: &Path,
    results: &WorkspaceResults,
    options: &Arguments,
    context: &RequestContext,
    session: &mut SemanticSession,
    deadline: QueryDeadline,
) -> Result<String> {
    let request = WorkspaceQuery {
        config,
        cache,
        results,
        options,
        context,
        deadline,
        policy: HomeIndexPolicy::for_search(options.no_daemon),
    };
    search_with_policy(&request, session)
}

pub(super) fn search_with_policy(
    request: &WorkspaceQuery,
    session: &mut SemanticSession,
) -> Result<String> {
    search_with_policy_using(request, session, search_member, |request, member| {
        super::stateless_queries::run_after_router_replacement(
            request.config,
            request.options,
            &request.deadline,
            member,
        )
    })
}

fn search_with_policy_using<SearchMember, RouterFallback>(
    request: &WorkspaceQuery,
    session: &mut SemanticSession,
    mut search_member: SearchMember,
    mut router_fallback: RouterFallback,
) -> Result<String>
where
    SearchMember: FnMut(
        &WorkspaceQuery<'_>,
        &MemberCommand<'_>,
        &MemberRoot,
        bool,
        &mut Option<CodedError>,
    ) -> Result<SearchedMember>,
    RouterFallback: FnMut(
        &WorkspaceQuery<'_>,
        &MemberRoot,
    ) -> Result<super::stateless_queries::RouterReplacementAnswer>,
{
    let invocation_root = request.options.root.canonicalize()?;
    let roots = request.config.member_roots(&invocation_root)?;
    let Scope {
        members: mut queue,
        words,
        implicit_home,
    } = scope(&roots, request.options)?;
    let command = MemberCommand::prepare(request, &words, invocation_root, session)?;
    let mut fusion = MemberFusion::new(request.config, &roots);
    let mut map_miss = None;
    let mut position = 0;
    let mut router_fallback_attempted = false;
    let mut router_fallback_failure = None;
    let mut answered_members = 0;
    while position < queue.len() {
        let index = queue[position];
        position += 1;
        let member = &roots[index];
        let searched = search_member(request, &command, member, position == 1, &mut map_miss)?;
        let replaced_owner = searched
            .answer
            .as_ref()
            .err()
            .is_some_and(|error| ErrorCode::RouterBinaryReplaced.is_named_in(error));
        if !router_fallback_attempted
            && replaced_owner
            && super::stateless_queries::eligible(request.options)
        {
            router_fallback_attempted = true;
            match router_fallback(request, member) {
                Ok(_) if request.deadline.cancelled() => {
                    anyhow::bail!(coded!(TimedOut, "query was cancelled"));
                }
                Ok(answer) if answer.complete => return Ok(answer.output),
                Ok(answer) => {
                    router_fallback_failure = Some((
                        member.name().to_owned(),
                        answer
                            .incomplete_reason
                            .unwrap_or_else(|| "current-source coverage incomplete".to_owned()),
                    ));
                }
                Err(error) if request.deadline.cancelled() => return Err(error),
                Err(error) => {
                    router_fallback_failure =
                        Some((member.name().to_owned(), short_recovery_reason(&error)));
                }
            }
        }
        let state = fusion.admit(member, searched, queue.len())?;
        if state == "searched" {
            answered_members += 1;
        }
        if let Some((failed_member, reason)) = &router_fallback_failure
            && failed_member == member.name()
        {
            fusion.annotate_router_recovery_failure(member.name(), reason);
        }
        if implicit_home && position == 1 && fusion.settle_home_scope(roots.len() - 1, state) {
            queue.extend((0..roots.len()).filter(|&i| i != index));
        }
    }
    if let Some((member, reason)) = &router_fallback_failure
        && answered_members == 0
    {
        if request.deadline.cancelled() || request.deadline.time_expired() {
            anyhow::bail!(coded!(
                TimedOut,
                "bounded current-source recovery did not answer workspace member {member}; absence=not_proven; recovery={reason}"
            ));
        }
        anyhow::bail!(coded!(
            WorkspaceUnavailable,
            "no workspace member answered after bounded current-source recovery for {member}; absence=not_proven; recovery={reason}"
        ));
    }
    let mut fused = fusion.interleave(map_miss, implicit_home)?;
    if command.rerank {
        fused_rerank::rerank_once(&mut fused, request, &command, &*session);
    }
    let id = request.results.save(fused.set)?;
    let options = request.options;
    let budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    request.results.page(&id, 0, options.limit, &budget)
}

fn short_recovery_reason(error: &anyhow::Error) -> String {
    let reason = format!("{error:#}");
    let mut short: String = reason.chars().take(120).collect();
    if reason.chars().count() > 120 {
        short.push('…');
    }
    short
}

fn timeout_without_member_answer(
    answered_members: usize,
    coverage: &[serde_json::Value],
    phase: Option<&'static str>,
) -> Option<anyhow::Error> {
    if answered_members > 0 {
        return None;
    }
    let timed_out = coverage
        .iter()
        .find(|member| member["state"] == "timed_out")?;
    let member = timed_out["member"].as_str().unwrap_or("unknown");
    let reason = timed_out["reason"]
        .as_str()
        .unwrap_or("query deadline expired");
    let phase = phase.unwrap_or("member_processing");
    let mut details = serde_json::json!({
        "absence": "not_proven",
        "fallback": "show path:FILE",
        "member": member,
        "phase": phase,
        "reason": reason,
    });
    if let Some(root) = timed_out["root"].as_str() {
        details["root"] = root.into();
    }
    Some(anyhow::anyhow!(coded!(
        TimedOut,
        "query deadline expired before any member answered; details={details}"
    )))
}

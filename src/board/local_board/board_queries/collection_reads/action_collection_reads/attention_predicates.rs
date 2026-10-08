//! Attention predicate over joined entries and actors.
use super::*;

/// Attention predicate over `entries e JOIN actors a` (?1 user, ?2 host, ?3 harness,
/// ?4 session, ?5 identity, ?6 scope kind, ?7 JSON keys, ?8 after seq, ?9 entry, ?10 through).
/// The kind prefix drives the scan from `entries_kind_state`; own unaddressed questions
/// stay out of "Needs you" and proposal currency reads the head as of ?10.
pub(in crate::board::local_board::board_queries::collection_reads) fn attention_predicate() -> String
{
    let strict_scope = ScopeSql::strict_plan_predicate("e.plan_id", 6, 7);
    let exact_author = "a.user=?1 AND a.host=?2 AND a.harness=?3 AND a.session=?4";
    let head_at_through = concat!(
        "(SELECT max(number) FROM revisions head_at WHERE head_at.plan_id=p.plan_id ",
        "AND head_at.seq<=?10)"
    );
    let plan_authority = concat!(
        "EXISTS(SELECT 1 FROM plans managed WHERE managed.id=e.plan_id AND managed.owner_user=?1 ",
        "AND (?3='human' OR managed.steward=?3))"
    );
    let current_proposal = format!(
        concat!(
            "EXISTS(SELECT 1 FROM proposals p JOIN plans head ON head.id=p.plan_id WHERE p.entry_id=e.id ",
            "AND p.state='open' AND p.base_revision={head_at_through} AND head.owner_user=?1 ",
            "AND (?3='human' OR head.steward=?3))"
        ),
        head_at_through = head_at_through
    );
    let stale_proposal = format!(
        concat!(
            "EXISTS(SELECT 1 FROM proposals p WHERE p.entry_id=e.id ",
            "AND p.state='open' AND p.base_revision<>{head_at_through})"
        ),
        head_at_through = head_at_through
    );
    let open_feedback = "e.kind='feedback' AND e.state IN ('open','triaged')";
    let open_question = open_question("?10");
    format!(
        concat!(
            "e.kind IN ('question','proposal','feedback') AND ((({exact_author}) ",
            "AND (({open_feedback}) OR {stale_proposal})) ",
            "OR (({current_proposal} OR (({open_feedback}) AND ({plan_authority}))) ",
            "AND (e.to_whom IS NULL OR e.to_whom IN (?1,?3,?5))) ",
            "OR ((({open_question} AND (NOT ({exact_author}) OR e.to_whom IN (?1,?3,?5))) OR (({open_feedback}) AND (e.plan_id IS NULL AND a.user=?1 AND ?3='human'))) ",
            "AND (e.to_whom IS NULL OR e.to_whom IN (?1,?3,?5)) ",
            "AND (?6<>1 OR NOT EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id) ",
            "OR e.to_whom IN (?1,?3,?5) OR e.repo_key IN(SELECT value FROM json_each(?7)) ",
            "OR EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id AND scope.repo_key IN(SELECT value FROM json_each(?7)))))) ",
            "AND {strict_scope} AND (e.seq>?8 OR (e.seq=?8 AND e.id>?9)) AND e.seq<=?10"
        ),
        strict_scope = strict_scope,
        exact_author = exact_author,
        open_feedback = open_feedback,
        stale_proposal = stale_proposal,
        current_proposal = current_proposal,
        plan_authority = plan_authority,
        open_question = open_question
    )
}

//! Attention predicate over joined entries and actors.
use super::*;
use crate::board::board_protocol::AuthorityRole;
use crate::board::local_board::board_queries::board_reads::recipient_predicates::{
    addressed_to_actor_sql, task_entry_sql, task_owner_human_question_sql,
};

/// Attention predicate over `entries e JOIN actors a` (?1 user, ?2 host, ?3 harness,
/// ?4 session, ?5 identity, ?6 scope kind, ?7 JSON keys, ?8 after seq, ?9 entry,
/// ?10 through, ?11 authority time).
/// The kind prefix drives the scan from `entries_kind_state`; own unaddressed questions
/// stay out of "Needs you" and proposal currency reads the head as of ?10.
pub(in crate::board::local_board::board_queries::collection_reads) fn attention_predicate() -> String
{
    let strict_scope = ScopeSql::strict_plan_predicate("e.plan_id", 6, 7);
    let exact_author = "a.user=?1 AND a.host=?2 AND a.harness=?3 AND a.session=?4";
    let addressed = addressed_to_actor_sql("e.to_whom", "?1", "?3", "?5");
    let task_entry = task_entry_sql("e.id");
    let task_owner = task_owner_human_question_sql("e.id", "?1", "?3");
    let own_task_question = format!("({exact_author} AND {task_entry})");
    let question_recipient =
        format!("((e.to_whom IS NULL AND NOT ({task_entry})) OR {addressed} OR {task_owner})");
    let head_at_through = concat!(
        "(SELECT max(number) FROM revisions head_at WHERE head_at.plan_id=p.plan_id ",
        "AND head_at.seq<=?10)"
    );
    let decide_authority = crate::board::local_board::board_authority::authority_predicate_sql(
        AuthorityRole::Decide,
        "p.plan_id",
        "head.owner_user",
        "?1",
        "?2",
        "?3",
        "?4",
        "?5",
        "?11",
    );
    let current_proposal = format!(
        concat!(
            "EXISTS(SELECT 1 FROM proposals p JOIN plans head ON head.id=p.plan_id WHERE p.entry_id=e.id ",
            "AND p.state='open' AND p.base_revision={head_at_through} AND {decide_authority})"
        ),
        head_at_through = head_at_through,
        decide_authority = decide_authority,
    );
    let stale_proposal = format!(
        concat!(
            "EXISTS(SELECT 1 FROM proposals p WHERE p.entry_id=e.id ",
            "AND p.state='open' AND p.base_revision<>{head_at_through})"
        ),
        head_at_through = head_at_through
    );
    let open_question = open_question("?10");
    let task_open_question = format!("(({open_question}) AND ({task_entry}))");
    format!(
        concat!(
            "e.kind IN ('question','proposal') AND ((({exact_author}) ",
            "AND ({stale_proposal})) ",
            "OR ({current_proposal} ",
            "AND (e.to_whom IS NULL OR {addressed})) ",
            "OR ({open_question} AND (NOT ({exact_author}) OR {addressed} OR {task_owner}) ",
            "AND ({question_recipient}) ",
            "AND (?6<>1 OR NOT EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id) ",
            "OR {addressed} OR e.repo_key IN(SELECT value FROM json_each(?7)) ",
            "OR EXISTS(SELECT 1 FROM plan_repos scope WHERE scope.plan_id=e.plan_id AND scope.repo_key IN(SELECT value FROM json_each(?7)))))) ",
            "AND {strict_scope} AND NOT ({own_task_question}) ",
            "AND ((e.seq>?8 OR (e.seq=?8 AND e.id>?9)) OR {task_open_question}) AND e.seq<=?10"
        ),
        strict_scope = strict_scope,
        exact_author = exact_author,
        stale_proposal = stale_proposal,
        current_proposal = current_proposal,
        open_question = open_question,
        addressed = addressed,
        task_owner = task_owner,
        question_recipient = question_recipient,
        task_open_question = task_open_question,
        own_task_question = own_task_question,
    )
}

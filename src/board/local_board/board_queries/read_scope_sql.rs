//! Shared plan membership predicates with a single JSON repository-key binding.
use crate::board::board_ids::{PlanId, RepoKey};
use crate::board::board_protocol::ReadScope;
use crate::board::local_board::{BoardError, invalid, sql_error, sql_number};
use rusqlite::Connection;

pub(in crate::board::local_board) struct ScopeSql {
    pub kind: i64,
    pub keys_json: String,
}

impl ScopeSql {
    pub fn new(scope: &ReadScope) -> Result<Self, BoardError> {
        let (kind, keys_json) = match scope {
            ReadScope::All => (0, Ok("[]".to_owned())),
            ReadScope::Repo(key) => (1, serde_json::to_string(&[key])),
            ReadScope::Keys(keys) => (2, serde_json::to_string(keys)),
            ReadScope::Unscoped => (3, Ok("[]".to_owned())),
        };
        let keys_json = keys_json.map_err(|error| invalid("invalid_options", error.to_string()))?;
        Ok(Self { kind, keys_json })
    }

    pub fn plan_predicate(plan: &str, kind: usize, keys: usize) -> String {
        format!(
            concat!(
                "(?{kind}=0 OR (?{kind} IN (1,3) AND {plan} IS NOT NULL ",
                "AND NOT EXISTS(SELECT 1 FROM plan_repos scoped WHERE scoped.plan_id={plan})) ",
                "OR (?{kind} IN (1,2) AND EXISTS(SELECT 1 FROM plan_repos scoped ",
                "WHERE scoped.plan_id={plan} AND scoped.repo_key IN(SELECT value FROM json_each(?{keys})))))"
            ),
            kind = kind,
            keys = keys,
            plan = plan,
        )
    }

    pub fn strict_plan_predicate(plan: &str, kind: usize, keys: usize) -> String {
        let membership = Self::plan_predicate(plan, kind, keys);
        format!("(?{kind}<2 OR {membership})")
    }

    pub fn event_predicate(kind: usize, keys: usize) -> String {
        let event = Self::plan_predicate("e.plan_id", kind, keys);
        let evidence = Self::plan_predicate("evidence.plan_id", kind, keys);
        format!(
            "({event} OR EXISTS(SELECT 1 FROM entries evidence WHERE evidence.seq=e.seq AND {evidence}))"
        )
    }
}

pub(in crate::board::local_board) fn plan_repo_keys(
    conn: &Connection,
    plan: PlanId,
) -> Result<Vec<RepoKey>, BoardError> {
    let mut statement = conn
        .prepare("SELECT repo_key FROM plan_repos WHERE plan_id=?1 ORDER BY repo_key")
        .map_err(sql_error)?;
    statement
        .query_map([sql_number(plan.get())], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .map(|key| {
            key.map_err(sql_error)?.parse().map_err(|error| {
                invalid(
                    "board_unavailable",
                    format!("invalid stored repository key: {error}"),
                )
            })
        })
        .collect()
}

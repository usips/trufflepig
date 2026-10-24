//! Central plan and board grant authority checks.

mod authority_writes;
#[cfg(test)]
mod tests;

pub(in crate::board) use authority_writes::{write_grant, write_revoke};

use super::{invalid, require_plan, sql_error, sql_number};
use crate::board::board_actor::{BoardActor, BoardRecipient, HarnessLabel};
use crate::board::board_ids::{EntryId, PlanId};
use crate::board::board_protocol::BoardErrorCode;
use crate::board::board_protocol::{
    AuthorityRole, AuthoritySource, BoardError, GrantRecord, GrantSource,
};
use rusqlite::{Connection, OptionalExtension, params};

/// Returns the grant or owner authority that covers a plan at `now`.
pub(in crate::board) fn authority_for(
    conn: &Connection,
    actor: &BoardActor,
    plan: Option<PlanId>,
    role: AuthorityRole,
    now: i64,
) -> Result<Option<AuthoritySource>, BoardError> {
    authority_for_owner(conn, actor, plan, None, role, now)
}

/// Returns owner-visible grant history, newest first, capped at 200 rows.
pub(in crate::board::local_board) fn read_grants(
    conn: &Connection,
    plan: Option<PlanId>,
) -> Result<Vec<GrantRecord>, BoardError> {
    let mut statement = conn
        .prepare(concat!(
            "SELECT entry_id,plan_id,owner_user,roles_json,grantee,granted_at,expires_at,",
            "revoked_by_entry,source FROM authority_grants WHERE plan_id IS ?1 ",
            "ORDER BY granted_at DESC,entry_id DESC LIMIT 200"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query([plan.map(|id| sql_number(id.get()))])
        .map_err(sql_error)?;
    let mut grants = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let entry_id = parse_entry_id(row.get(0).map_err(sql_error)?)?;
        let plan_id: Option<i64> = row.get(1).map_err(sql_error)?;
        let roles_json: String = row.get(3).map_err(sql_error)?;
        let roles: Vec<AuthorityRole> = serde_json::from_str(&roles_json)
            .map_err(|error| invalid(BoardErrorCode::BoardUnavailable, error.to_string()))?;
        let grantee: String = row.get(4).map_err(sql_error)?;
        let granted_at: i64 = row.get(5).map_err(sql_error)?;
        let expires_at: Option<i64> = row.get(6).map_err(sql_error)?;
        let revoked_by: Option<i64> = row.get(7).map_err(sql_error)?;
        let source: String = row.get(8).map_err(sql_error)?;
        grants.push(GrantRecord {
            entry: entry_id,
            plan: plan_id.map(parse_plan_id).transpose()?,
            owner_user: row.get(2).map_err(sql_error)?,
            roles,
            grantee: BoardRecipient::parse(&grantee).map_err(BoardError::from)?,
            granted_at,
            expires_at,
            revoked_by: revoked_by.map(parse_entry_id).transpose()?,
            source: GrantSource::parse(&source).map_err(BoardError::from)?,
        });
    }
    Ok(grants)
}

/// Grant management is restricted to the plan's human owner.
pub(in crate::board::local_board) fn can_manage_grants(
    conn: &Connection,
    actor: &BoardActor,
    plan: PlanId,
) -> Result<bool, BoardError> {
    require_plan(conn, plan)?;
    let owner: String = conn
        .query_row(
            "SELECT owner_user FROM plans WHERE id=?1",
            [sql_number(plan.get())],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    Ok(actor.user == owner && actor.harness.is_human())
}

/// Resolves planless feedback authority against the report owner's user.
pub(in crate::board) fn authority_for_feedback(
    conn: &Connection,
    actor: &BoardActor,
    plan: Option<PlanId>,
    reporter_user: &str,
    role: AuthorityRole,
    now: i64,
) -> Result<Option<AuthoritySource>, BoardError> {
    authority_for_owner(conn, actor, plan, Some(reporter_user), role, now)
}

fn authority_for_owner(
    conn: &Connection,
    actor: &BoardActor,
    plan: Option<PlanId>,
    owner_override: Option<&str>,
    role: AuthorityRole,
    now: i64,
) -> Result<Option<AuthoritySource>, BoardError> {
    let owner_user = match plan {
        Some(plan) => {
            require_plan(conn, plan)?;
            conn.query_row(
                "SELECT owner_user FROM plans WHERE id=?1",
                [sql_number(plan.get())],
                |row| row.get::<_, String>(0),
            )
            .map_err(sql_error)?
        }
        None => owner_override.unwrap_or(&actor.user).to_owned(),
    };
    if actor.user == owner_user && actor.harness.is_human() {
        return Ok(Some(AuthoritySource::Owner));
    }

    let grant_entry: Option<i64> = conn
        .query_row(
            concat!(
                "SELECT g.entry_id FROM authority_grants g ",
                "WHERE g.plan_id IS ?1 AND g.owner_user=?2 AND g.revoked_by_entry IS NULL ",
                "AND (g.expires_at IS NULL OR g.expires_at>?3) ",
                "AND EXISTS(SELECT 1 FROM json_each(g.roles_json) r WHERE r.value=?4) ",
                "AND (g.grantee=?5 OR (g.grantee=?6 AND ?7=g.owner_user)) ",
                "ORDER BY g.granted_at DESC,g.entry_id DESC LIMIT 1"
            ),
            params![
                plan.map(|id| sql_number(id.get())),
                owner_user,
                now,
                role.as_str(),
                actor.identity(),
                actor.harness.as_str(),
                actor.user
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    grant_entry
        .map(|id| {
            EntryId::new(u64::try_from(id).map_err(|_| {
                invalid(
                    BoardErrorCode::BoardUnavailable,
                    "authority grant has an invalid entry id",
                )
            })?)
            .map(AuthoritySource::Grant)
            .map_err(BoardError::from)
        })
        .transpose()
}

/// Builds the shared SQLite predicate used by attention and task reads.
pub(in crate::board) fn authority_predicate_sql(
    role: AuthorityRole,
    plan_id_sql: &str,
    owner_user_sql: &str,
    actor_user_sql: &str,
    actor_host_sql: &str,
    actor_harness_sql: &str,
    actor_session_sql: &str,
    actor_identity_sql: &str,
    now_sql: &str,
) -> String {
    format!(
        concat!(
            "(({owner}={user} AND {harness}='human') OR EXISTS(SELECT 1 FROM authority_grants auth_g ",
            "WHERE auth_g.plan_id IS ({plan}) AND auth_g.owner_user=({owner}) ",
            "AND auth_g.revoked_by_entry IS NULL AND (auth_g.expires_at IS NULL OR auth_g.expires_at>({now})) ",
            "AND EXISTS(SELECT 1 FROM json_each(auth_g.roles_json) auth_r WHERE auth_r.value='{role}') ",
            "AND ((auth_g.grantee=({identity}) AND ({identity})=({user}||'@'||{host}||'/'||{harness}||'/'||{session})) ",
            "OR (auth_g.grantee=({harness}) AND ({user})=auth_g.owner_user))))"
        ),
        owner = owner_user_sql,
        user = actor_user_sql,
        host = actor_host_sql,
        harness = actor_harness_sql,
        session = actor_session_sql,
        plan = plan_id_sql,
        now = now_sql,
        role = role.as_str(),
        identity = actor_identity_sql,
    )
}

/// A bare label is harness-wide scope; reserved harnesses can never match a grant.
pub(in crate::board::local_board) fn validate_grantee(
    grantee: &BoardRecipient,
) -> Result<(), BoardError> {
    let harness = match BoardActor::parse(grantee.as_str()) {
        Ok(actor) => actor.harness,
        Err(_) => HarnessLabel::parse(grantee.as_str()).map_err(|_| {
            invalid(
                BoardErrorCode::InvalidOptions,
                "grant target must be a full actor identity or harness label",
            )
        })?,
    };
    if harness.is_reserved() {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            format!("grant target cannot name reserved harness {harness}"),
        ));
    }
    Ok(())
}

fn parse_entry_id(id: i64) -> Result<EntryId, BoardError> {
    let id = u64::try_from(id).map_err(|_| {
        invalid(
            BoardErrorCode::BoardUnavailable,
            "grant has an invalid entry id",
        )
    })?;
    EntryId::new(id).map_err(BoardError::from)
}

fn parse_plan_id(id: i64) -> Result<PlanId, BoardError> {
    let id = u64::try_from(id).map_err(|_| {
        invalid(
            BoardErrorCode::BoardUnavailable,
            "grant has an invalid plan id",
        )
    })?;
    PlanId::new(id).map_err(BoardError::from)
}

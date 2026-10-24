//! Owner-created grant and revocation evidence.

use super::*;
use crate::board::board_actor::BoardRecipient;
use crate::board::board_ids::{EntryId, PlanId};
use crate::board::board_protocol::BoardErrorCode;
use crate::board::board_protocol::{
    AuthorityRole, AuthoritySource, BoardError, BoardOp, BoardReply,
};
use crate::board::board_vocabulary::EntryKind;
use rusqlite::{OptionalExtension, Transaction, params};

use super::super::{EntryDraft, WriteContext, insert_entry, insert_event};

pub(in crate::board) fn write_grant(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    op: &BoardOp,
) -> Result<BoardReply, BoardError> {
    let BoardOp::Grant {
        plan,
        roles,
        grantee,
        expires_in,
        note,
    } = op
    else {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "expected grant operation",
        ));
    };
    op.validate().map_err(BoardError::from)?;
    let grantee = grantee.normalize_for_caller(&ctx.actor);
    validate_grantee(&grantee)?;
    if plan.is_none() && (roles.len() != 1 || roles[0] != AuthorityRole::Triage) {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "board-wide grants may contain only triage",
        ));
    }
    let owner_user = match plan {
        Some(plan) => {
            require_plan(tx, *plan)?;
            tx.query_row(
                "SELECT owner_user FROM plans WHERE id=?1",
                [sql_number(plan.get())],
                |row| row.get::<_, String>(0),
            )
            .map_err(sql_error)?
        }
        None => ctx.actor.user.clone(),
    };
    require_owner_human(&ctx.actor, &owner_user, "create grants")?;
    let expires_at = expires_in
        .map(|seconds| {
            let seconds = i64::try_from(seconds).map_err(|_| {
                invalid(
                    BoardErrorCode::InvalidOptions,
                    "grant duration is too large",
                )
            })?;
            if seconds <= 0 {
                return Err(invalid(
                    BoardErrorCode::InvalidOptions,
                    "grant duration must be positive",
                ));
            }
            ctx.now.checked_add(seconds).ok_or_else(|| {
                invalid(
                    BoardErrorCode::InvalidOptions,
                    "grant expiration is too large",
                )
            })
        })
        .transpose()?;
    let roles_text = roles
        .iter()
        .map(|role| role.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let body = match expires_at {
        Some(expiry) => format!(
            "granted {roles_text} to {grantee} until {expiry}{}",
            note_suffix(note.as_ref().map(|note| note.as_str()))
        ),
        None => format!(
            "granted {roles_text} to {grantee} without expiry{}",
            note_suffix(note.as_ref().map(|note| note.as_str()))
        ),
    };
    let body = bounded_entry_text(body);
    let owner_ctx = ctx.with_authority(AuthoritySource::Owner);
    let entry = insert_entry(
        tx,
        &owner_ctx,
        &EntryDraft {
            plan_id: *plan,
            kind: EntryKind::Grant,
            body,
            to_whom: Some(grantee.clone()),
            supersedes: None,
            repo_key: None,
            state: None,
        },
    )?;
    let roles_json = serde_json::to_string(roles)
        .map_err(|error| invalid(BoardErrorCode::InvalidOptions, error.to_string()))?;
    tx.execute(
        concat!(
            "INSERT INTO authority_grants(entry_id,plan_id,owner_user,roles_json,grantee,",
            "granted_at,expires_at,revoked_by_entry,source) VALUES(?1,?2,?3,?4,?5,?6,?7,NULL,'owner')"
        ),
        params![
            sql_number(entry.get()),
            plan.map(|id| sql_number(id.get())),
            owner_user,
            roles_json,
            grantee.as_str(),
            owner_ctx.now,
            expires_at
        ],
    )
    .map_err(sql_error)?;
    let summary = format!("grant {entry}: {roles_text} to {grantee}");
    insert_event(
        tx,
        &owner_ctx,
        *plan,
        EntryKind::Grant,
        &entry.to_string(),
        Some(&grantee),
        &summary,
    )?;
    Ok(owner_ctx.change_reply(entry, *plan, None, None))
}

pub(in crate::board) fn write_revoke(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    op: &BoardOp,
) -> Result<BoardReply, BoardError> {
    let BoardOp::Revoke { entry: grant_entry } = op else {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "expected revoke operation",
        ));
    };
    op.validate().map_err(BoardError::from)?;
    let grant: Option<(Option<i64>, String, Option<i64>, String)> = tx
        .query_row(
            "SELECT plan_id,owner_user,revoked_by_entry,grantee FROM authority_grants WHERE entry_id=?1",
            [sql_number(grant_entry.get())],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let (plan_id, owner_user, revoked_by, grantee) = grant.ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidReference,
            format!("{grant_entry} is not a grant entry"),
        )
    })?;
    let grantee = BoardRecipient::parse(&grantee).map_err(BoardError::from)?;
    require_owner_human(&ctx.actor, &owner_user, "revoke grants")?;
    if let Some(revoked_by) = revoked_by {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{grant_entry} was already revoked by E{revoked_by}"),
        ));
    }
    let plan = plan_id
        .map(|id| {
            let id = u64::try_from(id).map_err(|_| {
                invalid(
                    BoardErrorCode::BoardUnavailable,
                    "grant has an invalid plan id",
                )
            })?;
            PlanId::new(id).map_err(BoardError::from)
        })
        .transpose()?;
    let grant_entry_id = EntryId::new(grant_entry.get()).map_err(BoardError::from)?;
    let owner_ctx = ctx.with_authority(AuthoritySource::Owner);
    let entry = insert_entry(
        tx,
        &owner_ctx,
        &EntryDraft {
            plan_id: plan,
            kind: EntryKind::Revoke,
            body: format!("revoked grant {grant_entry}"),
            to_whom: Some(grantee.clone()),
            supersedes: Some(grant_entry_id),
            repo_key: None,
            state: None,
        },
    )?;
    let updated = tx
        .execute(
            "UPDATE authority_grants SET revoked_by_entry=?1 WHERE entry_id=?2 AND revoked_by_entry IS NULL",
            params![sql_number(entry.get()), sql_number(grant_entry.get())],
        )
        .map_err(sql_error)?;
    if updated != 1 {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{grant_entry} was revoked concurrently"),
        ));
    }
    let summary = format!("revoke {grant_entry}");
    insert_event(
        tx,
        &owner_ctx,
        plan,
        EntryKind::Revoke,
        &entry.to_string(),
        Some(&grantee),
        &summary,
    )?;
    Ok(owner_ctx.change_reply(entry, plan, None, None))
}

fn require_owner_human(
    actor: &crate::board::board_actor::BoardActor,
    owner_user: &str,
    action: &str,
) -> Result<(), BoardError> {
    if actor.user == owner_user && actor.harness.is_human() {
        Ok(())
    } else {
        Err(invalid(
            BoardErrorCode::InvalidActor,
            format!("only owner user {owner_user} acting as human may {action}"),
        ))
    }
}

fn note_suffix(note: Option<&str>) -> String {
    note.map_or_else(String::new, |note| format!(" ({note})"))
}

fn bounded_entry_text(mut text: String) -> String {
    let limit = crate::board::board_vocabulary::ENTRY_TEXT_LIMIT;
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

//! Plan, revision, entry, and claim records share their line-format presentation.

use super::{author, cell, recipient, source_suffix, vendor_suffix};
use crate::board::board_protocol::{
    ClaimRecord, EntryRecord, FeedbackVia, GrantSource, PlanView, RevisionRecord,
};
use crate::board::board_vocabulary::TaskColumn;
use std::fmt::Write;

pub(super) fn plan_lines(text: &mut String, view: &PlanView) {
    writeln!(
        text,
        "permissions: edit={} manage_grants={}",
        view.can_edit, view.can_manage_grants
    )
    .unwrap();
    if !view.grants.is_empty() {
        text.push_str("--- grants ---\n");
        for grant in &view.grants {
            let roles = grant
                .roles
                .iter()
                .map(|role| role.as_str())
                .collect::<Vec<_>>()
                .join(",");
            writeln!(
                text,
                "grant {} plan={} owner={} grantee={} roles={} granted_at={} expires_at={} revoked_by={} source={}",
                grant.entry,
                grant.plan.map_or_else(|| "board".into(), |plan| plan.to_string()),
                cell(&grant.owner_user),
                cell(grant.grantee.as_str()),
                roles,
                grant.granted_at,
                grant.expires_at.map_or_else(|| "never".into(), |at| at.to_string()),
                grant.revoked_by.map_or_else(|| "-".into(), |entry| entry.to_string()),
                match grant.source {
                    GrantSource::Owner => "owner",
                    GrantSource::Migration => "migration",
                }
            )
            .unwrap();
        }
    }
    text.push_str("--- working now ---\n");
    for claim in view.claims.iter().filter(|claim| claim.ended_at.is_none()) {
        claim_line(text, claim);
    }
    text.push_str("--- open for claiming ---\n");
    for task in &view.tasks {
        if view
            .claims
            .iter()
            .any(|claim| claim.task == task.id && claim.ended_at.is_none() && !claim.stale)
        {
            continue;
        }
        if matches!(task.column, TaskColumn::Done | TaskColumn::Review) {
            continue;
        }
        writeln!(
            text,
            "{}\t{}\t{}{}",
            task.id,
            task.column,
            cell(task.title.as_str()),
            task.section
                .as_ref()
                .map_or_else(String::new, |section| format!("\t§ {}", cell(section)))
        )
        .unwrap();
    }
    text.push_str("--- plan sections without tasks ---\n");
    for section in &view.sections_without_tasks {
        writeln!(text, "§ {}", cell(section)).unwrap();
    }
    writeln!(
        text,
        "--- {} {} ---",
        view.revision.id,
        cell(view.plan.title.as_str())
    )
    .unwrap();
    revision_lines(text, &view.revision);
    text.push_str("--- recent evidence ---\n");
    for entry in &view.entries {
        entry_line(text, entry);
    }
}

pub(super) fn revision_lines(text: &mut String, revision: &RevisionRecord) {
    writeln!(
        text,
        "revision {} source={:?} seq={}",
        revision.id, revision.source, revision.seq
    )
    .unwrap();
    // A fixed prefix keeps SSOT lines distinct from controlled grammar/footer lines.
    for line in revision.body.as_str().split_inclusive('\n') {
        writeln!(text, "| {}", cell(line.strip_suffix('\n').unwrap_or(line))).unwrap();
    }
}

pub(in crate::board::board_render) fn entry_line(text: &mut String, entry: &EntryRecord) {
    let source = source_suffix(entry.origin.as_ref(), entry.authority.as_ref());
    let vendor = vendor_suffix(entry.vendor);
    let verdict = entry
        .verdict
        .map_or_else(String::new, |verdict| format!("\tverdict={verdict}"));
    writeln!(
        text,
        "{}\t{}\t{}\t{}{}{}{}{}{}",
        entry.id,
        entry.kind,
        author(
            &entry.actor,
            entry.model.as_deref(),
            entry.effort.as_deref()
        ),
        cell(entry.body.as_str()),
        recipient(&entry.to),
        match entry.via {
            Some(FeedbackVia::Outbox) => "\tvia=outbox spooled unverified",
            None => "",
        },
        verdict,
        vendor,
        source
    )
    .unwrap();
}

pub(in crate::board::board_render) fn claim_line(text: &mut String, claim: &ClaimRecord) {
    let holder = author(
        &claim.actor,
        claim.model.as_deref(),
        claim.effort.as_deref(),
    );
    let holder = match &claim.delegated_by {
        Some(delegator) => format!("{holder} (via {})", cell(&delegator.identity())),
        None => holder,
    };
    writeln!(
        text,
        "{}\t{}\t{} since={} active={}\t{}\t{}{}",
        claim.task,
        holder,
        claim.entry,
        claim.claimed_at,
        claim.last_active,
        if claim.stale {
            "STALE (claimable)"
        } else if claim.ended_at.is_some() {
            "ended"
        } else {
            "active"
        },
        cell(claim.scope.as_str()),
        vendor_suffix(claim.vendor)
    )
    .unwrap();
}

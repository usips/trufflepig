use super::*;
use crate::board::board_protocol::ReadScope;

fn evidence(id: u64) -> EntryRecord {
    serde_json::from_value(serde_json::json!({
        "id": format!("E{id}"), "plan": "P1", "kind": "progress",
        "body": "visible row evidence ".repeat(150), "to": null, "supersedes": null,
        "actor": {"user":"josh","host":"host","harness":"codex","session":"s"},
        "vendor": "codex",
        "model": null, "effort": null, "repo_key": null, "state": null,
        "refs": [], "seq":10, "created_at":100
    }))
    .unwrap()
}

#[test]
fn collection_render_budget_keeps_shared_sequence_entries_reachable() {
    let entries = (1..=4).map(evidence).collect::<Vec<_>>();
    let mut reply = BoardReply::new(
        "local",
        BoardResult::Entries(EntriesPage {
            plan: Some(PlanId::new(1).unwrap()),
            references: None,
            entries,
            after: None,
            through: EventSeq::new(12),
            next_after: None,
            next_before: None,
        }),
    );
    reply.snapshot_seq = Some(EventSeq::new(15));
    let budget = OutputBudget::new(1600)
        .unwrap()
        .with_format(OutputFormat::Json);
    let rendered = render_reply(&reply, &budget).unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let page: EntriesPage = serde_json::from_value(value["result"]["data"].clone()).unwrap();
    assert!(!page.entries.is_empty() && page.entries.len() < 4);
    assert_eq!(
        page.next_before,
        Some(EntryCursor {
            seq: EventSeq::new(10),
            entry: EntryId::new(page.entries.len() as u64).unwrap(),
        })
    );
    assert!(page.next_after.is_none());
    assert_eq!(page.through, EventSeq::new(12));
    assert_eq!(value["snapshot_seq"], 15);
    assert_eq!(value["omitted"]["entries"], 4 - page.entries.len());
}

#[test]
fn collection_render_feedback_keeps_metadata_and_accumulates_omissions() {
    let feedback = (1..=4)
        .map(|id| {
            let mut entry = evidence(id);
            entry.kind = EntryKind::Feedback;
            FeedbackRecord {
                entry,
                kind: crate::board::board_vocabulary::FeedbackKind::Missing,
                state: crate::board::board_vocabulary::FeedbackState::Open,
                metadata: FeedbackMetadata {
                    version: "test-version".into(),
                    cwd: "/checkout".into(),
                    ..FeedbackMetadata::default()
                },
            }
        })
        .collect();
    let reply = BoardReply::new(
        "local",
        BoardResult::Feedback(FeedbackPage {
            open_only: true,
            feedback,
            after: None,
            through: EventSeq::new(20),
            next_after: Some(EntryCursor {
                seq: EventSeq::new(10),
                entry: EntryId::new(4).unwrap(),
            }),
            omitted: 7,
        }),
    );
    let budget = OutputBudget::new(1800)
        .unwrap()
        .with_format(OutputFormat::Json);
    let rendered = render_reply(&reply, &budget).unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let page: FeedbackPage = serde_json::from_value(value["result"]["data"].clone()).unwrap();
    assert!(!page.feedback.is_empty() && page.feedback.len() < 4);
    assert_eq!(page.omitted, 11 - page.feedback.len());
    assert_eq!(
        page.next_after.unwrap().entry.get(),
        page.feedback.len() as u64
    );
    assert_eq!(page.feedback[0].metadata.version, "test-version");
    assert!(page.open_only);
    assert!(
        value["next"]
            .as_str()
            .unwrap()
            .starts_with("feedback ls --open --after ")
    );
}

#[test]
fn collection_attention_keeps_spooled_feedback_provenance_in_json_and_lines() {
    let mut value = serde_json::to_value(evidence(1)).unwrap();
    value["kind"] = serde_json::json!("feedback");
    value["state"] = serde_json::json!({"type":"feedback","state":"open"});
    value["actor"]["harness"] = serde_json::json!("human");
    value["vendor"] = serde_json::json!("human");
    value["via"] = serde_json::json!("outbox");
    let entry: EntryRecord = serde_json::from_value(value).unwrap();
    let reply = BoardReply::new(
        "local",
        BoardResult::Attention(AttentionReply {
            scope: ReadScope::All,
            actor: entry.actor.clone(),
            entries: vec![entry],
            entries_omitted: 0,
            stale_claims: Vec::new(),
            claims_omitted: 0,
            rebase_needed: Vec::new(),
            server_now: 100,
            claim_ttl_secs: 60,
            after: None,
            through: EventSeq::new(10),
            next_after: None,
            claims_next_after: None,
        }),
    );
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let rendered = render_reply(
            &reply,
            &OutputBudget::new(3000).unwrap().with_format(format),
        )
        .unwrap();
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            assert_eq!(value["result"]["data"]["entries"][0]["via"], "outbox");
            assert_eq!(
                value["result"]["data"]["entries"][0]["actor"]["harness"],
                "human"
            );
        } else {
            assert!(rendered.text.contains("via=outbox spooled unverified"));
        }
    }
}

#[test]
fn delegated_claims_render_the_holder_and_its_delegator() {
    use crate::board::board_ids::TaskId;
    let plan = PlanId::new(7).unwrap();
    let delegator = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("kimi").unwrap(),
        "orch",
    )
    .unwrap();
    let reply = BoardReply::new(
        "local",
        BoardResult::Claims(ClaimPage {
            scope: ReadScope::All,
            plan: Some(plan),
            own_stale: false,
            claims: vec![ClaimView {
                claim: ClaimRecord {
                    task: TaskId::new(plan, 3).unwrap(),
                    actor: actor(),
                    vendor: AgentVendor::Codex,
                    entry: EntryId::new(200).unwrap(),
                    scope: EntryText::new("delegated lane").unwrap(),
                    claimed_at: 100,
                    last_active: 120,
                    ended_at: None,
                    end_reason: None,
                    stale: false,
                    model: None,
                    effort: None,
                    delegated_by: Some(delegator),
                },
                cursor: ClaimCursor {
                    entry: EntryId::new(200).unwrap(),
                    claim: 6,
                },
            }],
            after: None,
            through: EventSeq::new(222),
            next_after: None,
            omitted: 0,
            server_now: 150,
            claim_ttl_secs: 60,
        }),
    );
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let rendered = render_reply(
            &reply,
            &OutputBudget::new(1500).unwrap().with_format(format),
        )
        .unwrap();
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            let claim = &value["result"]["data"]["claims"][0]["claim"];
            assert_eq!(claim["actor"]["session"], "c1");
            assert_eq!(claim["delegated_by"]["harness"], "kimi");
            assert_eq!(claim["delegated_by"]["session"], "orch");
        } else {
            assert!(rendered.text.contains("(via josh@laptop/kimi/orch)"));
        }
    }
}

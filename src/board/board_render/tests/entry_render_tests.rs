use super::*;

#[test]
fn entry_drill_keeps_large_proposal_before_long_reverse_references() {
    let body = "complete proposal evidence line\n".repeat(1000);
    let entry = entry(80);
    let related = (100..120)
        .map(|seq| {
            let mut related = self::entry(seq);
            related.kind = EntryKind::Answer;
            related.body = EntryText::new("many tokens of related evidence ".repeat(125)).unwrap();
            related
        })
        .collect::<Vec<_>>();
    let view = EntryView {
        entry: entry.clone(),
        replies: related.clone(),
        replies_omitted: 7,
        backrefs: related,
        backrefs_omitted: 9,
        replies_next_after: None,
        backrefs_next_after: None,
        through: EventSeq::new(119),
        feedback: None,
        linked_commit: None,
        proposal: Some(ProposalRecord {
            entry: entry.id,
            plan: entry.plan.unwrap(),
            base_revision: 1,
            body: crate::board::board_vocabulary::PlanText::new(body.clone()).unwrap(),
            state: ProposalState::Open,
            decision_entry: None,
            result_revision: None,
            stale_base: false,
        }),
        can_decide: true,
        can_supersede: true,
        can_answer: false,
        can_triage: false,
        can_close: false,
        plan_head_revision: Some(1),
    };
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(32768).unwrap().with_format(format);
        let rendered = render_reply(
            &BoardReply::new("local", BoardResult::Entry(view.clone())),
            &budget,
        )
        .unwrap();
        assert!(budget.fits(&rendered.text));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            assert_eq!(value["result"]["data"]["proposal"]["body"], body);
            let replies = value["result"]["data"]["replies"].as_array().unwrap().len();
            let backrefs = value["result"]["data"]["backrefs"]
                .as_array()
                .unwrap()
                .len();
            assert!(replies + backrefs < 40);
            assert_eq!(value["result"]["data"]["replies_omitted"], 27 - replies);
            assert_eq!(value["result"]["data"]["backrefs_omitted"], 29 - backrefs);
            assert_eq!(value["omitted"]["body_lines"], 0);
        } else {
            assert_eq!(
                rendered
                    .text
                    .matches("| complete proposal evidence line")
                    .count(),
                1000
            );
            assert!(rendered.text.contains("omitted: replies="));
        }
        let short = render_reply(
            &BoardReply::new("local", BoardResult::Entry(view.clone())),
            &OutputBudget::new(600).unwrap().with_format(format),
        )
        .unwrap();
        assert!(short.text.contains("board show E80 -b 32768"));
    }
}

#[test]
fn claim_line_shows_the_lease_entry() {
    use crate::board::board_ids::TaskId;
    let plan = PlanId::new(7).unwrap();
    let claim = ClaimRecord {
        task: TaskId::new(plan, 3).unwrap(),
        actor: BoardActor::new("josh", "laptop", HarnessLabel::parse("codex").unwrap(), "s")
            .unwrap(),
        vendor: AgentVendor::Codex,
        entry: EntryId::new(12).unwrap(),
        scope: EntryText::new("parser only").unwrap(),
        claimed_at: 100,
        last_active: 120,
        ended_at: None,
        end_reason: None,
        stale: false,
        model: Some("claimed-model".into()),
        effort: Some("xhigh".into()),
        delegated_by: None,
    };
    let mut rendered = String::new();
    claim_line(&mut rendered, &claim);
    assert_eq!(
        rendered,
        "P7.3\tjosh@laptop/codex/s(claimed-model/xhigh)\tE12 since=100 active=120\tactive\tparser only\n"
    );
}

#[test]
fn spooled_entry_lines_mark_imported_claims_unverified() {
    let mut imported = entry(1);
    imported.via = Some(FeedbackVia::Outbox);
    let mut rendered = String::new();
    entry_line(&mut rendered, &imported);
    assert!(rendered.contains("via=outbox spooled unverified"));
    imported.via = None;
    rendered.clear();
    entry_line(&mut rendered, &imported);
    assert!(!rendered.contains("spooled"));
}

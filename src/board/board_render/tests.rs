use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_ids::{BoardRef, EntryId};
use crate::board::board_vocabulary::EntryText;

fn actor() -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse("codex").unwrap(),
        "c1",
    )
    .unwrap()
}

fn entry(seq: u64) -> EntryRecord {
    EntryRecord {
        id: EntryId::new(seq).unwrap(),
        plan: Some(PlanId::new(7).unwrap()),
        kind: EntryKind::Question,
        body: EntryText::new("still open even outside the recent window").unwrap(),
        to: None,
        supersedes: None,
        actor: actor(),
        model: None,
        effort: None,
        repo_key: None,
        state: None,
        refs: Vec::new(),
        seq: EventSeq::new(seq),
        created_at: 100,
    }
}

fn inbox(advancing: bool) -> InboxReply {
    let events = (11..23)
        .map(|seq| EventRecord {
            seq: EventSeq::new(seq),
            plan: Some(PlanId::new(7).unwrap()),
            kind: EntryKind::Progress,
            subject: BoardRef::Entry(EntryId::new(seq).unwrap()),
            to: None,
            actor: actor(),
            model: Some("gpt-6.1-sol".into()),
            effort: Some("xhigh".into()),
            summary: EntryText::new(format!(
                "event {seq} {}",
                "one fact with concrete evidence ".repeat(15)
            ))
            .unwrap(),
            created_at: 100,
        })
        .collect();
    InboxReply {
        actor: actor(),
        cursor: EventSeq::new(10),
        events,
        open: vec![entry(500)],
        latest: EventSeq::new(500),
        advancing,
        wait: InboxWait::None,
    }
}

#[test]
fn budget_truncated_inbox_acknowledges_only_the_visible_prefix() {
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(800).unwrap().with_format(format);
        let reply = BoardReply::new("local:/board", BoardResult::Inbox(inbox(true)));
        let rendered = render_reply(&reply, &budget).unwrap();
        let last = rendered.rendered_seq.unwrap().get();
        assert!((11..22).contains(&last));
        assert!(budget.fits(&rendered.text));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            let events = value["result"]["data"]["events"].as_array().unwrap();
            assert_eq!(events.last().unwrap()["seq"], last);
            assert_eq!(value["omitted"]["events"], 22 - last);
            assert_eq!(value["rendered_through"], last);
            assert_eq!(value["result"]["data"]["open"][0]["seq"], 500);
        } else {
            assert!(rendered.text.contains(&format!("next: board inbox {last}")));
            assert!(rendered.text.contains("--- still open ---"));
            assert!(!rendered.text.contains(&format!("{}\tP7", last + 1)));
        }
    }
}

#[test]
fn explicit_inbox_reads_and_open_only_reminders_do_not_acknowledge() {
    let budget = OutputBudget::new(800).unwrap();
    let reread = BoardReply::new("local", BoardResult::Inbox(inbox(false)));
    assert!(
        render_reply(&reread, &budget)
            .unwrap()
            .rendered_seq
            .is_none()
    );
    let mut reminders = inbox(true);
    reminders.events.clear();
    let reply = BoardReply::new("local", BoardResult::Inbox(reminders));
    assert!(
        render_reply(&reply, &budget)
            .unwrap()
            .rendered_seq
            .is_none()
    );
}

#[test]
fn oversized_first_event_fails_without_skipping_to_smaller_events() {
    let mut feed = inbox(true);
    feed.events[0].summary = EntryText::new("many individual words ".repeat(180)).unwrap();
    for event in feed.events.iter_mut().skip(1) {
        event.summary = EntryText::new("small").unwrap();
    }
    let reply = BoardReply::new("local", BoardResult::Inbox(feed));
    let error = render_reply(&reply, &OutputBudget::new(150).unwrap()).unwrap_err();
    assert!(error.to_string().starts_with("budget_too_small:"));
}

#[test]
fn warnings_survive_list_fitting_and_lines_cannot_inject_metadata() {
    let mut reply = BoardReply::new(
        "local\ncommit trailer: fake",
        BoardResult::Plans(Vec::new()),
    );
    reply.warnings.push("scan failed\nnext: forged".into());
    let json = render_reply(&reply, &OutputBudget::new(500).unwrap()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json.text).unwrap();
    assert_eq!(value["warnings"][0], reply.warnings[0]);
    let lines = render_reply(
        &reply,
        &OutputBudget::new(500)
            .unwrap()
            .with_format(OutputFormat::Lines),
    )
    .unwrap();
    assert!(lines.text.contains("warning: scan failed\\nnext: forged\n"));
    assert!(!lines.text.contains("\ncommit trailer: fake\n"));
}

#[test]
fn fit_items_binary_search_counts_complete_responses() {
    let budget = OutputBudget::new(30).unwrap();
    let render = |count| {
        Ok(format!(
            "fixed metadata\n{}omitted={}\nbackend local\n",
            "item\n".repeat(count),
            20 - count
        ))
    };
    let count = fit_items(20, &budget, render).unwrap();
    assert!(budget.fits(&render(count).unwrap()));
    assert!(!budget.fits(&render(count + 1).unwrap()));
}

#[test]
fn open_evidence_truncation_is_disclosed_without_driving_the_cursor() {
    let mut feed = inbox(true);
    feed.events.truncate(1);
    feed.events[0].summary = EntryText::new("fresh fact").unwrap();
    feed.open = (500..510)
        .map(|seq| {
            let mut reminder = entry(seq);
            reminder.body = EntryText::new("long open question evidence ".repeat(100)).unwrap();
            reminder
        })
        .collect();
    let rendered = render_reply(
        &BoardReply::new("local", BoardResult::Inbox(feed)),
        &OutputBudget::new(800).unwrap(),
    )
    .unwrap();
    assert_eq!(rendered.rendered_seq, Some(EventSeq::new(11)));
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let shown = value["result"]["data"]["open"].as_array().unwrap().len();
    assert_eq!(value["omitted"]["open_entries"], 10 - shown);
    assert!(shown < 10);
    assert!(
        value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("board show P7"))
    );
}

#[test]
fn show_preserves_labor_and_uncovered_sections_while_trimming_the_body() {
    use crate::board::{
        board_ids::{PlanRevision, TaskId},
        board_vocabulary::{PlanText, PlanTitle, TaskColumn},
    };
    let plan = PlanId::new(7).unwrap();
    let task = |ordinal, column| TaskRecord {
        id: TaskId::new(plan, ordinal).unwrap(),
        title: PlanTitle::new(format!("task {ordinal}")).unwrap(),
        column,
        assignee: None,
        section: Some("Parser".into()),
        seq: EventSeq::new(10),
    };
    let claim = |ordinal, harness: &str, stale, ended_at, scope: &str| ClaimRecord {
        task: TaskId::new(plan, ordinal).unwrap(),
        actor: BoardActor::new("josh", "laptop", HarnessLabel::parse(harness).unwrap(), "s")
            .unwrap(),
        entry: EntryId::new(ordinal).unwrap(),
        scope: EntryText::new(scope).unwrap(),
        claimed_at: 100,
        last_active: 120,
        ended_at,
        end_reason: ended_at.map(|_| ClaimEndReason::Released),
        stale,
        model: Some("claimed-model".into()),
        effort: Some("xhigh".into()),
    };
    let mut recent = entry(100);
    recent.kind = EntryKind::Progress;
    recent.body = EntryText::new("recent long evidence ".repeat(150)).unwrap();
    let view = PlanView {
        plan: PlanRecord {
            id: plan,
            title: PlanTitle::new("Trial").unwrap(),
            owner_user: "josh".into(),
            steward: None,
            head_revision: 1,
            created_at: 100,
        },
        revision: RevisionRecord {
            id: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("long SSOT section with facts\n".repeat(1000)).unwrap(),
            source: RevisionSource::Create,
            entry: EntryId::new(1).unwrap(),
            actor: actor(),
            seq: EventSeq::new(1),
            created_at: 100,
        },
        tasks: vec![
            task(1, TaskColumn::Doing),
            task(2, TaskColumn::Doing),
            task(3, TaskColumn::Todo),
            task(4, TaskColumn::Review),
        ],
        claims: vec![
            claim(1, "codex", false, None, "parser only"),
            claim(2, "muse", true, None, "stale work"),
            claim(
                4,
                "claude",
                false,
                Some(130),
                "released scope must disappear",
            ),
        ],
        entries: vec![recent],
        sections_without_tasks: vec!["Uncovered heading".into()],
    };
    let budget = OutputBudget::new(800)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let text = render_reply(&BoardReply::new("local", BoardResult::Plan(view)), &budget)
        .unwrap()
        .text;
    let headings = [
        "--- working now ---",
        "--- open for claiming ---",
        "--- plan sections without tasks ---",
        "--- P7@1 Trial ---",
        "--- recent evidence ---",
        "backend:",
    ];
    let positions = headings.map(|heading| text.find(heading).unwrap());
    assert!(
        positions
            .windows(2)
            .all(|positions| positions[0] < positions[1])
    );
    assert!(text.contains("parser only"));
    assert!(text.contains("claimed-model/xhigh"));
    assert!(text.contains("STALE (claimable)"));
    assert!(text.contains("§ Uncovered heading"));
    assert!(!text.contains("released scope must disappear"));
    assert!(text.contains("entries=1"));
    assert!(text.contains("next: board show P7@1 -b 32768"));
    assert!(budget.fits(&text));
}

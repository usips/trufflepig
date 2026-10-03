use super::*;

#[test]
fn budget_truncated_inbox_acknowledges_only_the_visible_prefix() {
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(800).unwrap().with_format(format);
        let reply = BoardReply::new("local:/board", BoardResult::Inbox(inbox(true)));
        let rendered = render_reply(&reply, &budget).unwrap();
        let last = rendered.acknowledge_seq.unwrap().get();
        assert!((11..22).contains(&last));
        assert!(budget.fits(&rendered.text));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            let events = value["result"]["data"]["events"].as_array().unwrap();
            assert_eq!(events.last().unwrap()["seq"], last);
            assert_eq!(value["omitted"]["events"], 22 - last);
            assert_eq!(value["rendered_through"], last);
            let shown_open = value["result"]["data"]["open"].as_array().unwrap().len();
            assert_eq!(value["omitted"]["open_entries"], 1 - shown_open);
        } else {
            assert!(rendered.text.contains("next: board inbox --all"));
            assert!(rendered.text.contains("omitted: events="));
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
            .acknowledge_seq
            .is_none()
    );
    let mut reminders = inbox(true);
    reminders.events.clear();
    reminders.scanned_through = reminders.cursor;
    let reply = BoardReply::new("local", BoardResult::Inbox(reminders));
    assert!(
        render_reply(&reply, &budget)
            .unwrap()
            .acknowledge_seq
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
fn open_evidence_truncation_is_disclosed_without_driving_the_cursor() {
    let mut feed = inbox(true);
    feed.events.truncate(1);
    feed.scanned_through = EventSeq::new(11);
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
    assert_eq!(rendered.acknowledge_seq, Some(EventSeq::new(11)));
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
fn busy_inbox_uses_the_wire_state_name_in_lines() {
    let mut feed = inbox(false);
    feed.events.clear();
    feed.wait = InboxWait::Busy;
    let budget = OutputBudget::new(800)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered =
        render_reply(&BoardReply::new("local", BoardResult::Inbox(feed)), &budget).unwrap();
    assert!(rendered.text.contains("wait=busy\n"));
}

#[test]
fn fresh_events_fit_before_oversized_reminders() {
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let mut feed = inbox(true);
        feed.events.truncate(2);
        feed.scanned_through = EventSeq::new(12);
        for event in &mut feed.events {
            event.summary = EntryText::new("fresh fact").unwrap();
        }
        feed.open[0].body = EntryText::new("large unresolved evidence ".repeat(150)).unwrap();
        feed.open_omitted = 23;
        let rendered = render_reply(
            &BoardReply::new("local", BoardResult::Inbox(feed)),
            &OutputBudget::new(600).unwrap().with_format(format),
        )
        .unwrap();
        assert_eq!(rendered.acknowledge_seq, Some(EventSeq::new(12)));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            assert_eq!(value["omitted"]["events"], 0);
            assert_eq!(value["omitted"]["open_entries"], 24);
        } else {
            assert!(rendered.text.contains("open_entries=24"));
        }
        assert!(rendered.text.contains("--all"));
    }
}

#[test]
fn complete_empty_inbox_acknowledges_scan_without_claiming_rendered_events() {
    let mut feed = inbox(true);
    feed.events.clear();
    feed.open.clear();
    feed.scanned_through = EventSeq::new(42);
    let reply = BoardReply::new("local", BoardResult::Inbox(feed.clone()));
    let rendered = render_reply(&reply, &OutputBudget::new(600).unwrap()).unwrap();
    assert_eq!(rendered.acknowledge_seq, Some(EventSeq::new(42)));
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    assert!(value["rendered_through"].is_null());
    assert_eq!(value["result"]["data"]["scanned_through"], 42);
    feed.advancing = false;
    let rendered = render_reply(
        &BoardReply::new("local", BoardResult::Inbox(feed)),
        &OutputBudget::new(600).unwrap(),
    )
    .unwrap();
    assert!(rendered.acknowledge_seq.is_none());
}

#[test]
fn query_truncated_inbox_acknowledges_only_actual_events() {
    let mut feed = inbox(true);
    feed.events.truncate(1);
    feed.events[0].seq = EventSeq::new(3);
    feed.cursor = EventSeq::new(1);
    feed.scanned_through = EventSeq::new(5);
    feed.query_truncated = true;
    feed.open.clear();
    let rendered = render_reply(
        &BoardReply::new("local", BoardResult::Inbox(feed)),
        &OutputBudget::new(800).unwrap(),
    )
    .unwrap();
    assert_eq!(rendered.acknowledge_seq, Some(EventSeq::new(3)));
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    assert_eq!(value["rendered_through"], 3);
    assert_eq!(value["next"], "board inbox --all");
}

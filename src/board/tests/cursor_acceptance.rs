use super::*;
use std::time::Duration;

#[test]
fn explicit_inbox_replays_and_open_reminders_never_advance_the_session_cursor() {
    let fixture = EdgeFixture::new();
    fixture.run(&["board", "new", "Cursor acceptance"], "codex", "writer");
    fixture.run(&["board", "inbox"], "claude", "reader");
    assert_eq!(fixture.cursor("claude", "reader"), 1);
    for index in 0..6 {
        fixture.run(
            &[
                "board",
                "post",
                "P1",
                "progress",
                &format!("Progress {index}"),
            ],
            "codex",
            "writer",
        );
    }
    fixture.run(
        &[
            "board",
            "post",
            "P1",
            "question",
            "Open reminder",
            "--to",
            "claude",
        ],
        "codex",
        "writer",
    );
    let mut options = fixture.options(&["board", "inbox", "0"]);
    options.limit = 2;
    let replay = fixture
        .run_options(options, "claude", "reader", None)
        .unwrap();
    assert_eq!(data(&replay)["events"].as_array().unwrap().len(), 2);
    assert_eq!(fixture.cursor("claude", "reader"), 1);

    let mut options = fixture.options(&["board", "inbox"]);
    options.limit = 2;
    let first = fixture
        .run_options(options.clone(), "claude", "reader", None)
        .unwrap();
    let inbox = data(&first);
    let rendered = inbox["events"].as_array().unwrap();
    assert_eq!(rendered.len(), 2);
    let through = rendered.last().unwrap()["seq"].as_u64().unwrap();
    assert_eq!(fixture.cursor("claude", "reader"), through);
    assert!(
        inbox["open"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["seq"].as_u64().unwrap() > through)
    );
    let second = fixture
        .run_options(options, "claude", "reader", None)
        .unwrap();
    assert!(
        data(&second)["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["seq"].as_u64().unwrap() > through)
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM events"),
        8,
        "acknowledgement minted an event"
    );
}

#[test]
fn failed_complete_render_does_not_acknowledge_or_skip_events() {
    let fixture = EdgeFixture::new();
    fixture.run(&["board", "new", "Rendering failure"], "codex", "writer");
    fixture.run(
        &["board", "post", "P1", "progress", "Unread evidence"],
        "codex",
        "writer",
    );
    let mut too_small = fixture.options(&["board", "inbox"]);
    too_small.budget = 1;
    let error = fixture
        .run_options(too_small, "claude", "budget-reader", None)
        .unwrap_err();
    assert!(
        error.to_string().starts_with("budget_too_small:"),
        "{error:#}"
    );
    assert_eq!(fixture.cursor("claude", "budget-reader"), 0);
    let recovered = fixture.run(&["board", "inbox"], "claude", "budget-reader");
    assert_eq!(data(&recovered)["events"].as_array().unwrap().len(), 2);
    assert_eq!(fixture.cursor("claude", "budget-reader"), 2);
}

#[test]
fn short_router_deadline_returns_empty_timeout_without_spending_the_worker_budget() {
    let fixture = EdgeFixture::new();
    let options = fixture.options(&["board", "inbox", "0", "--wait"]);
    fixture.run(&["board", "inbox", "0"], "codex", "wait-reader");
    let started = std::time::Instant::now();
    let output = fixture
        .host
        .run(
            &options,
            &context("codex", "wait-reader"),
            QueryDeadline::after(Duration::from_millis(500)),
        )
        .unwrap();
    let reply: Value = serde_json::from_str(&output).unwrap();
    assert!(data(&reply)["events"].as_array().unwrap().is_empty());
    assert_eq!(data(&reply)["wait"], "timeout");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(fixture.cursor("codex", "wait-reader"), 0);
}

#[test]
fn expired_accepted_board_write_never_opens_storage_or_commits_an_event() {
    let fixture = EdgeFixture::new();
    let error = fixture
        .host
        .run(
            &fixture.options(&["board", "new", "Expired request"]),
            &context("codex", "expired-writer"),
            QueryDeadline::after(Duration::ZERO),
        )
        .unwrap_err();
    assert!(crate::daemon::deadline::is_timed_out(&error), "{error:#}");
    assert!(
        !fixture.config.db_path.exists(),
        "expired accepted mutation opened durable storage"
    );
}

#[test]
fn a_waiting_inbox_receives_a_host_write_before_the_one_second_poll() {
    let fixture = EdgeFixture::new();
    fixture.run(&["board", "new", "Wait notification"], "codex", "writer");
    let host = fixture.host.clone();
    let options = fixture.options(&["board", "inbox", "1", "--wait"]);
    let waiting = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let output = host
            .run(
                &options,
                &context("claude", "wake-reader"),
                QueryDeadline::after(Duration::from_secs(4)),
            )
            .unwrap();
        (
            started.elapsed(),
            serde_json::from_str::<Value>(&output).unwrap(),
        )
    });
    // Let the initial empty query enter the wait; no daemon or process env changes.
    std::thread::sleep(Duration::from_millis(75));
    fixture.run(
        &["board", "post", "P1", "progress", "Wake up with evidence"],
        "codex",
        "writer",
    );
    let (elapsed, reply) = waiting.join().unwrap();
    assert_eq!(data(&reply)["wait"], "ready");
    assert_eq!(data(&reply)["events"].as_array().unwrap().len(), 1);
    assert!(
        elapsed < Duration::from_millis(900),
        "writer notification waited for polling: {elapsed:?}"
    );
}

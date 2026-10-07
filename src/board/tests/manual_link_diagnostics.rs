use super::*;
use crate::board::board_protocol::{BoardError, BoardErrorCode};

fn link_diagnostics_fixture() -> (EdgeFixture, String) {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(
        &["board", "new", "Link diagnostics", "--steward", "codex"],
        "human",
        "owner",
    );
    fixture.run(&["board", "task", "P1", "Linked lane"], "human", "owner");
    let oid = git(&fixture.root, &["rev-parse", "HEAD"]);
    (fixture, oid)
}

#[test]
fn manual_link_reports_a_missing_repository_on_the_actor_host() {
    let (fixture, oid) = link_diagnostics_fixture();
    for ordinal in 2..=5 {
        let title = format!("Another plan {ordinal}");
        fixture.run(&["board", "new", &title], "human", "owner");
    }
    fixture.run(&["board", "task", "P5", "Remote lane"], "human", "owner");
    let mut config = fixture.config.clone();
    config.host = "unregistered-host".into();
    let remote_host = BoardHost::with_config(config);
    let repositories_before = fixture.scalar("SELECT COUNT(*) FROM repo_paths");
    assert!(repositories_before > 0);
    let events_before = fixture.scalar("SELECT COUNT(*) FROM events");
    let error = remote_host
        .run(
            &fixture.options(&["board", "link", &oid, "P5.1"]),
            &context("human", "owner"),
            QueryDeadline::start(),
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<BoardError>().unwrap().code,
        BoardErrorCode::InvalidReference,
    );
    assert_eq!(
        error.to_string(),
        "invalid_reference: P5 has no registered repository on unregistered-host; run any P5 board write from the checkout first",
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM repo_paths"),
        repositories_before
    );
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM events"), events_before);
}

#[test]
fn manual_link_checks_authority_before_git_resolution() {
    let (fixture, _) = link_diagnostics_fixture();
    git(
        &fixture.root,
        &["tag", "-a", "release", "-m", "tagged release"],
    );
    let tag = git(&fixture.root, &["rev-parse", "release"]);
    let events_before = fixture.scalar("SELECT COUNT(*) FROM events");
    let error = fixture
        .run_options(
            fixture.options(&["board", "link", &tag, "P1.1"]),
            "kimi",
            "unauthorized",
            None,
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<BoardError>().unwrap().code,
        BoardErrorCode::InvalidActor,
        "{error:#}",
    );
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM events"), events_before);
}

#[test]
fn manual_link_replay_returns_its_own_task_event_sequence() {
    let (fixture, oid) = link_diagnostics_fixture();
    fixture.run(&["board", "task", "P1", "Second lane"], "human", "owner");
    let first = fixture.run(&["board", "link", &oid, "P1.1"], "human", "first-linker");
    let second = fixture.run(&["board", "link", &oid, "P1.2"], "codex", "second-linker");
    assert_eq!(data(&first)["entry"], data(&second)["entry"]);
    assert_ne!(data(&first)["seq"], data(&second)["seq"]);
    let durable_seq = fixture.scalar("SELECT link_seq FROM commit_tasks WHERE task_ordinal=2");
    assert_eq!(data(&second)["seq"], durable_seq);
    fixture.run(
        &[
            "board",
            "post",
            "P1",
            "progress",
            "Advance the board sequence",
        ],
        "human",
        "owner",
    );
    let events_before = fixture.scalar("SELECT COUNT(*) FROM events");
    let reopened = BoardHost::with_config(fixture.config.clone());
    let output = reopened
        .run(
            &fixture.options(&["board", "link", &oid, "P1.2"]),
            &context("human", "replay"),
            QueryDeadline::start(),
        )
        .unwrap();
    let replay: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(data(&replay)["deduplicated"], true);
    assert_eq!(data(&replay)["entry"], data(&second)["entry"]);
    assert_eq!(data(&replay)["seq"], durable_seq);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM events"), events_before);
}

#[test]
fn manual_link_cli_rejection_explains_the_human_client_flag() {
    let (fixture, oid) = link_diagnostics_fixture();
    let error = fixture
        .run_options(
            fixture.options(&["board", "link", &oid, "P1.1"]),
            "cli",
            "terminal",
            None,
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<BoardError>().unwrap().code,
        BoardErrorCode::InvalidActor,
    );
    assert!(
        error.to_string().contains("pass --client human"),
        "{error:#}"
    );
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 0);
}

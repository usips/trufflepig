use super::*;
use crate::board::{
    board_actor::BoardActor,
    board_backend::BoardBackend,
    board_protocol::{BoardOp, BoardRequest, BoardResult},
    board_vocabulary::{PlanText, PlanTitle},
    local_board::LocalBoard,
};

fn reply_timeout() -> anyhow::Error {
    crate::daemon_transport::DaemonReplyTimeout::new(
        Duration::from_secs(30),
        Duration::from_secs(30),
    )
    .into()
}

fn capability_status(database: &Path) -> String {
    serde_json::json!({
        "status": "ok",
        "board_api": BOARD_API,
        "board_db": database,
    })
    .to_string()
}

fn timeout_gateway(database: &Path) -> FakeGateway {
    FakeGateway {
        replies: VecDeque::from([Ok(Some(capability_status(database))), Err(reply_timeout())]),
        ..Default::default()
    }
}

#[test]
fn ambiguous_mutation_timeout_prints_a_receipt_key_without_retrying() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = timeout_gateway(&database);
    let error = invoke(
        &["board", "new", "Ambiguous write"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
    )
    .unwrap_err();
    let message = error.to_string();

    assert!(
        message.starts_with("daemon_reply_timeout: phase=awaiting_reply"),
        "{message}"
    );
    assert!(message.contains("board_write_outcome=unknown"), "{message}");
    assert!(message.contains("request_key="), "{message}");
    assert!(message.contains("board receipt"), "{message}");
    assert!(message.contains("do not automatically replay"), "{message}");
    assert_eq!(
        gateway.requests.len(),
        2,
        "ambiguous writes are not retried"
    );
    assert!(!database.exists());
}

#[test]
fn session_timeout_does_not_advertise_a_boardchange_receipt_key() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = timeout_gateway(&database);
    let error = invoke(
        &["board", "hello", "gpt-6-luna"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
    )
    .unwrap_err();
    let message = error.to_string();

    assert!(
        message.starts_with("daemon_reply_timeout: phase=awaiting_reply"),
        "{message}"
    );
    assert!(message.contains("board_write_outcome=unknown"), "{message}");
    assert!(message.contains("no BoardChange receipt key"), "{message}");
    assert!(!message.contains("request_key="), "{message}");
    assert_eq!(gateway.requests.len(), 2);
}

#[test]
fn read_timeout_keeps_its_read_only_error_without_a_write_receipt() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = timeout_gateway(&database);
    let error = invoke(
        &["board", "show"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
    )
    .unwrap_err();
    let message = error.to_string();

    assert!(
        message.starts_with("daemon_reply_timeout: phase=awaiting_reply"),
        "{message}"
    );
    assert!(!message.contains("board_write_outcome"), "{message}");
    assert!(!message.contains("request_key="), "{message}");
    assert_eq!(gateway.requests.len(), 2);
}

#[test]
fn ambiguous_feedback_timeout_still_uses_its_durable_outbox() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let args = feedback_args(directory.path(), "ambiguous feedback", "body preserved");
    let mut gateway = timeout_gateway(&database);
    let reply = invoke_args_with_load_delay(
        &args,
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
        QueryDeadline::after(Duration::from_secs(30)),
        Duration::ZERO,
    )
    .unwrap();

    let key = assert_queued_reply(&reply);
    assert_queued_feedback(
        &directory.path().join("spool"),
        "ambiguous feedback",
        "body preserved",
    );
    assert_eq!(gateway.requests.len(), 2);
    assert!(!database.exists());
    assert!(!key.is_empty());
}

fn create_plan(database: &Path, actor: BoardActor) -> crate::board::board_ids::PlanId {
    let mut board = LocalBoard::open_path(database, Duration::from_secs(120)).unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::New {
                title: PlanTitle::new("Client receipt integration").unwrap(),
                body: PlanText::new("# Receipt").unwrap(),
                repo_key: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected plan creation change");
    };
    change.plan.unwrap()
}

#[test]
fn client_post_key_matches_the_actual_host_receipt_with_claims_and_origin() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let config = BoardConfig::for_database(&database);
    let actor = config
        .actor(Some("human"), Some("receipt-integration"))
        .unwrap();
    let plan = create_plan(&database, actor.clone());
    let context = RequestContext::with_origin(
        Some("receipt-integration".into()),
        Some("human".into()),
        Some(actor.origin()),
    );
    let words = [
        "board".to_owned(),
        "post".to_owned(),
        format!("P{}", plan.get()),
        "note".to_owned(),
        "host-shaped request".to_owned(),
    ];
    let mut options = crate::cli::parse(&words).unwrap();
    options.root = directory.path().to_path_buf();
    options.board.agent_model = Some("gpt-6-luna".into());
    options.board.agent_effort = Some("xhigh".into());
    let command = board_grammar::parse(&options, None).unwrap();
    let key = request_receipt_key(&command, &options, &context, &mut || Ok(config.clone()))
        .unwrap()
        .unwrap();

    let host = BoardHost::with_config(config);
    host.run(
        &options,
        &context,
        QueryDeadline::after(Duration::from_secs(30)),
    )
    .unwrap();

    let receipt_words = ["board".to_owned(), "receipt".to_owned(), key.to_string()];
    let mut receipt_options = crate::cli::parse(&receipt_words).unwrap();
    receipt_options.root = directory.path().to_path_buf();
    let result = host
        .run(
            &receipt_options,
            &context,
            QueryDeadline::after(Duration::from_secs(30)),
        )
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["result"]["result"], "receipt");
    assert_eq!(result["result"]["data"]["key"], key.to_string());
    assert_eq!(result["result"]["data"]["state"], "committed");
    assert!(!result["result"]["data"]["change"]["entry"].is_null());

    let other_actor = BoardActor::new(
        "test",
        "localhost",
        crate::request_context::HarnessLabel::parse("codex").unwrap(),
        "other-session",
    )
    .unwrap();
    let other_context = RequestContext::with_origin(
        Some("other-session".into()),
        Some("codex".into()),
        Some(other_actor.origin()),
    );
    let hidden = host
        .run(
            &receipt_options,
            &other_context,
            QueryDeadline::after(Duration::from_secs(30)),
        )
        .unwrap();
    let hidden: serde_json::Value = serde_json::from_str(&hidden).unwrap();
    assert_eq!(hidden["result"]["data"]["state"], "unknown");
    assert!(hidden["result"]["data"]["change"].is_null());
}

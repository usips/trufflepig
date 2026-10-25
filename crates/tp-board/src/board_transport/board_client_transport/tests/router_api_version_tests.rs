use super::*;

#[test]
fn api_7_router_gets_restart_hint() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let status = serde_json::json!({
        "status": "ok", "board_api": 7, "board_db": database
    })
    .to_string();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(Some(status)), Ok(Some("dispatched".into()))]),
        ..Default::default()
    };
    let result = invoke(
        &["board", "show"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
    );
    assert!(result.is_err(), "API 7 router must be refused: {result:?}");
    let error = result.unwrap_err();
    assert!(
        error.to_string().starts_with("board_api_mismatch:"),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("restart trufflepig-system.service"),
        "{error}"
    );
    assert_eq!(gateway.requests.len(), 1);
    assert_eq!(gateway.ensured, 0);
    assert!(!database.exists());
}

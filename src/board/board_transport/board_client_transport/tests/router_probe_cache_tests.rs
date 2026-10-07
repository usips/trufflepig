mod cli_retry_probe;

use super::*;

#[test]
fn router_api_is_probed_once_before_dispatch_and_mismatch_never_falls_back() {
    if cli_retry_probe::run_isolated() {
        return;
    }
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let status =
        serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":database}).to_string();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([
            Ok(Some(status)),
            Ok(Some("first".into())),
            Ok(Some("second".into())),
        ]),
        ..Default::default()
    };
    let mut transport = BoardClientTransport::default();
    assert_eq!(
        invoke(
            &["board", "show"],
            &mut gateway,
            &mut transport,
            &database,
            None
        )
        .unwrap(),
        "first"
    );
    assert_eq!(
        invoke(
            &["board", "show"],
            &mut gateway,
            &mut transport,
            &database,
            None
        )
        .unwrap(),
        "second"
    );
    assert_eq!(gateway.requests.len(), 3);
    assert_eq!(gateway.requests[0], ["system", "status"]);
    assert!(!database.exists());
    for stale in [4, BOARD_API + 1] {
        let status =
            serde_json::json!({"status":"ok","board_api":stale,"board_db":database}).to_string();
        let mut gateway = FakeGateway {
            replies: VecDeque::from([Ok(Some(status))]),
            ..Default::default()
        };
        let error = invoke(
            &["board", "new", "Never written"],
            &mut gateway,
            &mut BoardClientTransport::default(),
            &database,
            None,
        )
        .unwrap_err();
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
}

use super::super::web_serve::{BoardWebServer, PublishedEndpoint};
use std::{path::PathBuf, sync::OnceLock};

const CHILD: &str = "TRUFFLEPIG_SERVE_LOCK_TEST_CHILD";

#[test]
fn second_bind_refuses_and_leaves_token_untouched() {
    if std::env::var_os(CHILD).is_none() {
        let directory = crate::board::board_test_support::scratch("serve-lock-");
        let runtime = directory.path().join("runtime");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env(CHILD, "1")
            .env("TRUFFLEPIG_SYSTEM_DIR", &runtime)
            .env("TRUFFLEPIG_BOARD_DB", directory.path().join("web.sqlite3"))
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .args([
                "board::board_web::serve_lock::tests::second_bind_refuses_and_leaves_token_untouched",
                "--exact",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            runtime.join("second-bind-refused").exists(),
            "the child ran the refusal check"
        );
        return;
    }
    let runtime = PathBuf::from(std::env::var_os("TRUFFLEPIG_SYSTEM_DIR").unwrap());
    let published: PublishedEndpoint = OnceLock::new();
    let first = BoardWebServer::bind("127.0.0.1:0".parse().unwrap(), &published).unwrap();
    // Bind hands the descriptor to the pre-bind signal waiter through the lock.
    assert_eq!(
        published.get().map(|(path, _)| path),
        Some(&runtime.join("board-web.json")),
    );
    let token_path = runtime.join("board-web.token");
    let before = std::fs::read(&token_path).unwrap();
    let error = BoardWebServer::bind("127.0.0.1:0".parse().unwrap(), &OnceLock::new())
        .err()
        .expect("second bind refuses");
    assert!(
        error
            .to_string()
            .contains("board-serve already running at http://127.0.0.1:"),
        "{error:#}"
    );
    assert_eq!(
        std::fs::read(&token_path).unwrap(),
        before,
        "a refused start must not rotate the live token"
    );
    std::fs::write(runtime.join("second-bind-refused"), "refused").unwrap();
    drop(first);
}

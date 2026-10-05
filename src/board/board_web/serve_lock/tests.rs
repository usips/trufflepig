use super::super::{web_endpoint, web_guard::BoardWebToken};
use super::*;
use std::net::SocketAddr;

#[test]
fn second_acquire_refuses_and_leaves_token_untouched() {
    let directory = crate::board::board_test_support::scratch("serve-lock-");
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let token_path = runtime.join("board-web.token");
    BoardWebToken::rotate_at(&token_path).unwrap();
    let before = std::fs::read(&token_path).unwrap();
    let address: SocketAddr = "127.0.0.1:7341".parse().unwrap();
    web_endpoint::publish(&runtime, address, &directory.path().join("web.sqlite3")).unwrap();
    let _first = acquire_at(&runtime).unwrap();
    let error = acquire_at(&runtime).err().expect("second acquire refuses");
    assert!(
        error
            .to_string()
            .contains("board-serve already running at http://127.0.0.1:7341"),
        "{error:#}"
    );
    assert_eq!(
        std::fs::read(&token_path).unwrap(),
        before,
        "a refused start must not rotate the live token"
    );
}

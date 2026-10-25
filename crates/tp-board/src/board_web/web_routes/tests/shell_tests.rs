use super::*;

#[test]
fn shell_carries_the_database_board_id() {
    let fixture = render_fixture();
    let reply = render_get(&fixture, "/");
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
    let stored = fixture
        .state
        .store
        .with_writer(
            &fixture.config,
            Instant::now() + Duration::from_secs(5),
            |writer| writer.board_uuid(),
        )
        .unwrap();
    assert_eq!(fixture.state.board_id, stored);
    assert!(
        reply.contains(&format!("<meta name=\"board-id\" content=\"{stored}\">")),
        "{reply}"
    );
}

#[test]
fn shell_and_static_assets_defeat_caching() {
    let fixture = render_fixture();
    for path in [
        "/",
        "/board_web_main.js",
        "/board_web.css",
        "/stream/board_stream.js",
    ] {
        let reply = render_get(&fixture, path);
        assert!(reply.starts_with("HTTP/1.1 200 "), "{path}: {reply}");
        assert!(
            reply.contains("Cache-Control: no-store\r\n"),
            "{path}: {reply}"
        );
    }
}

use super::*;

struct ByeFixture {
    directory: tempfile::TempDir,
    database: std::path::PathBuf,
    host: BoardHost,
}

impl ByeFixture {
    fn new() -> Self {
        let directory = tp_core::test_fixtures::scratch("board-bye-");
        let database = directory.path().join("board.sqlite3");
        let host = BoardHost::with_config(BoardConfig::for_database(&database));
        Self {
            directory,
            database,
            host,
        }
    }

    /// The reply's result line, without the lines-format footers.
    fn run(&self, session: &str, words: &[&str]) -> String {
        let root = self.directory.path().to_string_lossy().into_owned();
        let mut args = vec!["--format", "lines", "--root", &root];
        args.extend_from_slice(words);
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let reply = self
            .host
            .run(
                &crate::board_test_support::parse_board_arguments(&args).unwrap(),
                &RequestContext::new(Some(session.into()), Some("claude".into())),
                QueryDeadline::start(),
            )
            .unwrap();
        reply.lines().next().unwrap_or_default().to_owned()
    }

    /// Actor, entry, and event row counts.
    fn rows(&self) -> [i64; 3] {
        let conn = rusqlite::Connection::open(&self.database).unwrap();
        ["actors", "entries", "events"].map(|table| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        })
    }
}

#[test]
fn bye_without_a_session_writes_nothing_and_creates_no_storage() {
    let fixture = ByeFixture::new();
    assert_eq!(
        fixture.run("never-hello", &["board", "bye"]),
        "bye no session; nothing to end"
    );
    assert!(!fixture.database.exists(), "a no-op bye created storage");

    fixture
        .host
        .ensure_writer(QueryDeadline::after(Duration::from_secs(5)))
        .unwrap();
    let before = fixture.rows();
    assert_eq!(
        fixture.run("never-hello", &["board", "bye"]),
        "bye no session; nothing to end"
    );
    assert_eq!(fixture.rows(), before);

    // The writer path refuses to invent a session as well.
    let actor = BoardConfig::for_database(&fixture.database)
        .actor(Some("claude"), Some("never-hello"))
        .unwrap();
    let mut writer =
        crate::local_board::LocalBoard::open(&BoardConfig::for_database(&fixture.database))
            .unwrap();
    let reply = writer
        .handle(&BoardRequest::new(actor, BoardOp::Bye))
        .unwrap();
    assert!(matches!(
        reply.result,
        BoardResult::Bye(crate::board_protocol::ByeReply {
            outcome: crate::board_protocol::ByeOutcome::NoSession,
            entry: None,
            seq: None,
        })
    ));
    assert_eq!(fixture.rows(), before);
}

#[test]
fn repeated_bye_reports_the_original_bye_and_writes_nothing() {
    let fixture = ByeFixture::new();
    fixture.run("lead", &["board", "hello", "opus"]);
    let fresh = fixture.run("lead", &["board", "bye"]);
    let [_, entries, events] = fixture.rows();
    let original = fresh
        .strip_prefix("bye ended session ")
        .unwrap_or_else(|| panic!("{fresh}"));
    let (entry, cursor) = original.split_once(" cursor ").unwrap();
    assert_eq!(cursor, events.to_string(), "{fresh}");

    let repeated = fixture.run("lead", &["board", "bye"]);
    assert_eq!(
        repeated,
        format!("bye already ended; original bye {entry} cursor {cursor}; no new event")
    );
    assert_eq!(fixture.rows()[1..], [entries, events]);
}

use super::*;
use std::{collections::VecDeque, time::Duration};

#[derive(Default)]
struct FakeGateway {
    replies: VecDeque<Result<Option<String>>>,
    requests: Vec<Vec<String>>,
    ensured: usize,
}
impl BoardGateway for FakeGateway {
    fn request(&mut self, args: &[String], _: &RequestContext) -> Result<Option<String>> {
        self.requests.push(args.to_vec());
        self.replies.pop_front().unwrap_or(Ok(None))
    }
    fn ensure(&mut self) -> Result<()> {
        self.ensured += 1;
        Ok(())
    }
}

fn scratch() -> tempfile::TempDir {
    crate::board::board_test_support::scratch("board-client-")
}

fn invoke(
    words: &[&str],
    gateway: &mut FakeGateway,
    api: &AtomicU64,
    database: &Path,
    runtime: Option<&Path>,
) -> Result<String> {
    let args: Vec<_> = words.iter().map(|word| (*word).to_owned()).collect();
    let options = crate::cli::parse(&args)?;
    let args = super::super::prepare_client(&args, &options)?;
    let options = crate::cli::parse(&args)?;
    let command = board_grammar::parse(&options, None)?;
    run_prepared(
        &args,
        &options,
        &command,
        &RequestContext::new(None, None),
        gateway,
        api,
        &mut || Ok(BoardConfig::for_database(database)),
        runtime,
        &database.parent().unwrap().join("spool"),
    )
}

#[test]
fn no_daemon_board_access_never_contacts_or_starts_router() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway::default();
    invoke(
        &["--no-daemon", "board", "show"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        None,
    )
    .unwrap();
    assert!(gateway.requests.is_empty());
    assert_eq!(gateway.ensured, 0);
    assert!(database.exists());
}

#[test]
fn router_api_is_probed_once_before_dispatch_and_mismatch_never_falls_back() {
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
    let api = AtomicU64::new(0);
    assert_eq!(
        invoke(&["board", "show"], &mut gateway, &api, &database, None).unwrap(),
        "first"
    );
    assert_eq!(
        invoke(&["board", "show"], &mut gateway, &api, &database, None).unwrap(),
        "second"
    );
    assert_eq!(gateway.requests.len(), 3);
    assert_eq!(gateway.requests[0], ["system", "status"]);
    assert!(!database.exists());
    let status =
        serde_json::json!({"status":"ok","board_api":BOARD_API+1,"board_db":database}).to_string();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(Some(status))]),
        ..Default::default()
    };
    let error = invoke(
        &["board", "new", "Never written"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        None,
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("board_api_mismatch:"));
    assert_eq!(gateway.requests.len(), 1);
    assert_eq!(gateway.ensured, 0);
    assert!(!database.exists());
}

#[test]
fn unavailable_marker_skips_ensure_without_extending_its_deadline() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway::default();
    let api = AtomicU64::new(0);
    invoke(
        &["board", "show"],
        &mut gateway,
        &api,
        &database,
        Some(&runtime),
    )
    .unwrap();
    let before = std::fs::read(runtime.join("board-router-unavailable.json")).unwrap();
    invoke(
        &["board", "show"],
        &mut gateway,
        &api,
        &database,
        Some(&runtime),
    )
    .unwrap();
    assert_eq!(gateway.ensured, 1);
    assert_eq!(
        std::fs::read(runtime.join("board-router-unavailable.json")).unwrap(),
        before
    );
    let expired = SystemTime::now() - Duration::from_secs(31);
    crate::system::mark_board_router_unavailable(&runtime, expired).unwrap();
    invoke(
        &["board", "show"],
        &mut gateway,
        &api,
        &database,
        Some(&runtime),
    )
    .unwrap();
    assert_eq!(gateway.ensured, 2);
}

#[test]
fn lost_router_write_reply_never_replays_or_sets_absence_marker() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let database = directory.path().join("board.sqlite3");
    let status =
        serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":database}).to_string();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([
            Ok(Some(status)),
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into()),
        ]),
        ..Default::default()
    };
    invoke(
        &["board", "new", "Ambiguous write"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        Some(&runtime),
    )
    .unwrap_err();
    assert_eq!(gateway.requests.len(), 2);
    assert!(!database.exists());
    assert!(!runtime.join("board-router-unavailable.json").exists());
}

#[test]
fn every_local_client_path_refuses_a_different_router_database_pin() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let database = directory.path().join("client.sqlite3");
    crate::system::record_board_database(&runtime, &directory.path().join("router.sqlite3"))
        .unwrap();
    for words in [vec!["--no-daemon", "board", "show"], vec!["board", "show"]] {
        let error = invoke(
            &words,
            &mut FakeGateway::default(),
            &AtomicU64::new(0),
            &database,
            Some(&runtime),
        )
        .unwrap_err();
        assert!(error.to_string().contains("differs from router database"));
        assert!(!database.exists());
    }
}

#[test]
fn failed_capability_probe_queues_feedback_once_with_its_stable_key() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Err(
            std::io::Error::from(std::io::ErrorKind::BrokenPipe).into()
        )]),
        ..Default::default()
    };
    let reply = invoke(
        &["feedback", "blocked", "probe failed"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        None,
    )
    .unwrap();
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["result"]["result"], "queued");
    let key = reply["result"]["data"]["import_key"].as_str().unwrap();
    let spool = directory.path().join("spool");
    let files: Vec<_> = std::fs::read_dir(&spool).unwrap().collect();
    assert_eq!(files.len(), 1);
    let bytes = std::fs::read(spool.join(format!("{key}.feedback"))).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record["import_key"], key);
    assert_eq!(record["request"]["op"]["import_key"], key);
    assert_eq!(gateway.requests.len(), 1);
    assert_eq!(gateway.ensured, 0);
    assert!(!database.exists());
}

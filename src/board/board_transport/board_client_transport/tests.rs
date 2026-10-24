mod ambiguous_write_tests;
mod local_access_tests;
mod no_daemon_tests;
mod preflight_timeout_tests;
mod router_api_version_tests;
mod router_probe_cache_tests;
mod router_probe_tests;
mod schema_advice_tests;
mod shared_deadline_tests;

use super::*;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Default)]
struct FakeGateway {
    replies: VecDeque<Result<Option<String>>>,
    requests: Vec<Vec<String>>,
    request_remaining: Vec<Duration>,
    request_delay: Duration,
    deadline_expiry_windows: Vec<(Instant, Instant)>,
    ensured: usize,
    ensure_remaining: Vec<Duration>,
    ensure_delay: Duration,
    socket_denied: bool,
}
impl BoardGateway for FakeGateway {
    fn request(
        &mut self,
        args: &[String],
        _: &RequestContext,
        deadline: &QueryDeadline,
    ) -> Result<Option<String>> {
        self.requests.push(args.to_vec());
        let remaining = self.observe_deadline(deadline);
        self.request_remaining.push(remaining);
        if !self.request_delay.is_zero() {
            std::thread::sleep(self.request_delay);
        }
        self.replies.pop_front().unwrap_or(Ok(None))
    }
    fn ensure(&mut self, deadline: &QueryDeadline) -> Result<()> {
        self.ensured += 1;
        let remaining = self.observe_deadline(deadline);
        self.ensure_remaining.push(remaining);
        if !self.ensure_delay.is_zero() {
            std::thread::sleep(self.ensure_delay);
        }
        Ok(())
    }
    fn probe_socket(&mut self, _: &Path) -> std::io::Result<()> {
        if self.socket_denied {
            Err(std::io::ErrorKind::PermissionDenied.into())
        } else {
            Err(std::io::ErrorKind::ConnectionRefused.into())
        }
    }
}

impl FakeGateway {
    fn observe_deadline(&mut self, deadline: &QueryDeadline) -> Duration {
        let before = Instant::now();
        let remaining = deadline.remaining();
        let after = Instant::now();
        self.deadline_expiry_windows
            .push((before + remaining, after + remaining));
        remaining
    }
}

fn observations_share_expiry(windows: &[(Instant, Instant)]) -> bool {
    let Some((first_start, first_end)) = windows.first().copied() else {
        return false;
    };
    let latest_start = windows
        .iter()
        .map(|(start, _)| *start)
        .fold(first_start, |latest, current| latest.max(current));
    let earliest_end = windows
        .iter()
        .map(|(_, end)| *end)
        .fold(first_end, |earliest, current| earliest.min(current));
    latest_start <= earliest_end
}

fn scratch() -> tempfile::TempDir {
    crate::test_fixtures::scratch("board-client-")
}

fn invoke(
    words: &[&str],
    gateway: &mut FakeGateway,
    transport: &mut BoardClientTransport,
    database: &Path,
    runtime: Option<&Path>,
) -> Result<String> {
    invoke_with_deadline(
        words,
        gateway,
        transport,
        database,
        runtime,
        QueryDeadline::start(),
    )
}

fn invoke_with_deadline(
    words: &[&str],
    gateway: &mut FakeGateway,
    transport: &mut BoardClientTransport,
    database: &Path,
    runtime: Option<&Path>,
    deadline: QueryDeadline,
) -> Result<String> {
    invoke_with_load_delay(
        words,
        gateway,
        transport,
        database,
        runtime,
        deadline,
        Duration::ZERO,
    )
}

fn invoke_with_load_delay(
    words: &[&str],
    gateway: &mut FakeGateway,
    transport: &mut BoardClientTransport,
    database: &Path,
    runtime: Option<&Path>,
    deadline: QueryDeadline,
    load_delay: Duration,
) -> Result<String> {
    let args: Vec<_> = words.iter().map(|word| (*word).to_owned()).collect();
    invoke_args_with_load_delay(
        &args, gateway, transport, database, runtime, deadline, load_delay,
    )
}

fn invoke_args_with_load_delay(
    args: &[String],
    gateway: &mut FakeGateway,
    transport: &mut BoardClientTransport,
    database: &Path,
    runtime: Option<&Path>,
    deadline: QueryDeadline,
    load_delay: Duration,
) -> Result<String> {
    let options = crate::cli::parse(args)?;
    let args = crate::board::prepare_client(args, &options)?;
    let options = crate::cli::parse(&args)?;
    let command = board_grammar::parse(&options, None)?;
    transport.run_prepared(
        &args,
        &options,
        &command,
        &RequestContext::new(None, None),
        gateway,
        &mut || {
            if !load_delay.is_zero() {
                std::thread::sleep(load_delay);
            }
            Ok(BoardConfig::for_database(database))
        },
        runtime,
        &database.parent().unwrap().join("spool"),
        &deadline,
    )
}

fn feedback_args(directory: &Path, summary: &str, body: &str) -> Vec<String> {
    let body_path = directory.join("feedback-body.txt");
    std::fs::write(&body_path, body).unwrap();
    [
        "--body".to_owned(),
        body_path.display().to_string(),
        "feedback".to_owned(),
        "blocked".to_owned(),
        summary.to_owned(),
    ]
    .into()
}

fn assert_queued_feedback(spool: &Path, summary: &str, body: &str) -> String {
    let entries = std::fs::read_dir(spool)
        .unwrap()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one durable feedback record"
    );
    let path = entries[0].path();
    assert_eq!(
        path.extension().and_then(|value| value.to_str()),
        Some("feedback")
    );
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let key = record["import_key"].as_str().unwrap();
    assert_eq!(record["request"]["op"]["summary"], summary);
    assert_eq!(record["request"]["op"]["body"], body);
    assert_eq!(record["request"]["op"]["import_key"], key);
    key.to_owned()
}

fn assert_queued_reply(reply: &str) -> String {
    let reply: serde_json::Value = serde_json::from_str(reply).unwrap();
    assert_eq!(reply["result"]["result"], "queued");
    reply["result"]["data"]["import_key"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn failed_outbox_persistence_is_returned_without_a_second_attempt() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let spool = directory.path().join("spool");
    std::fs::write(&spool, "preserve this file").unwrap();
    let args: Vec<_> = ["feedback", "blocked", "unavailable outbox"]
        .map(str::to_owned)
        .into();
    let options = crate::cli::parse(&args).unwrap();
    let forwarded = crate::board::prepare_client(&args, &options).unwrap();
    let options = crate::cli::parse(&forwarded).unwrap();
    let command = board_grammar::parse(&options, None).unwrap();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(None)]),
        ensure_delay: Duration::from_millis(750),
        ..Default::default()
    };
    let mut loads = 0;
    let error = BoardClientTransport::default()
        .run_prepared(
            &forwarded,
            &options,
            &command,
            &RequestContext::new(None, None),
            &mut gateway,
            &mut || {
                loads += 1;
                Ok(BoardConfig::for_database(&database))
            },
            None,
            &spool,
            &QueryDeadline::after(Duration::from_millis(500)),
        )
        .unwrap_err();

    assert!(
        error.to_string().starts_with("board_unavailable:"),
        "{error}"
    );
    assert_eq!(loads, 1, "failed durability must not be retried");
    assert_eq!(std::fs::read(&spool).unwrap(), b"preserve this file");
    assert_eq!(gateway.requests.len(), 1);
    assert_eq!(gateway.ensured, 1);
}

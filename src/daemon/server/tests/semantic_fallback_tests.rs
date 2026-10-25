use super::*;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use tp_core::{
    daemon_transport::request_by, query_deadline::QueryDeadline, request_context::RequestContext,
};

const CLIENT_BUDGET: Duration = Duration::from_secs(12);
const TEST_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Eq, PartialEq)]
enum AdmissionEvent {
    SemanticEntered,
    LexicalEntered,
    LexicalObservedCancellation(bool),
}

#[derive(Clone)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }

    fn wait(&self) {
        let (released, changed) = &*self.0;
        let Ok(released) = released.lock() else {
            return;
        };
        let _ =
            changed.wait_timeout_while(released, Duration::from_secs(15), |released| !*released);
    }

    fn open(&self) {
        let (released, changed) = &*self.0;
        if let Ok(mut released) = released.lock() {
            *released = true;
            changed.notify_all();
        }
    }
}

struct ReleaseGates(Vec<Gate>);

impl ReleaseGates {
    fn open(&self) {
        for gate in &self.0 {
            gate.open();
        }
    }
}

impl Drop for ReleaseGates {
    fn drop(&mut self) {
        self.open();
    }
}

struct PendingRequest {
    label: &'static str,
    receiver: mpsc::Receiver<anyhow::Result<Option<String>>>,
    thread: JoinHandle<()>,
}

impl PendingRequest {
    fn wait(self) -> anyhow::Result<Option<String>> {
        let result = self
            .receiver
            .recv_timeout(TEST_WAIT)
            .unwrap_or_else(|error| panic!("{} did not finish: {error}", self.label));
        self.thread.join().expect("request thread panicked");
        result
    }
}

struct RunningRouter(Option<Served>);

impl Drop for RunningRouter {
    fn drop(&mut self) {
        if let Some(router) = self.0.take() {
            let _ = router.stop();
        }
    }
}

fn request_async(cache: PathBuf, args: Vec<String>, label: &'static str) -> PendingRequest {
    let (sender, receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let result = request_by(
            &cache,
            &args,
            &RequestContext::new(None, None),
            QueryDeadline::after(CLIENT_BUDGET),
        );
        let _ = sender.send(result);
    });
    PendingRequest {
        label,
        receiver,
        thread,
    }
}

fn workspace_args(config: &Path, root: &Path) -> Vec<String> {
    [
        "--workspace".to_owned(),
        config.display().to_string(),
        "--root".to_owned(),
        root.display().to_string(),
        "--format".to_owned(),
        "json".to_owned(),
        "--budget".to_owned(),
        "600".to_owned(),
        "search".to_owned(),
        "authentication".to_owned(),
        "kind:file".to_owned(),
    ]
    .into()
}

fn semantic_workspace(directory: &Path, root: &Path) -> PathBuf {
    let config = directory.join("semantic-fallback.toml");
    std::fs::write(
        &config,
        format!(
            "[workspace]\nname='semantic-fallback'\n[semantic]\nenabled=true\nrerank=true\n[members.home]\npath='{}'\n",
            root.display(),
        ),
    )
    .unwrap();
    config
}

fn lexical_search_page() -> String {
    serde_json::json!({
        "workspace": "semantic-fallback",
        "coverage": [{"member": "home", "indexed_files": 1, "total_files": 1}],
        "hits": [{"path": "src/auth.rs", "name": "authenticate"}],
        "next": null,
        "truncated": false,
        "tokenizer": "o200k_base",
    })
    .to_string()
}

#[test]
fn saturated_semantic_root_serves_annotated_lexical_search_without_extra_semantic_admission() {
    let semantic_gate = Gate::new();
    let lexical_gate = Gate::new();
    let (events_tx, events_rx) = mpsc::channel();
    let semantic_calls = Arc::new(AtomicUsize::new(0));
    let lexical_calls = Arc::new(AtomicUsize::new(0));
    let router = Served::start_tier(
        AdmissionTier::Router,
        false,
        WIDE,
        TestDaemon {
            reply: Box::new({
                let (events_tx, semantic_calls, lexical_calls) = (
                    events_tx.clone(),
                    Arc::clone(&semantic_calls),
                    Arc::clone(&lexical_calls),
                );
                let (semantic_gate, lexical_gate) = (semantic_gate.clone(), lexical_gate.clone());
                move |request| {
                    let class = crate::cli::classify_daemon_request(&request.args)?.class;
                    match class.lane {
                        RequestLane::Semantic => {
                            semantic_calls.fetch_add(1, Ordering::AcqRel);
                            let _ = events_tx.send(AdmissionEvent::SemanticEntered);
                            semantic_gate.wait();
                            Ok(lexical_search_page())
                        }
                        RequestLane::Query => {
                            assert!(request.args.iter().any(|arg| arg == "--no-sem"));
                            assert!(request.args.iter().any(|arg| arg == "--no-rerank"));
                            assert!(!request.args.iter().any(|arg| arg == "--sem"));
                            assert!(!request.args.iter().any(|arg| arg == "--rerank"));
                            assert_eq!(crate::cli::parse(&request.args)?.budget, 472);
                            let call = lexical_calls.fetch_add(1, Ordering::AcqRel);
                            let _ = events_tx.send(AdmissionEvent::LexicalEntered);
                            if call == 1 {
                                lexical_gate.wait();
                                let cancelled = request.deadline.cancelled();
                                let _ = events_tx
                                    .send(AdmissionEvent::LexicalObservedCancellation(cancelled));
                            }
                            Ok(lexical_search_page())
                        }
                        lane => panic!("unexpected lane for search: {lane:?}"),
                    }
                }
            }),
            reconcile: quiet(),
        },
    );
    let router_root = router.root.clone();
    let router_cache = router.cache.clone();
    let workspace_files = tempfile::tempdir().unwrap();
    let config = semantic_workspace(workspace_files.path(), &router_root);
    let mut router = RunningRouter(Some(router));
    let release = ReleaseGates(vec![semantic_gate, lexical_gate.clone()]);

    let mut held = Vec::with_capacity(2);
    for label in ["semantic request one", "semantic request two"] {
        held.push(request_async(
            router_cache.clone(),
            workspace_args(&config, &router_root),
            label,
        ));
    }
    for _ in 0..2 {
        assert_eq!(
            events_rx.recv_timeout(TEST_WAIT).unwrap(),
            AdmissionEvent::SemanticEntered
        );
    }
    assert_eq!(semantic_calls.load(Ordering::Acquire), 2);

    let lexical = request_async(
        router_cache.clone(),
        workspace_args(&config, &router_root),
        "semantic saturation lexical fallback",
    )
    .wait()
    .unwrap()
    .unwrap();
    assert_eq!(
        events_rx.recv_timeout(TEST_WAIT).unwrap(),
        AdmissionEvent::LexicalEntered
    );
    let response: serde_json::Value = serde_json::from_str(&lexical).unwrap();
    assert_eq!(response["hits"][0]["path"], "src/auth.rs");
    assert_eq!(response["coverage"][0]["member"], "home");
    assert_eq!(response["coverage"][0]["indexed_files"], 1);
    assert_eq!(response["semantic_admission"]["semantic_status"], "skipped");
    assert_eq!(
        response["semantic_admission"]["semantic_reason"],
        "root_limit"
    );
    assert_eq!(response["semantic_admission"]["rerank_status"], "skipped");
    assert_eq!(
        response["semantic_admission"]["rerank_reason"],
        "root_limit"
    );
    assert_eq!(response["semantic_admission"]["outstanding"], 2);
    assert_eq!(response["semantic_admission"]["limit"], 2);
    assert_eq!(semantic_calls.load(Ordering::Acquire), 2);
    assert_eq!(lexical_calls.load(Ordering::Acquire), 1);

    let mut client =
        UnixStream::connect(router_cache.join(tp_core::daemon_transport::SOCKET_NAME)).unwrap();
    tp_core::daemon_transport::daemon_wire_protocol::write_request(
        &mut client,
        &tp_core::daemon_transport::daemon_wire_protocol::DaemonRequest::Arguments {
            context: RequestContext::new(None, None),
            args: workspace_args(&config, &router_root),
        },
    )
    .unwrap();
    assert_eq!(
        events_rx.recv_timeout(TEST_WAIT).unwrap(),
        AdmissionEvent::LexicalEntered
    );
    client.shutdown(std::net::Shutdown::Both).unwrap();
    drop(client);
    lexical_gate.open();
    assert_eq!(
        events_rx.recv_timeout(TEST_WAIT).unwrap(),
        AdmissionEvent::LexicalObservedCancellation(true)
    );

    release.open();
    assert_eq!(lexical_calls.load(Ordering::Acquire), 2);
    for request in held {
        assert!(request.wait().unwrap().unwrap().contains("src/auth.rs"));
    }
    router.0.take().unwrap().stop().unwrap();
}

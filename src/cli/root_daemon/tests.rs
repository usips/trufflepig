use super::*;
use crate::{daemon::deadline::QueryDeadline, diagnostics::RequestContext};

fn ask(daemon: &RootDaemon, root: &Path, cache: &Path, words: &[&str]) -> Result<String> {
    let mut args: Vec<String> = [
        "--no-workspace",
        "--diagnostics",
        "off",
        "--root",
        root.to_str().unwrap(),
        "--cache",
        cache.to_str().unwrap(),
    ]
    .map(str::to_owned)
    .into();
    args.extend(words.iter().map(|word| (*word).to_owned()));
    daemon.request(AcceptedRequest {
        context: RequestContext::new(None, None),
        args,
        deadline: QueryDeadline::start(),
    })
}

#[test]
fn root_daemon_answers_index_warming_until_the_first_publication() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "pub fn warmed() {}\n").unwrap();
    let options = parse(&["--diagnostics".to_owned(), "off".to_owned()]).unwrap();
    let daemon = RootDaemon::open(root.path(), cache.path(), &options);
    let (root, cache) = (
        root.path().canonicalize().unwrap(),
        cache.path().canonicalize().unwrap(),
    );

    // Reads never index inside a request; the maintenance thread does.
    let error = ask(&daemon, &root, &cache, &["search", "sym:warmed"]).unwrap_err();
    assert!(crate::store::is_index_warming(&error), "{error:#}");
    let status: serde_json::Value =
        serde_json::from_str(&ask(&daemon, &root, &cache, &["status"]).unwrap()).unwrap();
    assert_eq!(
        (status["generation"].as_i64(), status["state"].as_str()),
        (Some(0), Some("warming"))
    );
    let read: serde_json::Value =
        serde_json::from_str(&ask(&daemon, &root, &cache, &["show", "lib.rs"]).unwrap()).unwrap();
    assert_eq!(read["lines"][0]["text"], "pub fn warmed() {}\n");

    daemon.reconcile().unwrap();
    let found: serde_json::Value =
        serde_json::from_str(&ask(&daemon, &root, &cache, &["search", "sym:warmed"]).unwrap())
            .unwrap();
    assert_eq!(found["hits"][0]["name"], "warmed", "{found}");
}

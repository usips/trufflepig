use super::*;
use crate::search;

#[test]
fn reconciliation_resumes_preparation_and_new_generations() -> anyhow::Result<()> {
    use crate::semantic::{DIMENSIONS, Embedding, preparation};
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    std::fs::write(root.path().join("source.rs"), "fn example() {}")?;
    let mut store = Store::open(root.path(), cache.path())?;
    store.index()?;
    let receipt = preparation::schedule(root.path(), cache.path())?;
    let database = rusqlite::Connection::open(cache.path().join("preparation.sqlite3"))?;
    database.execute("INSERT INTO preparation_runs(generation,state,cursor,total,cached,missing,failures,error,updated_ms) VALUES(?1,'running',0,1,0,1,0,NULL,0)", [receipt.captured_generation])?;
    let manager = preparation::PreparationManager::new(preparation::ClosureWorker(
        |inputs: &[preparation::EmbeddingInput]| {
            Ok(inputs
                .iter()
                .map(|_| {
                    let mut vector = [0.0; DIMENSIONS];
                    vector[0] = 1.0;
                    Ok(Embedding(vector))
                })
                .collect())
        },
    ));
    schedule_pending_preparation(&manager, root.path(), cache.path())?;
    let done = preparation::wait_timeout(
        root.path(),
        cache.path(),
        receipt.captured_generation,
        Duration::from_secs(3),
    )?;
    assert_eq!(done.state, preparation::PreparationState::Completed);
    std::fs::write(root.path().join("source.rs"), "fn changed() {}")?;
    store.index()?;
    schedule_pending_preparation(&manager, root.path(), cache.path())?;
    let done = preparation::wait_timeout(
        root.path(),
        cache.path(),
        store.generation()?,
        Duration::from_secs(3),
    )?;
    assert_eq!(done.state, preparation::PreparationState::Completed);
    Ok(())
}

#[test]
fn daemon_dispatch_rejects_wrong_root_and_unbounded_options() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut options = parse(&[
        "--root".into(),
        other.path().display().to_string(),
        "status".into(),
    ])
    .unwrap();
    assert!(
        local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string()
            .contains("invalid_root")
    );
    options.root = root.path().to_owned();
    options.limit = 0;
    assert!(
        local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string()
            .contains("invalid_limit")
    );
    options.limit = 1;
    options.budget = usize::MAX;
    assert!(
        local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string()
            .contains("invalid_budget")
    );
}

#[test]
fn unknown_commands_are_rejected_before_root_validation() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    for command in ["statuss", "not-a-command"] {
        let options = parse(&[
            "--root".into(),
            other.path().display().to_string(),
            command.into(),
        ])
        .unwrap();
        let error = local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown_command"), "{error}");
        assert!(error.contains("search"), "{error}");
        assert!(!error.contains("invalid_root"), "{error}");
    }
}

#[test]
fn explicit_search_accepts_command_words() {
    let options = parse(&["search".into(), "status".into(), "not-a-command".into()]).unwrap();
    validate(&options).unwrap();
    assert_eq!(
        options.words,
        vec![
            "search".to_owned(),
            "status".to_owned(),
            "not-a-command".to_owned(),
        ]
    );
}

#[test]
fn client_normalization_preserves_regex_spaces() {
    let options = parse(&[
        "search".into(),
        "re:a  b".into(),
        "--budget".into(),
        "500".into(),
    ])
    .unwrap();
    let root = Path::new("/example");
    let forwarded = parse(&normalized_args(&options, root)).unwrap();
    assert_eq!(forwarded.root, root);
    assert_eq!(forwarded.budget, 500);
    assert_eq!(
        search::Query::parse(&forwarded.words[1..].join(" "))
            .unwrap()
            .text,
        "a  b"
    );
}

#[test]
fn client_normalization_forwards_semantic_overrides() {
    let options = parse(&["--sem".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(forwarded.sem);
    assert!(!forwarded.no_sem);

    let options = parse(&["--no-sem".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(!forwarded.sem);
    assert!(forwarded.no_sem);
}

#[test]
fn semantic_flags_are_mutually_exclusive() {
    let error = parse(&["--sem".into(), "--no-sem".into(), "search".into()]).unwrap_err();
    assert!(error.to_string().contains("cannot be used with"));
}

#[test]
fn client_normalization_forwards_rerank_overrides() {
    let options = parse(&["--rerank".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(forwarded.rerank);
    assert!(!forwarded.no_rerank);

    let options = parse(&["--no-rerank".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(!forwarded.rerank);
    assert!(forwarded.no_rerank);
}

#[test]
fn rerank_flags_are_mutually_exclusive() {
    let error = parse(&["--rerank".into(), "--no-rerank".into(), "search".into()]).unwrap_err();
    assert!(error.to_string().contains("cannot be used with"));
}

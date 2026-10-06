use super::super::*;

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
    root_daemon::schedule_pending_preparation(&manager, root.path(), cache.path())?;
    let done = preparation::wait_timeout(
        root.path(),
        cache.path(),
        receipt.captured_generation,
        Duration::from_secs(3),
    )?;
    assert_eq!(done.state, preparation::PreparationState::Completed);
    std::fs::write(root.path().join("source.rs"), "fn changed() {}")?;
    store.index()?;
    root_daemon::schedule_pending_preparation(&manager, root.path(), cache.path())?;
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
fn system_routes_gates_internal_and_local_verbs() {
    let routed = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let options = parse(&args).unwrap();
        let verb = options
            .words
            .first()
            .map(String::as_str)
            .unwrap_or("status");
        system_routes(&options, verb)
    };
    for args in [
        &["serve"][..],
        &["workspace-serve"][..],
        &["system-serve"][..],
        &["system"][..],
        &["ws"][..],
        &["stop"][..],
        &["index"][..],
        &["init"][..],
        &["semantic-check", "model"][..],
        &["semantic", "status"][..],
        &["--no-daemon", "search", "query"][..],
    ] {
        assert!(!routed(args), "expected no system route for {args:?}");
    }
    for args in [
        &["search", "query"][..],
        &["show", "src/lib.rs"][..],
        &["ctx", "HANDLE"][..],
        &["refs", "name"][..],
        &["map"][..],
        &["hist", "src/lib.rs"][..],
        &["blame", "src/lib.rs"][..],
        &["diff", "src/lib.rs"][..],
        &["audit"][..],
        &["doctor"][..],
        &["semantic", "prepare"][..],
        &["hist-index"][..],
    ] {
        assert!(routed(args), "expected a system route for {args:?}");
    }
}

#[test]
fn system_dir_prints_the_resolved_runtime_path() {
    let args = ["system".to_owned(), "dir".to_owned()];
    let options = parse(&args).unwrap();
    let context = crate::diagnostics::RequestContext::new(None, None);
    let output = system_command(&options, &context).unwrap();
    assert_eq!(
        output,
        format!("{}\n", crate::system::dir().unwrap().display())
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

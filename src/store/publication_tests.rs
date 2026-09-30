use super::*;
use crate::daemon::deadline::QueryDeadline;

fn fixture() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let store = Store::open(&root, &directory.path().join("cache")).unwrap();
    (directory, store)
}

#[test]
fn publication_interval_survives_restart_and_failed_publication() {
    let (_directory, mut store) = fixture();
    assert!(store.publication().unwrap().is_none());
    std::fs::write(store.root.join("file.txt"), "before").unwrap();
    store.index().unwrap();
    let first = store.publication().unwrap().unwrap();
    assert_eq!(first.generation, store.generation().unwrap());
    assert!(first.capture_started_ms <= first.capture_completed_ms);
    assert_eq!(decode_path(&first.root).unwrap(), store.root);
    let reopened = Store::open(&store.root, &store.cache).unwrap();
    assert_eq!(reopened.publication().unwrap().unwrap(), first);
    store.index().unwrap();
    assert_eq!(store.publication().unwrap().unwrap(), first);
    store.conn.execute_batch("CREATE TRIGGER fail_publish BEFORE INSERT ON files BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    std::fs::write(store.root.join("file.txt"), "after").unwrap();
    assert!(store.index().is_err());
    assert_eq!(store.publication().unwrap().unwrap(), first);
    assert!(
        store
            .observations_since(first.generation)
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn publication_reads_hold_one_generation_across_another_writer() {
    let (_directory, mut store) = fixture();
    std::fs::write(store.root.join("file.txt"), "before").unwrap();
    store.index().unwrap();
    let mut writer = Store::open(&store.root, &store.cache).unwrap();
    let first = store.publication().unwrap().unwrap();
    store
        .with_publication(|conn, publication| {
            std::fs::write(writer.root.join("file.txt"), "after").unwrap();
            writer.index()?;
            let bytes: Vec<u8> = conn.query_row(
                "SELECT contents.bytes FROM files JOIN contents USING(revision)",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(bytes, b"before");
            assert_eq!(publication, &first);
            Ok(())
        })
        .unwrap();
    let snapshot = store.published_files().unwrap();
    assert_eq!(snapshot.publication.generation, first.generation + 1);
    assert_eq!(
        snapshot.files[0].revision,
        Some(ContentRevision::of(b"after"))
    );
}

#[test]
fn publication_epoch_changes_only_when_index_database_is_recreated() {
    let (_directory, mut store) = fixture();
    std::fs::write(store.root.join("file.txt"), "before").unwrap();
    store.index().unwrap();
    let first = store.publication().unwrap().unwrap();
    let root = store.root.clone();
    let cache = store.cache.clone();
    drop(store);
    let mut store = Store::open(&root, &cache).unwrap();
    std::fs::write(root.join("file.txt"), "after").unwrap();
    store.index().unwrap();
    assert_eq!(
        store.publication().unwrap().unwrap().index_epoch,
        first.index_epoch
    );
    drop(store);
    std::fs::remove_file(cache.join("index.sqlite3")).unwrap();
    let mut store = Store::open(&root, &cache).unwrap();
    store.index().unwrap();
    let recreated = store.publication().unwrap().unwrap();
    assert_eq!(recreated.generation, first.generation);
    assert_ne!(recreated.index_epoch, first.index_epoch);
}

#[test]
fn publication_observations_distinguish_ignore_delete_and_net_revert() {
    let (_directory, mut store) = fixture();
    for path in ["ignored.txt", "deleted.txt", "reverted.txt"] {
        std::fs::write(store.root.join(path), "before").unwrap();
    }
    store.index().unwrap();
    let baseline = store.generation().unwrap();
    std::fs::write(store.root.join(".ignore"), "ignored.txt\n").unwrap();
    std::fs::remove_file(store.root.join("deleted.txt")).unwrap();
    std::fs::write(store.root.join("reverted.txt"), "intermediate").unwrap();
    store.index().unwrap();
    std::fs::write(store.root.join("reverted.txt"), "before").unwrap();
    store.index().unwrap();
    let observations = store.observations_since(baseline).unwrap();
    assert!(observations.complete);
    let ignored = observations
        .changes
        .iter()
        .find(|change| change.path == "ignored.txt")
        .unwrap();
    assert_eq!(ignored.kind, "ignored_or_excluded");
    let deleted = observations
        .changes
        .iter()
        .find(|change| change.path == "deleted.txt")
        .unwrap();
    assert_eq!(deleted.kind, "deleted");
    let reverted: Vec<_> = observations
        .changes
        .iter()
        .filter(|change| change.path == "reverted.txt")
        .collect();
    assert_eq!(reverted.len(), 2);
    assert_eq!(reverted[0].before_revision, reverted[1].after_revision);
    store
        .conn
        .execute(
            "DELETE FROM publication_windows WHERE generation=?1",
            [baseline + 1],
        )
        .unwrap();
    assert!(!store.observations_since(baseline).unwrap().complete);
}

fn published_fixture() -> (tempfile::TempDir, Store) {
    let (directory, mut store) = fixture();
    std::fs::write(store.root.join("lib.rs"), "fn alpha() {}\n").unwrap();
    store.index().unwrap();
    (directory, store)
}

#[test]
fn open_read_rejects_writes() {
    let (_directory, store) = published_fixture();
    let mut reader = Store::open_read(&store.root, &store.cache, QueryDeadline::start()).unwrap();
    assert!(
        reader
            .index()
            .unwrap_err()
            .to_string()
            .contains("read_only_store")
    );
    assert!(
        reader
            .conn
            .execute("INSERT INTO meta VALUES('extra','x')", [])
            .is_err()
    );
    assert!(reader.conn.execute_batch("CREATE TABLE extra(x)").is_err());
    let id =
        crate::results::save_entries(&reader, 1, serde_json::json!({}), vec![], false).unwrap();
    assert!(crate::results::load_entries(&store, &id).is_ok());
    assert!(store.cache.join(crate::results::RESULTS_DATABASE).is_file());
}

#[test]
fn open_read_reports_unpublished_or_foreign_index() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let cache = directory.path().join("cache");
    let missing = Store::open_read(&root, &cache, QueryDeadline::start())
        .err()
        .unwrap();
    assert!(is_index_warming(&missing), "{missing:#}");
    let mut writer = Store::open(&root, &cache).unwrap();
    let unpublished = Store::open_read(&root, &cache, QueryDeadline::start())
        .err()
        .unwrap();
    assert!(is_index_warming(&unpublished), "{unpublished:#}");
    std::fs::write(root.join("lib.rs"), "fn alpha() {}\n").unwrap();
    writer.index().unwrap();
    let foreign = Store::open_read(directory.path(), &cache, QueryDeadline::start())
        .err()
        .unwrap();
    assert!(foreign.to_string().contains("another repository root"));
}

#[test]
fn open_read_interrupts_statements_after_its_deadline() {
    let (_directory, store) = published_fixture();
    let deadline = QueryDeadline::after(std::time::Duration::from_millis(200));
    let reader = Store::open_read(&store.root, &store.cache, deadline).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    let error = reader
        .conn
        .query_row(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c LIMIT 10000000) SELECT count(*) FROM c",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(anyhow::Error::from)
        .unwrap_err();
    assert!(crate::daemon::deadline::is_timed_out(
        &deadline.classify(error)
    ));
}

#[test]
fn parent_view_reads_worktree_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent");
    let worktree = directory.path().join("worktree");
    let (parent_cache, worktree_cache) = (directory.path().join("pc"), directory.path().join("wc"));
    for root in [&parent, &worktree] {
        std::fs::create_dir(root).unwrap();
    }
    std::fs::write(parent.join("lib.rs"), "fn alpha() {}\n").unwrap();
    std::fs::write(
        worktree.join("lib.rs"),
        "// edited in worktree\nfn alpha() {}\n",
    )
    .unwrap();
    Store::open(&parent, &parent_cache)
        .unwrap()
        .index()
        .unwrap();
    let view = Store::open_read(&parent, &parent_cache, QueryDeadline::start())
        .unwrap()
        .reading_from(&worktree, &worktree_cache)
        .unwrap();
    assert_eq!(view.index_root(), parent.canonicalize().unwrap());
    assert_eq!(view.root, worktree.canonicalize().unwrap());
    let query = crate::search::Query::parse("re:edited in worktree").unwrap();
    let live = crate::search::search(&view, &query, false, &parent_cache).unwrap();
    assert_eq!(live.hits.len(), 1);
    assert_eq!(live.hits[0].path, "lib.rs");
    let budget = crate::output::OutputBudget::new(600).unwrap();
    let read = crate::source::show(&view, "path:lib.rs:1-1", &budget).unwrap();
    assert!(read.contains("edited in worktree"), "{read}");
    let indexed = crate::search::definitions(
        &view,
        &crate::search::Query::parse("sym:alpha").unwrap(),
        &crate::search::InvocationDirectory::root(),
    )
    .unwrap();
    let id = crate::results::save(&view, indexed).unwrap();
    let stale = crate::source::show(&view, &format!("{id}:1"), &budget).unwrap_err();
    assert!(stale.to_string().contains("stale_source"), "{stale:#}");
    assert!(
        worktree_cache
            .join(crate::results::RESULTS_DATABASE)
            .is_file()
    );
    assert!(!parent_cache.join(crate::results::RESULTS_DATABASE).exists());
}

#[test]
fn publish_while_query_never_locks() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_directory, mut writer) = published_fixture();
    let (root, cache) = (writer.root.clone(), writer.cache.clone());
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let publishing = std::thread::spawn({
        let stop = stop.clone();
        move || {
            let mut publications = 0;
            while !stop.load(Ordering::Relaxed) {
                publications += 1;
                let body = format!("fn alpha() {{}}\nfn beta_{publications}() {{}}\n");
                std::fs::write(writer.root.join("lib.rs"), body).unwrap();
                writer.index().unwrap();
            }
            publications
        }
    });
    let query = crate::search::Query::parse("sym:alpha").unwrap();
    for _ in 0..60 {
        let reader = Store::open_read(&root, &cache, QueryDeadline::start()).unwrap();
        let found = crate::search::search(&reader, &query, false, &cache).unwrap();
        assert!(!found.hits.is_empty());
        crate::results::save(&reader, found).unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    assert!(publishing.join().unwrap() > 1);
}

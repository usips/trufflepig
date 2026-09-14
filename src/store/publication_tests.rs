use super::*;

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

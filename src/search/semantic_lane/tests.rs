#[cfg(feature = "semantic")]
use crate::{
    search::{self, Query},
    semantic::{
        DIMENSIONS, Embedding, SemanticSession,
        embedding_cache::{EmbeddingCache, content_key},
    },
    store::Store,
};

#[cfg(feature = "semantic")]
fn fixture(files: &[(&str, &[u8])]) -> (tempfile::TempDir, tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let destination = root.path().join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(destination, source).unwrap();
    }
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    (root, cache, store)
}

#[cfg(feature = "semantic")]
fn unit_vector(axis: usize) -> Embedding {
    let mut values = [0.0; DIMENSIONS];
    values[axis] = 1.0;
    Embedding(values)
}

#[cfg(feature = "semantic")]
fn cache_vector(directory: &std::path::Path, body: &str, vector: &Embedding) {
    let mut cache = EmbeddingCache::open(directory).unwrap();
    cache.put(&content_key(body), vector).unwrap();
}

#[cfg(feature = "semantic")]
fn prepared_search(
    store: &Store,
    cache: &std::path::Path,
    text: &str,
    vector: Embedding,
) -> crate::results::ResultSet {
    search::search_prepared(
        store,
        &Query::parse(text).unwrap(),
        cache,
        Some(vector),
        &mut crate::search::telemetry::RetrievalTrace::disabled(),
    )
    .unwrap()
}

#[cfg(feature = "semantic")]
#[test]
fn semantic_cache_follows_current_content_across_edits_and_moves() {
    let (root, cache, mut store) = fixture(&[("old.rs", b"fn old_body() {}")]);
    let old_body = "fn old_body() {}";
    let vector = unit_vector(0);
    cache_vector(cache.path(), old_body, &vector);

    let initial = prepared_search(&store, cache.path(), "semantic intent", vector.clone());
    assert!(
        initial
            .hits
            .iter()
            .any(|hit| { hit.path == "old.rs" && hit.provenance.as_deref() == Some("semantic") })
    );
    let old_region_id: i64 = store
        .conn
        .query_row(
            "SELECT r.id FROM regions r JOIN files f ON f.id=r.file_id WHERE f.path='old.rs'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    std::fs::write(root.path().join("old.rs"), b"fn new_body() {}").unwrap();
    store.index().unwrap();
    let new_region_id: i64 = store
        .conn
        .query_row(
            "SELECT r.id FROM regions r JOIN files f ON f.id=r.file_id WHERE f.path='old.rs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(new_region_id, old_region_id);

    let edited = prepared_search(&store, cache.path(), "semantic intent", vector);
    assert!(
        edited
            .hits
            .iter()
            .all(|hit| hit.provenance.as_deref() != Some("semantic"))
    );
    assert_eq!(edited.coverage["semantic_regions"], 0);
    assert_eq!(edited.coverage["semantic_pending"], 1);
    assert_eq!(edited.coverage["semantic_total_regions"], 1);

    std::fs::remove_file(root.path().join("old.rs")).unwrap();
    std::fs::write(root.path().join("moved.rs"), b"fn moved_body() {}").unwrap();
    store.index().unwrap();
    let moved_region_id: i64 = store
        .conn
        .query_row(
            "SELECT r.id FROM regions r JOIN files f ON f.id=r.file_id WHERE f.path='moved.rs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(moved_region_id, old_region_id);

    let moved = prepared_search(&store, cache.path(), "semantic intent", unit_vector(0));
    assert!(
        moved
            .hits
            .iter()
            .all(|hit| hit.provenance.as_deref() != Some("semantic"))
    );
    assert_eq!(moved.coverage["semantic_regions"], 0);
    assert_eq!(moved.coverage["semantic_pending"], 1);
    assert_eq!(moved.coverage["semantic_total_regions"], 1);
}

#[cfg(feature = "semantic")]
#[test]
fn semantic_coverage_counts_cached_and_pending_regions_exactly() {
    let (_root, cache, store) = fixture(&[
        ("a.rs", b"fn cached_a() {}"),
        ("b.rs", b"fn pending_b() {}"),
        ("c.rs", b"fn cached_c() {}"),
    ]);
    let vector = unit_vector(0);
    cache_vector(cache.path(), "fn cached_a() {}", &vector);
    cache_vector(cache.path(), "fn cached_c() {}", &vector);

    let result = prepared_search(&store, cache.path(), "semantic intent", vector);
    let semantic_paths = result
        .hits
        .iter()
        .filter(|hit| hit.provenance.as_deref() == Some("semantic"))
        .map(|hit| hit.path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(semantic_paths, ["a.rs", "c.rs"].into_iter().collect());
    assert_eq!(result.coverage["semantic_files"], 2);
    assert_eq!(result.coverage["semantic_total_regions"], 3);
    assert_eq!(result.coverage["semantic_regions"], 2);
    assert_eq!(result.coverage["semantic_pending"], 1);
    assert_eq!(result.coverage["semantic_status"], "partial");
}

#[cfg(feature = "semantic")]
#[test]
fn semantic_cache_failure_preserves_lexical_hits() {
    let (_root, cache, store) = fixture(&[("source.rs", b"lexical fallback needle")]);
    let result = prepared_search(&store, cache.path(), "fallback needle", unit_vector(0));
    assert!(
        result
            .hits
            .iter()
            .any(|hit| { hit.path == "source.rs" && hit.provenance.as_deref() == Some("lexical") })
    );
    assert_eq!(result.coverage["semantic_status"], "partial");
    assert_eq!(result.coverage["semantic_failures"], 1);
    assert!(result.coverage["semantic_reason"].is_string());
}

#[cfg(feature = "semantic")]
#[test]
fn semantic_region_limit_is_per_file() -> anyhow::Result<()> {
    const MANY_REGIONS: usize = 10_001;
    let source = vec![b'x'; MANY_REGIONS];
    let (_root, cache, mut store) = fixture(&[("many.txt", &source)]);
    let vector = unit_vector(0);
    cache_vector(cache.path(), std::str::from_utf8(&source)?, &vector);
    cache_vector(cache.path(), "x", &vector);
    cache_vector(cache.path(), "y", &vector);
    let original_bodies = store
        .conn
        .prepare("SELECT body FROM regions")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for body in &original_bodies {
        cache_vector(cache.path(), body, &vector);
    }

    let many_file_id: i64 =
        store
            .conn
            .query_row("SELECT id FROM files WHERE path='many.txt'", [], |row| {
                row.get(0)
            })?;
    let transaction = store.conn.transaction()?;
    for start in 0..MANY_REGIONS {
        transaction.execute(
            "INSERT INTO regions(file_id,start,end,name,kind,body) VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                many_file_id,
                start as i64,
                (start + 1) as i64,
                "synthetic",
                "synthetic",
                "x"
            ],
        )?;
    }
    let other_revision = blake3::hash(b"y").to_hex().to_string();
    transaction.execute(
        "INSERT INTO contents(revision,bytes) VALUES(?1,?2)",
        rusqlite::params![other_revision, b"y".as_slice()],
    )?;
    transaction.execute(
        "INSERT INTO files(path,revision,language,status,bytes) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params!["other.txt", other_revision, "text", "read", 1_i64],
    )?;
    let other_file_id = transaction.last_insert_rowid();
    transaction.execute(
        "INSERT INTO regions(file_id,start,end,name,kind,body) VALUES(?1,0,1,?2,?3,?4)",
        rusqlite::params![other_file_id, "other.txt", "file", "y"],
    )?;
    transaction.commit()?;

    let result = prepared_search(&store, cache.path(), "semantic intent", vector);
    let semantic_paths = result
        .hits
        .iter()
        .filter(|hit| hit.provenance.as_deref() == Some("semantic"))
        .map(|hit| hit.path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        semantic_paths,
        ["many.txt", "other.txt"].into_iter().collect()
    );
    assert_eq!(result.coverage["semantic_pending"], 0);
    assert_eq!(
        result.coverage["semantic_total_regions"],
        MANY_REGIONS + original_bodies.len() + 1
    );
    assert!(!result.truncated);
    Ok(())
}

#[cfg(feature = "semantic")]
#[test]
fn no_daemon_search_uses_cached_query_and_source_vectors_without_model() -> anyhow::Result<()> {
    let (_root, cache, store) = fixture(&[("source.rs", b"fn cached_source() {}")]);
    let query = "semantic intent";
    let vector = unit_vector(0);
    cache_vector(cache.path(), query, &vector);
    cache_vector(cache.path(), "fn cached_source() {}", &vector);

    let initializations = crate::semantic::model_initializations();
    let mut session = SemanticSession::new();
    session.set_no_daemon(true);
    let result = search::search_with_session(
        &store,
        &Query::parse(query)?,
        true,
        cache.path(),
        &mut session,
        &mut crate::search::telemetry::RetrievalTrace::disabled(),
    )?;
    assert!(
        result.hits.iter().any(|hit| {
            hit.path == "source.rs" && hit.provenance.as_deref() == Some("semantic")
        })
    );
    assert_eq!(result.coverage["semantic_status"], "ready");
    assert_eq!(result.coverage["semantic_pending"], 0);
    assert!(!session.is_loaded());
    assert_eq!(session.initializations(), 0);
    assert_eq!(crate::semantic::model_initializations(), initializations);
    Ok(())
}

#[cfg(not(feature = "semantic"))]
#[test]
fn semantic_disabled_falls_back_to_lexical_search() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    std::fs::write(
        root.path().join("source.rs"),
        b"// lexical fallback needle\n",
    )?;
    let mut store = crate::store::Store::open(root.path(), cache.path())?;
    store.index()?;

    let result = crate::search::search(
        &store,
        &crate::search::Query::parse("fallback needle")?,
        true,
        cache.path(),
    )?;
    assert!(
        result
            .hits
            .iter()
            .any(|hit| { hit.path == "source.rs" && hit.provenance.as_deref() == Some("lexical") })
    );
    assert_eq!(result.coverage["semantic_status"], "unavailable");
    Ok(())
}

use super::*;

fn fixture() -> Result<(tempfile::TempDir, Store)> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root");
    let cache = directory.path().join("cache");
    std::fs::create_dir(&root)?;
    std::fs::write(root.join("example.rs"), "fn example() {}\n")?;
    let mut store = Store::open(&root, &cache)?;
    store.index()?;
    Ok((directory, store))
}

#[test]
fn probes_observe_source_drift_without_corruption() -> Result<()> {
    let (directory, store) = fixture()?;
    let cache = directory.path().join("cache");
    std::fs::write(store.root.join("example.rs"), "fn changed() {}\n")?;
    let session = SemanticSession::new();
    let report = doctor(&store, &cache, &session)?;
    assert!(
        report
            .probes
            .iter()
            .all(|check| !matches!(check.outcome, ProbeOutcome::Failed))
    );
    let extraction = report
        .probes
        .iter()
        .find(|check| check.name == ProbeName::UnchangedExtractionIdentity)
        .unwrap();
    assert_eq!(extraction.drifted, 1);
    assert!(!session.is_loaded());
    assert_eq!(session.initializations(), 0);
    assert!(!cache.join("inference.lock").exists());
    assert!(!cache.join("embeddings.sqlite").exists());
    Ok(())
}

#[test]
fn probes_detect_corrupt_cached_extraction_on_unchanged_source() -> Result<()> {
    let (directory, store) = fixture()?;
    store
        .conn
        .execute("UPDATE extraction_cache SET facts='{}'", [])?;
    let report = doctor(
        &store,
        &directory.path().join("cache"),
        &SemanticSession::new(),
    )?;
    let extraction = report
        .probes
        .iter()
        .find(|check| check.name == ProbeName::UnchangedExtractionIdentity)
        .unwrap();
    assert!(matches!(extraction.outcome, ProbeOutcome::Failed));
    Ok(())
}

#[test]
fn probes_detect_published_definition_drift_on_unchanged_source() -> Result<()> {
    let (directory, store) = fixture()?;
    store.conn.execute(
        "UPDATE definitions SET name='incorrect' WHERE kind='function'",
        [],
    )?;
    let report = doctor(
        &store,
        &directory.path().join("cache"),
        &SemanticSession::new(),
    )?;
    let extraction = report
        .probes
        .iter()
        .find(|check| check.name == ProbeName::UnchangedExtractionIdentity)
        .unwrap();
    assert!(matches!(extraction.outcome, ProbeOutcome::Failed));
    Ok(())
}

#[test]
fn probes_healthy_index_passes_without_mutation() -> Result<()> {
    let (directory, store) = fixture()?;
    let before = store.generation()?;
    let report = doctor(
        &store,
        &directory.path().join("cache"),
        &SemanticSession::new(),
    )?;
    for probe in &report.probes {
        if probe.name != ProbeName::SemanticProvenance {
            assert!(matches!(probe.outcome, ProbeOutcome::Passed), "{probe:?}");
        }
    }
    assert_eq!(store.generation()?, before);
    Ok(())
}

#[test]
fn probes_legacy_semantic_cache_is_unverified_not_corruption() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let conn = Connection::open(directory.path().join("embeddings.sqlite"))?;
    conn.execute_batch(
        "CREATE TABLE embeddings(key TEXT PRIMARY KEY,vector BLOB,touched INTEGER)",
    )?;
    let report = semantic_probe::check(directory.path());
    assert!(matches!(report.outcome, ProbeOutcome::Incomplete));
    assert_eq!(report.unverified, 1);
    Ok(())
}

#[test]
fn probes_invalid_semantic_vector_fails_even_with_old_provenance() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let conn = Connection::open(directory.path().join("embeddings.sqlite"))?;
    conn.execute_batch("CREATE TABLE embeddings(key TEXT PRIMARY KEY,vector BLOB,touched INTEGER);
        CREATE TABLE embedding_provenance(key TEXT PRIMARY KEY,model_revision TEXT,input_version TEXT);
        INSERT INTO embeddings VALUES('key',x'00',1);
        INSERT INTO embedding_provenance VALUES('key','old','old')")?;
    let report = semantic_probe::check(directory.path());
    assert!(matches!(report.outcome, ProbeOutcome::Failed));
    Ok(())
}

#[test]
fn probes_foreign_key_failure_is_reported() -> Result<()> {
    let (directory, store) = fixture()?;
    store.conn.execute_batch(
        "PRAGMA foreign_keys=OFF;
        INSERT INTO definitions(file_id,name,kind,start,end) VALUES(99999,'broken','function',0,1)",
    )?;
    let report = sql_probe(
        &directory.path().join("cache"),
        ProbeName::ForeignKeys,
        "PRAGMA foreign_key_check",
    );
    assert!(matches!(report.outcome, ProbeOutcome::Failed));
    Ok(())
}

#[test]
fn probes_missing_fts_documents_are_reported() -> Result<()> {
    let (directory, store) = fixture()?;
    store.conn.execute("DELETE FROM documents", [])?;
    let report = fts_probe(&directory.path().join("cache"));
    assert!(matches!(report.outcome, ProbeOutcome::Failed));
    Ok(())
}

#[test]
fn probes_sql_deadline_reports_incomplete() -> Result<()> {
    let (directory, _) = fixture()?;
    let report = sql_probe(
        &directory.path().join("cache"),
        ProbeName::ForeignKeys,
        "WITH RECURSIVE work(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM work WHERE n<1000000000) SELECT sum(n) FROM work",
    );
    assert!(matches!(report.outcome, ProbeOutcome::Incomplete));
    Ok(())
}

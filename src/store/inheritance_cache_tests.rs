use super::{Store, php_resolver, scan, schema};
use crate::extract::{self, Extraction, Occurrence, php_markers::MARKER_KIND};
use rusqlite::{Connection, params};

const CHILD_PATH: &str = "src/addons/Owner/Addon/Impl.php";
const CHILD_SOURCE: &str = "<?php\nnamespace Owner\\Addon;\nclass Impl extends \\XFCP_Base { public function run() { return parent::run(); } }\n";
const BASE_PATH: &str = "src/addons/Base/Addon/Base.php";
const BASE_SOURCE: &str =
    "<?php\nnamespace Base\\Addon;\nclass Base { public function run() {} }\n";
const EXTENSIONS_PATH: &str = "src/addons/Owner/Addon/_data/class_extensions.xml";
const EXTENSIONS_XML: &str = "<class_extensions>\n<extension from_class=\"Base\\Addon\\Base\" to_class=\"Owner\\Addon\\Impl\" active=\"1\" execute_order=\"1\"/>\n</class_extensions>\n";

struct InheritanceCacheFixture {
    root: tempfile::TempDir,
    _cache: tempfile::TempDir,
    store: Store,
}

fn inheritance_cache_fixture() -> InheritanceCacheFixture {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for (path, source) in [
        (CHILD_PATH, CHILD_SOURCE),
        (BASE_PATH, BASE_SOURCE),
        (EXTENSIONS_PATH, EXTENSIONS_XML),
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    let store = Store::open(root.path(), cache.path()).unwrap();
    InheritanceCacheFixture {
        root,
        _cache: cache,
        store,
    }
}

fn php_cache_facts(store: &Store, revision: &str, version: i64) -> String {
    store
        .conn
        .query_row(
            "SELECT facts FROM extraction_cache WHERE revision=?1 AND grammar='php' AND version=?2",
            params![revision, version],
            |row| row.get(0),
        )
        .unwrap()
}

fn php_marker_count(extraction: &Extraction) -> usize {
    extraction
        .relationships
        .iter()
        .filter(|relationship| relationship.kind == MARKER_KIND)
        .count()
}

fn prior_extraction_cache_version(revision: u32) -> i64 {
    let mut version = blake3::Hasher::new();
    version.update(b"trufflepig-extraction-cache-v1");
    version.update(&revision.to_le_bytes());
    version.update(include_bytes!("../../Cargo.lock"));
    i64::from_le_bytes(version.finalize().as_bytes()[..8].try_into().unwrap())
}

#[test]
fn extraction_revision_rebuilds_markerless_php_cache_facts() {
    let mut fixture = inheritance_cache_fixture();
    let extracted = extract::extract(CHILD_PATH, CHILD_SOURCE.as_bytes());
    assert!(
        php_marker_count(&extracted) > 0,
        "fixture must produce PHP markers"
    );

    let mut stale = extracted;
    stale
        .relationships
        .retain(|relationship| relationship.kind != MARKER_KIND);
    assert_eq!(php_marker_count(&stale), 0);
    let revision = blake3::hash(CHILD_SOURCE.as_bytes()).to_hex().to_string();
    let prior_version = prior_extraction_cache_version(4);
    assert_ne!(prior_version, scan::extraction_cache_version());
    fixture
        .store
        .conn
        .execute(
            "INSERT INTO extraction_cache(revision,grammar,version,facts) VALUES(?1,'php',?2,?3)",
            params![
                revision,
                prior_version,
                serde_json::to_string(&stale).unwrap()
            ],
        )
        .unwrap();

    fixture.store.index().unwrap();

    let current_version = scan::extraction_cache_version();
    let refreshed: Extraction =
        serde_json::from_str(&php_cache_facts(&fixture.store, &revision, current_version)).unwrap();
    assert!(
        php_marker_count(&refreshed) > 0,
        "the upgraded extraction contract must re-extract missing PHP markers"
    );
    let stale_rows: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM extraction_cache WHERE revision=?1 AND grammar='php' AND version=?2",
            params![revision, prior_version],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale_rows, 0);
}

fn scan_fingerprint(fixture: &InheritanceCacheFixture) -> String {
    let mut staged = Connection::open_in_memory().unwrap();
    schema::create(&staged).unwrap();
    scan::stage(
        &mut staged,
        &fixture.store.conn,
        &fixture.store.root,
        &fixture.store.cache,
    )
    .unwrap()
    .1
}

fn xenforo_graph(store: &Store) -> Vec<(String, i64, Option<i64>, i64, i64, String)> {
    let mut statement = store
        .conn
        .prepare(
            "SELECT kind,source,target,start,end,provenance FROM relationships WHERE kind LIKE 'xenforo_%' ORDER BY kind,source,target,start,end,provenance",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[test]
fn resolver_revision_rebuild_reuses_extraction_cache() {
    let mut fixture = inheritance_cache_fixture();
    fixture.store.index().unwrap();
    let generation = fixture.store.generation().unwrap();
    let cache_version = scan::extraction_cache_version();
    let revision = blake3::hash(CHILD_SOURCE.as_bytes()).to_hex().to_string();
    let mut extraction: Extraction =
        serde_json::from_str(&php_cache_facts(&fixture.store, &revision, cache_version)).unwrap();
    extraction.occurrences.push(Occurrence {
        name: "resolver_revision_cache_probe".into(),
        start: 0,
        end: 1,
        role: "cache_reuse_probe".into(),
        target: None,
        candidates: Vec::new(),
        provenance: "test_fixture".into(),
    });
    fixture
        .store
        .conn
        .execute(
            "UPDATE extraction_cache SET facts=?1 WHERE revision=?2 AND grammar='php' AND version=?3",
            params![serde_json::to_string(&extraction).unwrap(), revision, cache_version],
        )
        .unwrap();

    let scan_fingerprint = scan_fingerprint(&fixture);
    let resolver_revision = php_resolver::SHARED_RESOLVER_REVISION;
    let current_fingerprint = scan::resolved_fingerprint(&scan_fingerprint, resolver_revision);
    let previous_fingerprint =
        scan::resolved_fingerprint(&scan_fingerprint, "previous-shared-resolver-revision");
    assert_ne!(current_fingerprint, previous_fingerprint);
    fixture
        .store
        .conn
        .execute(
            "UPDATE meta SET value=?1 WHERE key='fingerprint'",
            [previous_fingerprint],
        )
        .unwrap();

    fixture.store.index().unwrap();

    assert_eq!(fixture.store.generation().unwrap(), generation + 1);
    let published_fingerprint: String = fixture
        .store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key='fingerprint'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(published_fingerprint, current_fingerprint);
    assert_eq!(scan::extraction_cache_version(), cache_version);
    let probes: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM occurrences o JOIN files f ON f.id=o.file_id WHERE f.path=?1 AND o.role='cache_reuse_probe' AND o.name='resolver_revision_cache_probe'",
            [CHILD_PATH],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        probes, 1,
        "the unchanged PHP extraction cache entry was reused"
    );
    let cache_rows: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM extraction_cache WHERE revision=?1 AND grammar='php' AND version=?2",
            params![revision, cache_version],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cache_rows, 1);
}

#[test]
fn changed_metadata_rebuild_does_not_duplicate_derived_graph() {
    let mut fixture = inheritance_cache_fixture();
    fixture.store.index().unwrap();
    let generation = fixture.store.generation().unwrap();
    let before = xenforo_graph(&fixture.store);
    assert!(
        !before.is_empty(),
        "fixture must publish XenForo graph facts"
    );
    std::fs::write(
        fixture.root.path().join(EXTENSIONS_PATH),
        format!("{EXTENSIONS_XML}<!-- metadata bytes changed -->"),
    )
    .unwrap();

    fixture.store.index().unwrap();

    assert_eq!(fixture.store.generation().unwrap(), generation + 1);
    assert_eq!(xenforo_graph(&fixture.store), before);
}

#[test]
fn staged_resolver_failure_preserves_published_generation_and_graph() {
    let mut fixture = inheritance_cache_fixture();
    fixture.store.index().unwrap();
    let generation = fixture.store.generation().unwrap();
    let before_graph = xenforo_graph(&fixture.store);
    assert!(
        !before_graph.is_empty(),
        "fixture must publish XenForo graph facts"
    );
    let before_revision: String = fixture
        .store
        .conn
        .query_row(
            "SELECT revision FROM files WHERE path=?1",
            [EXTENSIONS_PATH],
            |row| row.get(0),
        )
        .unwrap();
    std::fs::write(
        fixture.root.path().join(EXTENSIONS_PATH),
        format!("{EXTENSIONS_XML}<!-- force a staged scan -->"),
    )
    .unwrap();
    let mut resolver_called = false;

    let error = fixture
        .store
        .index_with_resolver(|_staged| {
            resolver_called = true;
            anyhow::bail!("injected resolver bound failure")
        })
        .unwrap_err();

    assert!(resolver_called);
    assert!(
        error
            .to_string()
            .contains("injected resolver bound failure")
    );
    assert_eq!(fixture.store.generation().unwrap(), generation);
    assert_eq!(xenforo_graph(&fixture.store), before_graph);
    let published_revision: String = fixture
        .store
        .conn
        .query_row(
            "SELECT revision FROM files WHERE path=?1",
            [EXTENSIONS_PATH],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(published_revision, before_revision);
}

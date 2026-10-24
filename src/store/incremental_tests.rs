//! Incremental publication: the stat shortcut, racy timestamps, diff
//! publication, and equivalence with a full rebuild of the same tree.
mod canonical_index;

use super::scan::{TEST_PASS_START_SKEW_NS, TEST_SOURCE_READS};
use super::*;
use canonical_index::assert_same_index;
use std::time::{Duration, SystemTime};

/// A root and its persistent incremental cache; each check rebuilds a fresh cache.
struct Tree {
    directory: tempfile::TempDir,
    store: Store,
    /// Explicit distinct mtimes make stats differ for every write, so a pass
    /// may trust recorded stats while the pass clock is skewed forward.
    trusted_stats: bool,
    writes: u64,
}

impl Tree {
    fn new(trusted_stats: bool) -> Self {
        TEST_PASS_START_SKEW_NS.set(if trusted_stats { 60_000_000_000 } else { 0 });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let store = Store::open(&root, &directory.path().join("cache")).unwrap();
        Self {
            directory,
            store,
            trusted_stats,
            writes: 0,
        }
    }

    fn write(&mut self, path: &str, body: &str) {
        let path = self.store.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        if self.trusted_stats {
            self.writes += 1;
            let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000 + self.writes);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        }
    }

    fn remove(&self, path: &str) {
        std::fs::remove_file(self.store.root.join(path)).unwrap();
    }

    fn rename(&self, from: &str, to: &str) {
        std::fs::rename(self.store.root.join(from), self.store.root.join(to)).unwrap();
    }

    /// Indexes incrementally, then compares with a full rebuild in a fresh cache.
    fn check(&mut self, step: &str) {
        self.store.index().unwrap();
        let rebuilt_cache = self
            .directory
            .path()
            .join(format!("rebuild-{}", step.replace(' ', "-")));
        let mut rebuilt = Store::open(&self.store.root, &rebuilt_cache).unwrap();
        rebuilt.index().unwrap();
        assert_same_index(step, &self.store.conn, &rebuilt.conn);
    }

    fn generation(&self) -> i64 {
        self.store.generation().unwrap()
    }
}

const BASE_PHP: &str = "<?php\nnamespace App;\nclass Base { public function run() {} }\n";
const CHILD_PHP: &str =
    "<?php\nnamespace App;\nclass Child extends Base { public function go() { $this->run(); } }\n";

fn edit_sequence(trusted_stats: bool) {
    let mut tree = Tree::new(trusted_stats);
    tree.write(
        "src/a.rs",
        "pub fn shared() {}\npub fn alpha() { beta(); }\n",
    );
    tree.write("src/b.rs", "pub fn beta() { shared(); }\n");
    tree.write("src/c.rs", "fn gamma() { shared(); alpha(); }\n");
    tree.write(
        "web/main.ts",
        "import { helper } from './util';\nhelper();\n",
    );
    tree.write("web/util.ts", "export function helper() {}\n");
    tree.write("php/Base.php", BASE_PHP);
    tree.write("php/Child.php", CHILD_PHP);
    tree.write("notes.txt", "free text\n");
    tree.check("initial");
    tree.write(
        "src/a.rs",
        "// moved down\npub fn shared() {}\npub fn alpha() { beta(); }\n",
    );
    tree.check("span shift");
    tree.write("src/a.rs", "pub fn alpha() { beta(); }\n");
    tree.write(
        "src/b.rs",
        "pub fn beta() { shared(); }\npub fn shared() {}\n",
    );
    tree.check("definition moved between files");
    tree.write(
        "src/d.rs",
        "pub fn alpha() {}\npub fn delta() { gamma(); }\n",
    );
    tree.remove("notes.txt");
    tree.check("addition and deletion");
    tree.rename("web/util.ts", "web/helpers.ts");
    tree.check("rename breaks import");
    tree.write(
        "web/main.ts",
        "import { helper } from './helpers';\nhelper();\n",
    );
    tree.check("import follows rename");
    tree.write("php/Base.php", &BASE_PHP.replace("run()", "execute()"));
    tree.check("inherited method renamed");
    tree.write("src/c.rs", "fn gamma() {\0}\n");
    tree.write(".ignore", "src/d.rs\n");
    tree.check("binary and ignored");
    tree.write("src/c.rs", "fn gamma() { shared(); alpha(); }\n");
    tree.write("php/Base.php", BASE_PHP);
    tree.check("reverted");
    tree.store
        .conn
        .execute(
            "UPDATE meta SET value='stale' WHERE key='regions_contract'",
            [],
        )
        .unwrap();
    tree.write(
        "src/b.rs",
        "pub fn beta() { shared(); alpha(); }\npub fn shared() {}\n",
    );
    tree.check("stale region contract");
}

#[test]
fn incremental_index_matches_full_rebuild_with_racy_stats() {
    edit_sequence(false);
}

#[test]
fn incremental_index_matches_full_rebuild_with_trusted_stats() {
    edit_sequence(true);
}

fn stat_recorded(store: &Store, path: &str) -> bool {
    store
        .conn
        .query_row(
            "SELECT stat_mtime_ns IS NOT NULL FROM files WHERE path=?1",
            [path],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn racy_timestamps_are_not_trusted() {
    let Tree {
        directory: _directory,
        mut store,
        ..
    } = Tree::new(false);
    let path = store.root.join("lib.rs");
    std::fs::write(&path, "fn alpha() {}\n").unwrap();
    store.index().unwrap();
    assert!(
        !stat_recorded(&store, "lib.rs"),
        "a fresh file's stat is racy"
    );
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, "fn omega() {}\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    store.index().unwrap();
    let revision: String = store
        .conn
        .query_row(
            "SELECT revision FROM files WHERE path='lib.rs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        revision,
        ContentRevision::of(b"fn omega() {}\n").to_string()
    );

    TEST_PASS_START_SKEW_NS.set(60_000_000_000);
    std::fs::write(&path, "fn sigma() {}\n").unwrap();
    store.index().unwrap();
    assert!(
        stat_recorded(&store, "lib.rs"),
        "a safely old stat is recorded"
    );
}

/// `total_changes()` counts every row this connection has written.
fn total_changes(store: &Store) -> i64 {
    store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn unchanged_pass_reads_and_writes_nothing() {
    let mut tree = Tree::new(true);
    tree.write("lib.rs", "pub fn alpha() {}\n");
    tree.write("main.rs", "fn main() { alpha(); }\n");
    tree.store.index().unwrap();
    let generation = tree.generation();
    let publication = tree.store.publication().unwrap();
    let observations = tree.store.observations_since(0).unwrap().changes.len();
    TEST_SOURCE_READS.set(0);
    let changes = total_changes(&tree.store);
    tree.store.index().unwrap();
    assert_eq!(TEST_SOURCE_READS.get(), 0);
    assert_eq!(total_changes(&tree.store), changes);
    assert_eq!(tree.generation(), generation);
    assert_eq!(tree.store.publication().unwrap(), publication);
    assert_eq!(
        tree.store.observations_since(0).unwrap().changes.len(),
        observations
    );

    // A touched file is read once, publishes nothing, and records its new stat.
    let touched = SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
    std::fs::File::options()
        .write(true)
        .open(tree.store.root.join("lib.rs"))
        .unwrap()
        .set_modified(touched)
        .unwrap();
    tree.store.index().unwrap();
    assert_eq!(TEST_SOURCE_READS.get(), 1);
    assert_eq!(tree.generation(), generation);
    tree.store.index().unwrap();
    assert_eq!(TEST_SOURCE_READS.get(), 1);
}

#[test]
fn one_file_edit_rewrites_only_that_file() {
    let mut tree = Tree::new(true);
    tree.write("lib.rs", "pub fn alpha() {}\npub fn beta() {}\n");
    tree.write("main.rs", "fn main() { alpha(); beta(); }\n");
    tree.write("other.rs", "fn other() { beta(); }\n");
    tree.store.index().unwrap();
    let ids = |store: &Store| -> Vec<(String, i64)> {
        let mut statement = store
            .conn
            .prepare("SELECT f.path||':'||o.name||':'||o.start,o.id FROM occurrences o JOIN files f ON f.id=o.file_id ORDER BY 1")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let before = ids(&tree.store);
    TEST_SOURCE_READS.set(0);
    tree.write("lib.rs", "// header\npub fn alpha() {}\npub fn beta() {}\n");
    tree.store.index().unwrap();
    assert_eq!(TEST_SOURCE_READS.get(), 1);
    let after = ids(&tree.store);
    let kept = |path: &str| {
        before
            .iter()
            .filter(|(key, _)| key.starts_with(path))
            .all(|row| after.contains(row))
    };
    assert!(
        kept("main.rs:") && kept("other.rs:"),
        "callers keep their rows"
    );
}

#[test]
fn journal_records_every_kind_of_change() {
    let mut tree = Tree::new(true);
    for path in ["edited.txt", "deleted.txt", "ignored.txt", "binary.txt"] {
        tree.write(path, "before");
    }
    tree.store.index().unwrap();
    let baseline = tree.generation();
    tree.write("edited.txt", "after!");
    tree.write("added.txt", "new");
    tree.write("binary.txt", "\0\0\0\0\0\0");
    tree.write(".ignore", "ignored.txt\n");
    tree.remove("deleted.txt");
    tree.store.index().unwrap();
    let observations = tree.store.observations_since(baseline).unwrap();
    assert!(observations.complete);
    let kinds: Vec<(&str, &str, Option<ContentRevision>, Option<ContentRevision>)> = observations
        .changes
        .iter()
        .map(|change| {
            (
                change.path.as_str(),
                change.kind.as_str(),
                change.before_revision,
                change.after_revision,
            )
        })
        .collect();
    let before = Some(ContentRevision::of(b"before"));
    assert_eq!(
        kinds,
        vec![
            (
                ".ignore",
                "added",
                None,
                Some(ContentRevision::of(b"ignored.txt\n"))
            ),
            (
                "added.txt",
                "added",
                None,
                Some(ContentRevision::of(b"new"))
            ),
            ("binary.txt", "coverage_changed", before, None),
            ("deleted.txt", "deleted", before, None),
            (
                "edited.txt",
                "modified",
                before,
                Some(ContentRevision::of(b"after!"))
            ),
            ("ignored.txt", "ignored_or_excluded", before, None),
        ]
    );
    assert!(
        observations
            .changes
            .iter()
            .all(|change| change.generation == baseline + 1)
    );
}

#[test]
fn lexical_documents_follow_edits_between_optimizations() {
    let mut tree = Tree::new(true);
    for file in 0..20 {
        tree.write(
            &format!("f{file}.rs"),
            &format!("pub fn unchanged_{file}() {{}}\n"),
        );
    }
    tree.write("edited.rs", "pub fn obsolete_marker() {}\n");
    tree.store.index().unwrap();
    tree.write("edited.rs", "pub fn replacement_marker() {}\n");
    tree.store.index().unwrap();
    let churn: i64 = tree
        .store
        .conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='documents_churn'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(churn > 0, "a small edit defers optimize");
    let matches = |term: &str| -> i64 {
        tree.store
            .conn
            .query_row(
                "SELECT count(*) FROM documents WHERE documents MATCH ?1",
                [term],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(matches("obsolete_marker"), 0);
    assert_eq!(matches("replacement_marker"), 1);
    assert_eq!(matches("unchanged_7"), 1);
    tree.store
        .conn
        .execute_batch("INSERT INTO documents(documents) VALUES('integrity-check');")
        .unwrap();
}

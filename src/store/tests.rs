use super::*;
use std::os::unix::ffi::OsStringExt;

fn fixture() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let store = Store::open(&root, &directory.path().join("cache")).unwrap();
    (directory, store)
}

#[test]
fn identical_index_preserves_generation_and_prunes_content() {
    let (_directory, mut store) = fixture();
    std::fs::write(store.root.join("README.md"), "first content").unwrap();
    store.index().unwrap();
    let generation = store.generation().unwrap();
    store.index().unwrap();
    assert_eq!(store.generation().unwrap(), generation);
    std::fs::write(store.root.join("README.md"), "second content").unwrap();
    store.index().unwrap();
    assert_eq!(store.generation().unwrap(), generation + 1);
    let retained: i64 = store
        .conn
        .query_row("SELECT count(*) FROM contents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(retained, 1);
}

#[test]
fn failed_publication_keeps_complete_previous_snapshot() {
    let (_directory, mut store) = fixture();
    std::fs::write(store.root.join("README.md"), "original").unwrap();
    store.index().unwrap();
    let generation = store.generation().unwrap();
    store.conn.execute_batch("CREATE TRIGGER fail_publish BEFORE INSERT ON files BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    std::fs::write(store.root.join("README.md"), "replacement").unwrap();
    assert!(store.index().is_err());
    assert_eq!(store.generation().unwrap(), generation);
    let text: String = store
        .conn
        .query_row("SELECT body FROM regions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(text, "original");
    let hits: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM documents WHERE documents MATCH 'original'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hits, 1);
    store
        .conn
        .execute_batch("DROP TRIGGER fail_publish")
        .unwrap();
    store.index().unwrap();
    assert_eq!(store.generation().unwrap(), generation + 1);
}

#[test]
fn lexical_regions_cover_bom_crlf_invalid_utf8_and_gaps() {
    let (_directory, mut store) = fixture();
    let mut source = Vec::with_capacity(12_000);
    source.extend_from_slice(b"\xef\xbb\xbfintro\r\n");
    source.extend(std::iter::repeat_n(b'x', 9000));
    source.extend_from_slice(b"\xff tail\r\n");
    std::fs::write(store.root.join("text.md"), &source).unwrap();
    store.index().unwrap();
    let mut statement = store
        .conn
        .prepare("SELECT start,end FROM regions ORDER BY start")
        .unwrap();
    let spans: Vec<(usize, usize)> = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get::<_, i64>(1)? as usize,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(spans[0].0, 0);
    assert_eq!(spans.last().unwrap().1, source.len());
    assert!(spans.iter().all(|&(start, end)| end - start <= 4096));
    assert!(spans.windows(2).all(|pair| pair[0].1 == pair[1].0));
}

#[test]
fn global_candidates_refresh_for_unchanged_callers() {
    let (_directory, mut store) = fixture();
    std::fs::write(store.root.join("caller.rs"), "fn caller() { target(); }").unwrap();
    std::fs::write(store.root.join("target.rs"), "pub fn target() {}\n").unwrap();
    store.index().unwrap();
    let (resolved, candidates): (Option<i64>, String) = store
        .conn
        .query_row(
            "SELECT target,candidates FROM occurrences WHERE name='target' AND role='call'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(resolved.is_none());
    assert_ne!(candidates, "[]");
    std::fs::write(store.root.join("target.rs"), "pub fn replacement() {}\n").unwrap();
    store.index().unwrap();
    let candidates: String = store
        .conn
        .query_row(
            "SELECT candidates FROM occurrences WHERE name='target' AND role='call'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(candidates, "[]");
}

#[test]
fn configuration_changes_and_resource_exclusions_are_visible() {
    let (_directory, mut store) = fixture();
    std::fs::write(store.root.join(".luaurc"), "{}").unwrap();
    let file = std::fs::File::create(store.root.join("huge.rs")).unwrap();
    file.set_len(3 * 1024 * 1024).unwrap();
    let coverage = store.index().unwrap();
    assert_eq!(coverage.indexed_files, 1);
    assert_eq!(coverage.excluded_files, 1);
    let generation = store.generation().unwrap();
    std::fs::write(store.root.join(".luaurc"), "{\"aliases\":{}}").unwrap();
    store.index().unwrap();
    assert_eq!(store.generation().unwrap(), generation + 1);
    let excluded: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM files WHERE status='resource_excluded'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(excluded, 1);
    let empty_regions: i64 = store
        .conn
        .query_row("SELECT count(*) FROM regions WHERE start=end", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(empty_regions, 0);
}

#[test]
fn paths_round_trip_literal_percent_control_and_non_utf8() {
    let path = PathBuf::from(std::ffi::OsString::from_vec(
        b"space name/%0A\n\xff.rs".to_vec(),
    ));
    let encoded = encode_path(&path);
    assert!(!encoded.contains('\n'));
    assert_eq!(decode_path(&encoded).unwrap(), path);
    assert!(decode_path("bad%XX").is_err());
    assert!(decode_path("nul%00").is_err());
    assert_eq!(encode_path(Path::new("a:1-2@3")), "a%3A1-2%403");
}

#[test]
fn modules_preserve_require_uncertainty_and_resolve_static_relative_imports() {
    let (_directory, mut store) = fixture();
    std::fs::write(store.root.join("util.ts"), "export function util() {}\n").unwrap();
    std::fs::write(
        store.root.join("caller.ts"),
        "import { util } from './util.ts';\nfunction f(require) { require('./util'); }\n",
    )
    .unwrap();
    let coverage = store.index().unwrap();
    assert_eq!(coverage.parse_failures, 0);
    let import_target: Option<i64> = store
        .conn
        .query_row(
            "SELECT target FROM occurrences WHERE role='import_path'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let (require_target, candidates): (Option<i64>, String) = store
        .conn
        .query_row(
            "SELECT target,candidates FROM occurrences WHERE role='require'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(import_target.is_some());
    assert!(require_target.is_none());
    assert_ne!(candidates, "[]");
    let kinds: String = store
        .conn
        .query_row(
            "SELECT group_concat(kind, ',') FROM relationships WHERE kind LIKE 'import%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(kinds.contains("import_candidate"));
}

#[test]
fn configuration_alias_changes_refresh_module_candidates() {
    let (_directory, mut store) = fixture();
    std::fs::create_dir(store.root.join("first")).unwrap();
    std::fs::create_dir(store.root.join("second")).unwrap();
    std::fs::write(store.root.join("first/util.ts"), "export const util = 1;").unwrap();
    std::fs::write(store.root.join("second/util.ts"), "export const util = 2;").unwrap();
    std::fs::write(
        store.root.join("caller.ts"),
        "import { util } from '@lib/util';",
    )
    .unwrap();
    std::fs::write(
        store.root.join("tsconfig.json"),
        "{\"compilerOptions\": {\"paths\": {\"@lib/*\": [\"first/*\"], // comment\n},},}",
    )
    .unwrap();
    store.index().unwrap();
    let query = "SELECT files.path FROM occurrences JOIN definitions ON definitions.id=json_extract(occurrences.candidates,'$[0]') JOIN files ON files.id=definitions.file_id WHERE occurrences.role='import_path'";
    let first: String = store.conn.query_row(query, [], |row| row.get(0)).unwrap();
    assert_eq!(first, "first/util.ts");
    std::fs::write(
        store.root.join("tsconfig.json"),
        "{\"compilerOptions\":{\"paths\":{\"@lib/*\":[\"second/*\"]}}}",
    )
    .unwrap();
    store.index().unwrap();
    let second: String = store.conn.query_row(query, [], |row| row.get(0)).unwrap();
    assert_eq!(second, "second/util.ts");
}

#[test]
fn luau_alias_rojo_and_conditional_dm_include_have_candidate_edges() {
    let (_directory, mut store) = fixture();
    std::fs::create_dir(store.root.join("shared")).unwrap();
    std::fs::write(store.root.join("shared/module.luau"), "return {}\n").unwrap();
    std::fs::write(
        store.root.join(".luaurc"),
        "{\"aliases\":{\"shared\":\"shared\"}}",
    )
    .unwrap();
    std::fs::write(
        store.root.join("trufflepig.json"),
        "{\"rojo_project\":\"default.project.json\"}",
    )
    .unwrap();
    std::fs::write(
        store.root.join("default.project.json"),
        "{\"tree\":{\"ReplicatedStorage\":{\"$path\":\"shared\"}}}",
    )
    .unwrap();
    std::fs::write(
        store.root.join("caller.luau"),
        "local a = require('@shared/module')\nlocal b = require(game.ReplicatedStorage.module)\n",
    )
    .unwrap();
    std::fs::write(
        store.root.join("world.dme"),
        "#ifdef FEATURE\n#include \"feature.dm\"\n#endif\n",
    )
    .unwrap();
    std::fs::write(store.root.join("feature.dm"), "/obj/feature\n").unwrap();
    store.index().unwrap();
    let count: i64 = store.conn.query_row("SELECT count(*) FROM occurrences WHERE role IN ('require','include') AND target IS NULL AND candidates<>'[]'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 3);
    let edges: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM relationships WHERE kind='include_candidate'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(edges, 1);
}
#[test]
fn interrupted_staging_is_discarded_without_replacing_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn intact() {}\n").unwrap();
    let mut store = super::Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let generation = store.generation().unwrap();
    let abandoned = tempfile::Builder::new()
        .prefix("stage-")
        .tempdir_in(cache.path())
        .unwrap()
        .keep();
    std::fs::write(abandoned.join("index.sqlite3"), b"interrupted extraction").unwrap();
    store.index().unwrap();
    assert!(!abandoned.exists());
    assert_eq!(store.generation().unwrap(), generation);
    assert!(super::Store::open(root.path(), root.path()).is_err());
}

#[test]
fn linked_worktree_git_file_is_not_indexed() {
    let (_directory, mut store) = fixture();
    std::fs::write(
        store.root.join(".git"),
        "gitdir: /nowhere/.git/worktrees/x\n",
    )
    .unwrap();
    std::fs::write(store.root.join("lib.rs"), "fn indexed() {}\n").unwrap();
    store.index().unwrap();
    let paths: Vec<String> = store
        .conn
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(paths, ["lib.rs"]);
    assert_eq!(store.coverage().unwrap().total_files, 1);
}

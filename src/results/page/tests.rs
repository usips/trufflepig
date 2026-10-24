use super::*;
use crate::{
    output::{OutputBudget, OutputFormat},
    query_deadline::QueryDeadline,
    results::{ChangeEntry, Hit, ResultEntry, ResultSet, StoredResultSet, load_entries},
    store::{Store, read_index_progress},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn create_sources(root: &Path) -> Result<()> {
    fs::create_dir_all(root)?;
    for number in 0..512 {
        fs::write(
            root.join(format!("source-{number:04}.txt")),
            format!("Retained page observation {number}.\n"),
        )?;
    }
    Ok(())
}

fn retain_real_progress(
    directory: &Path,
    root: &Path,
    cache: &Path,
    store: &mut Store,
) -> Result<()> {
    let claim = directory.join("page-index-claim");
    fs::write(&claim, "active")?;
    let watch_root = root.to_path_buf();
    let watch_cache = cache.to_path_buf();
    let watch_claim = claim.clone();
    let watcher = thread::spawn(move || {
        let stop_at = Instant::now() + Duration::from_secs(4);
        loop {
            if read_index_progress(&watch_root, &watch_cache)
                .is_some_and(|progress| progress.completed_files > 0)
            {
                fs::remove_file(&watch_claim).expect("cancel after retained work appears");
                return true;
            }
            if Instant::now() >= stop_at {
                let _ = fs::remove_file(&watch_claim);
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    let result = store
        .index_available_until(QueryDeadline::after(Duration::from_secs(5)).with_claim_path(claim));
    assert!(watcher.join().expect("progress watcher did not panic"));
    assert!(
        crate::coded_error::ErrorCode::TimedOut
            .is_code_of(&result.expect_err("claim cancellation interrupts the pass"))
    );
    assert!(read_index_progress(root, cache).is_some_and(|progress| progress.completed_files > 0));
    Ok(())
}

fn change(name: &str) -> ResultEntry {
    ResultEntry::Change(ChangeEntry {
        handle: String::new(),
        name: name.into(),
        status: "modified".into(),
        before: None,
        after: None,
        correspondence: "exact".into(),
    })
}

fn current_hit(name: &str, path: &str) -> Hit {
    Hit {
        handle: format!("read:path:root-digest:{path}:0-1@revision"),
        path: path.into(),
        revision: Some("revision".into()),
        start: 0,
        end: 1,
        start_line: 1,
        end_line: 1,
        name: name.into(),
        kind: "function".into(),
        container: None,
        provenance: Some("stateless_current_source".into()),
        resolution: None,
        candidates: Vec::new(),
        target: None,
        repeats: None,
        snippet: Some(crate::results::Snippet {
            line: 1,
            text: format!("fn {name}() {{}}"),
        }),
        differs: false,
    }
}

fn current_set(hits: Vec<Hit>, truncated: bool) -> ResultSet {
    ResultSet {
        generation: 0,
        coverage: json!({"endpoint":"stateless_current_source","absence":"exhaustive"}),
        truncated,
        hits,
    }
}

#[test]
fn stateless_member_pages_round_robin_and_preserve_coverage_and_fallback_metadata() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first_root = directory.path().join("first");
    let second_root = directory.path().join("second");
    let first = current_set(
        vec![current_hit("a0", "a.rs"), current_hit("a1", "a.rs")],
        false,
    );
    let second = current_set(
        vec![current_hit("b0", "b.rs"), current_hit("b1", "b.rs")],
        false,
    );
    let metadata = json!({
        "workspace":"space", "source":"current_source", "cache":"unused",
        "home":"first", "scope":"all", "truncated":false,
        "cache_fallback":{"status":"index_cache_unavailable","cache":"selected-cache",
            "reason":"selected cache is read-only"},
        "coverage":[
            {"member":"first","state":"searched","coverage":{"absence":"exhaustive"}},
            {"member":"second","state":"searched","coverage":{"absence":"exhaustive"}}
        ]
    });
    let parts = [
        (first_root.as_path(), Some("first"), &first),
        (second_root.as_path(), Some("second"), &second),
    ];
    let budget = OutputBudget::new(10_000)?;
    let rendered = stateless_page_members(&parts, &metadata, 3, &budget)?;
    let page: Value = serde_json::from_str(&rendered)?;

    assert_eq!(page["hits"][0]["name"], "a0");
    assert_eq!(page["hits"][1]["name"], "b0");
    assert_eq!(page["hits"][2]["name"], "a1");
    assert_eq!(page["hits"][0]["member"], "first");
    assert_eq!(
        page["members"]["first"],
        crate::store::encode_path(&first_root)
    );
    assert_eq!(page["coverage"][1]["coverage"]["absence"], "exhaustive");
    assert_eq!(page["cache_fallback"]["status"], "index_cache_unavailable");
    assert_eq!(page["next"], Value::Null);
    assert_eq!(page["truncated"], true);

    let lines_budget = OutputBudget::new(10_000)?.with_format(OutputFormat::Lines);
    let lines = stateless_page_members(&parts, &metadata, 3, &lines_budget)?;
    assert!(lines.contains("first/a.rs:1-1"));
    assert!(lines.contains("second/b.rs:1-1"));
    assert!(lines.contains("cache_fallback"));
    assert!(lines.contains("truncated"));
    Ok(())
}

#[test]
fn stateless_single_page_keeps_auto_fallback_metadata() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root");
    let set = current_set(Vec::new(), false);
    let metadata = json!({"cache_fallback":{"status":"index_cache_unavailable",
        "cache":"selected-cache","reason":"read-only"}});
    let rendered = stateless_page(&root, None, &set, &metadata, 20, &OutputBudget::new(1_000)?)?;
    let page: Value = serde_json::from_str(&rendered)?;
    assert_eq!(page["cache_fallback"]["status"], "index_cache_unavailable");
    assert_eq!(page["coverage"]["absence"], "exhaustive");
    assert_eq!(page["hits"], json!([]));
    Ok(())
}

#[test]
fn standalone_more_refreshes_progress_after_explicit_index_without_mutating_results() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root");
    let cache = directory.path().join("cache");
    create_sources(&root)?;
    let mut store = Store::open(&root, &cache)?;
    let id = store.result_sets()?.save(StoredResultSet {
        generation: 17,
        coverage: json!({
            "endpoint":"live",
            "observed_generation": 9,
            "index_progress":{"phase":"maintenance_required","completed_files":1,
                "reused_files":0,"maintenance_required":true}
        }),
        truncated: false,
        hits: vec![change("first"), change("second")],
    })?;

    retain_real_progress(directory.path(), &root, &cache, &mut store)?;
    assert_eq!(store.generation()?, 0);
    let budget = OutputBudget::new(4_096)?;
    let first: Value = serde_json::from_str(&page(&store, &id, 0, 1, &budget)?)?;
    assert!(
        first["coverage"]["index_progress"]["completed_files"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(first["coverage"]["observed_generation"], 9);
    assert_eq!(first["generation"], 17);
    let cursor = first["next"].as_str().expect("first page has a cursor");
    let lines = page(
        &store,
        &id,
        0,
        1,
        &OutputBudget::new(4_096)?.with_format(OutputFormat::Lines),
    )?;
    // The claim cancellation lands wherever the pass is, so any phase may be reported.
    assert!(lines.contains("(retained preparation:"), "{lines}");

    store.index()?;
    assert!(read_index_progress(&root, &cache).is_none());
    let second: Value = serde_json::from_str(&more(&store, cursor, 1, &budget)?)?;
    assert!(second["coverage"].get("index_progress").is_none());
    assert_eq!(second["coverage"]["observed_generation"], 9);
    assert_eq!(second["generation"], 17);
    assert_eq!(second["hits"][0]["name"], "second");

    let saved = load_entries(&store, &id)?;
    assert_eq!(saved.coverage["observed_generation"], 9);
    assert_eq!(
        saved.coverage["index_progress"]["phase"],
        "maintenance_required"
    );
    assert_eq!(saved.hits.len(), 2);
    Ok(())
}

use super::*;
use crate::{
    daemon::deadline::{QueryDeadline, is_timed_out},
    output::{OutputBudget, OutputFormat},
    results::{ChangeEntry, ResultEntry, StoredResultSet, load_entries},
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
    assert!(is_timed_out(
        &result.expect_err("claim cancellation interrupts the pass")
    ));
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
    assert!(
        lines.contains("index scanning (retained preparation:"),
        "{lines}"
    );

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

use super::*;
use crate::{
    daemon::deadline::{QueryDeadline, is_timed_out},
    output::{OutputBudget, OutputFormat},
    results::{ChangeEntry, ResultEntry},
    store::{Store, encode_path, read_index_progress},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn create_sources(root: &Path) -> Result<()> {
    fs::create_dir_all(root)?;
    for number in 0..512 {
        fs::write(
            root.join(format!("source-{number:04}.txt")),
            format!("Retained workspace observation {number}.\n"),
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
    let claim = directory.join("workspace-index-claim");
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

fn open_results(cache: &Path, deadline: &QueryDeadline) -> Result<WorkspaceResults> {
    WorkspaceResults::create(cache, deadline)?;
    WorkspaceResults::open(cache, deadline)
}

#[test]
fn workspace_more_refreshes_saved_source_until_explicit_index_completes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("member");
    let member_cache = directory.path().join("member-cache");
    let workspace_cache = directory.path().join("workspace-cache");
    create_sources(&root)?;
    let mut store = Store::open(&root, &member_cache)?;
    let source = IndexProgressSource::capture(&root, &member_cache)
        .expect("verified fixture paths are canonical");
    let root_metadata = root.metadata()?;
    let owner = MemberSnapshot {
        name: "member".into(),
        root: encode_path(&root),
        cache: encode_path(&member_cache),
        device: root_metadata.dev(),
        inode: root_metadata.ino(),
        index_identity: "unpublished".into(),
        generation: 0,
        coverage: json!({
            "index_progress":{"phase":"maintenance_required","completed_files":1,
                "reused_files":0,"maintenance_required":true}
        }),
        worktree: None,
        parent_index: None,
    };
    let deadline = QueryDeadline::after(Duration::from_secs(20));
    let results = open_results(&workspace_cache, &deadline)?;
    let id = results.save(WorkspaceSet {
        workspace: "test".into(),
        home: Some("member".into()),
        owners: vec![owner],
        coverage: vec![json!({
            "member":"member",
            "state":"searched",
            "generation":23,
            "index_progress":{"phase":"maintenance_required","completed_files":1,
                "reused_files":0,"maintenance_required":true},
            "index_progress_source":serde_json::to_value(source)?
        })],
        hits: vec![
            OwnedEntry {
                owner: 0,
                member_rank: 1,
                entry: change("first"),
                worktree_differs: false,
            },
            OwnedEntry {
                owner: 0,
                member_rank: 1,
                entry: change("second"),
                worktree_differs: false,
            },
        ],
        truncated: false,
        scope: None,
    })?;

    retain_real_progress(directory.path(), &root, &member_cache, &mut store)?;
    assert_eq!(store.generation()?, 0);
    let budget = OutputBudget::new(4_096)?;
    let first: Value = serde_json::from_str(&results.page(&id, 0, 1, &budget)?)?;
    assert!(
        first["coverage"][0]["index_progress"]["completed_files"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(first["coverage"][0].get("index_progress_source").is_none());
    assert_eq!(first["coverage"][0]["generation"], 23);
    assert_eq!(first["hits"][0]["name"], "first");
    let cursor = first["next"].as_str().expect("first page has a cursor");
    let lines = results.page(
        &id,
        0,
        1,
        &OutputBudget::new(4_096)?.with_format(OutputFormat::Lines),
    )?;
    // The claim cancellation lands wherever the pass is, so any phase may be reported.
    assert!(lines.contains("(retained preparation:"), "{lines}");
    assert!(!lines.contains("index_progress_source"), "{lines}");

    store.index()?;
    assert!(read_index_progress(&root, &member_cache).is_none());
    drop(results);
    let followup_deadline = QueryDeadline::after(Duration::from_secs(20));
    let results = WorkspaceResults::open(&workspace_cache, &followup_deadline)?;
    let second: Value = serde_json::from_str(&results.more(cursor, 1, &budget)?)?;
    assert!(second["coverage"][0].get("index_progress").is_none());
    assert!(second["coverage"][0].get("index_progress_source").is_none());
    assert_eq!(second["coverage"][0]["generation"], 23);
    assert_eq!(second["hits"][0]["name"], "second");

    let saved = results.load(&id)?;
    assert_eq!(saved.coverage[0]["generation"], 23);
    assert!(saved.coverage[0]["index_progress_source"]["root"].is_string());
    assert_eq!(
        saved.owners[0].coverage["index_progress"]["phase"],
        "maintenance_required"
    );
    Ok(())
}

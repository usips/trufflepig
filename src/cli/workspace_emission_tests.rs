use super::{emission::execute, run};
use crate::{
    diagnostics::{DiagnosticStore, DiagnosticsMode, EventStage, RequestEvent},
    store::encode_path,
    workspace::{config::WorkspaceConfig, member_cache},
};
use serde_json::Value;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    cache: tempfile::TempDir,
    config: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        for member in ["engine", "pack"] {
            fs::create_dir(root.path().join(member)).unwrap();
            fs::write(
                root.path().join(member).join("lib.rs"),
                format!("pub struct SharedThing {{ pub {member}_marker: u8 }}\n"),
            )
            .unwrap();
        }
        let config = root.path().join("trufflepig.workspace.toml");
        fs::write(&config, "[workspace]\nname='emission-test'\n[members.engine]\npath='engine'\n[members.pack]\npath='pack'\n").unwrap();
        Self {
            root,
            cache,
            config,
        }
    }

    fn args(&self, extra: &[&str]) -> Vec<String> {
        let mut args = vec![
            "--workspace".into(),
            self.config.display().to_string(),
            "--root".into(),
            self.root.path().join("engine").display().to_string(),
            "--cache".into(),
            self.cache.path().display().to_string(),
            "--no-daemon".into(),
            "--diagnostics".into(),
            "metadata".into(),
        ];
        args.extend(extra.iter().map(|word| (*word).to_owned()));
        args
    }

    fn member_cache(&self, name: &str) -> PathBuf {
        let config = WorkspaceConfig::load(&self.config).unwrap();
        member_cache(
            config
                .members
                .iter()
                .find(|member| member.name == name)
                .unwrap(),
            Some(self.cache.path()),
        )
        .unwrap()
    }
}

fn records(cache: &Path) -> Vec<RequestEvent> {
    let Ok(entries) = fs::read_dir(cache.join("diagnostics")) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            let bytes = fs::read(path).unwrap();
            records.extend(
                bytes
                    .split(|byte| *byte == b'\n')
                    .filter_map(|line| serde_json::from_slice::<RequestEvent>(line).ok()),
            );
        }
    }
    records
}

fn deliveries(cache: &Path, expected: usize) -> Vec<RequestEvent> {
    let started = Instant::now();
    loop {
        let events = records(cache)
            .into_iter()
            .filter(|event| event.stage == EventStage::Delivery)
            .collect::<Vec<_>>();
        if events.len() == expected {
            return events;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "expected {expected} deliveries, found {}",
            events.len()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn workspace_page_emission_records_foreign_identity_and_one_final_receipt() {
    let fixture = Fixture::new();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        execute(
            &fixture.args(&["sym:SharedThing"]),
            &mut stdout,
            &mut stderr
        ),
        0
    );
    assert!(stderr.is_empty());
    let value: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(value["hits"].as_array().unwrap().len(), 2);
    let cache = fixture.member_cache("engine");
    let events = deliveries(&cache, 1);
    let event = &events[0];
    assert_eq!(event.member.as_deref(), Some("engine"));
    assert_eq!(
        event.workspace.as_deref(),
        Some(WorkspaceConfig::load(&fixture.config).unwrap().id.as_str())
    );
    assert_eq!(event.emitted.len(), 2);
    for (rank, member) in ["engine", "pack"].iter().enumerate() {
        let identity = &event.emitted[rank];
        assert_eq!(identity.member.as_deref(), Some(*member));
        assert_eq!(
            identity.repository,
            encode_path(&fixture.root.path().join(member))
        );
        assert_eq!(identity.path, "lib.rs");
        assert_eq!(identity.original_rank, Some(rank + 1));
        assert!(!identity.source_body);
    }
    let receipt = event.receipt.as_ref().unwrap();
    assert!(receipt.complete);
    assert_eq!(receipt.accepted_bytes, stdout.len());
    assert_eq!(receipt.prepared_bytes, stdout.len());
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(
        receipt.prepared_tokens,
        tokenizer
            .encode_ordinary(std::str::from_utf8(&stdout).unwrap())
            .len()
    );
    assert!(receipt.prepared_tokens <= 600);
    assert!(
        records(&fixture.member_cache("pack"))
            .iter()
            .all(|event| event.receipt.is_none())
    );
    let audit = DiagnosticStore::open(&cache, DiagnosticsMode::Metadata)
        .unwrap()
        .audit(None)
        .unwrap();
    assert_eq!(audit.complete_deliveries, 1);
    assert_eq!(audit.surfaced_metadata_identities, 2);
    assert_eq!(audit.viewed_source_identities, 0);
    assert!(
        !serde_json::to_string(event)
            .unwrap()
            .contains("pack_marker")
    );
}

struct PartialWriter {
    accepted: usize,
}
impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.accepted != 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.accepted = bytes.len().min(7);
        Ok(self.accepted)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn workspace_source_emission_counts_complete_delivery_only() {
    let fixture = Fixture::new();
    let mut search_args = fixture.args(&["sym:SharedThing", "in:pack"]);
    let mode = search_args
        .iter()
        .position(|arg| arg == "metadata")
        .unwrap();
    search_args[mode] = "off".into();
    let page: Value = serde_json::from_str(&run(&search_args).unwrap()).unwrap();
    let handle = page["hits"][0]["handle"].as_str().unwrap();
    let args = fixture.args(&["show", handle]);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(execute(&args, &mut stdout, &mut stderr), 0);
    let cache = fixture.member_cache("engine");
    let events = deliveries(&cache, 1);
    let event = &events[0];
    assert_eq!(event.emitted.len(), 1);
    assert_eq!(event.emitted[0].member.as_deref(), Some("pack"));
    assert_eq!(
        event.emitted[0].repository,
        encode_path(&fixture.root.path().join("pack"))
    );
    assert!(event.emitted[0].source_body);
    assert_eq!(event.receipt.as_ref().unwrap().accepted_bytes, stdout.len());
    let shown: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(
        event.emitted[0].start_byte,
        shown["lines"][0]["start"].as_u64().unwrap() as usize
    );
    assert_eq!(
        event.emitted[0].end_byte,
        shown["lines"].as_array().unwrap().last().unwrap()["end"]
            .as_u64()
            .unwrap() as usize
    );

    let mut partial = PartialWriter { accepted: 0 };
    assert_eq!(execute(&args, &mut partial, &mut stderr), 2);
    let events = deliveries(&cache, 2);
    let incomplete = events
        .iter()
        .find(|event| !event.receipt.as_ref().unwrap().complete)
        .unwrap();
    assert_eq!(incomplete.receipt.as_ref().unwrap().accepted_bytes, 7);
    assert!(incomplete.receipt.as_ref().unwrap().prepared_bytes > 7);
    let audit = DiagnosticStore::open(&cache, DiagnosticsMode::Metadata)
        .unwrap()
        .audit(None)
        .unwrap();
    assert_eq!(audit.complete_deliveries, 1);
    assert_eq!(audit.incomplete_deliveries, 1);
    assert_eq!(audit.viewed_source_identities, 1);
    assert_eq!(audit.stdout_accepted_bytes, stdout.len() + 7);

    let mut silent = Vec::new();
    execute(
        &fixture.args(&["--budget", "0", "show", handle]),
        &mut silent,
        &mut stderr,
    );
    assert!(silent.is_empty());
    let events = deliveries(&cache, 3);
    let zero = events
        .iter()
        .find(|event| event.receipt.as_ref().unwrap().prepared_bytes == 0)
        .unwrap();
    assert!(zero.emitted.is_empty());
    assert_eq!(zero.receipt.as_ref().unwrap().accepted_bytes, 0);
    assert_eq!(zero.receipt.as_ref().unwrap().prepared_tokens, 0);
    let audit = DiagnosticStore::open(&cache, DiagnosticsMode::Metadata)
        .unwrap()
        .audit(None)
        .unwrap();
    assert_eq!(audit.viewed_source_identities, 1);
    assert_eq!(audit.surfaced_metadata_identities, 0);
}

#[test]
fn workspace_ancestor_does_not_redirect_explicit_subtree_diagnostics() {
    let fixture = Fixture::new();
    let subtree = fixture.root.path().join("engine/subtree");
    fs::create_dir(&subtree).unwrap();
    fs::write(subtree.join("lib.rs"), "struct SubtreeThing {}\n").unwrap();
    let args = vec![
        "--root".into(),
        subtree.display().to_string(),
        "--no-daemon".into(),
        "--cache".into(),
        fixture.cache.path().display().to_string(),
        "--diagnostics".into(),
        "metadata".into(),
        "sym:SubtreeThing".into(),
    ];
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(execute(&args, &mut stdout, &mut stderr), 0);
    let value: Value = serde_json::from_slice(&stdout).unwrap();
    assert!(value.get("workspace").is_none());
    let events = deliveries(fixture.cache.path(), 1);
    let event = &events[0];
    assert!(event.workspace.is_none());
    assert!(event.member.is_none());
    assert_eq!(event.emitted.len(), 1);
    assert_eq!(event.emitted[0].repository, encode_path(&subtree));
    assert!(event.emitted[0].member.is_none());
    assert!(!fixture.member_cache("engine").join("diagnostics").exists());
    let audit = DiagnosticStore::open(fixture.cache.path(), DiagnosticsMode::Metadata)
        .unwrap()
        .audit(None)
        .unwrap();
    assert_eq!(audit.complete_deliveries, 1);
    assert_eq!(audit.surfaced_metadata_identities, 1);
}

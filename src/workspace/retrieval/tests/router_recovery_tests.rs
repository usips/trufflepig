mod explicit_scope_tests;
mod failure_tests;

use super::super::{
    HomeIndexPolicy, MemberCommand, SearchedMember, WorkspaceQuery, search_member,
    search_with_policy_using,
};
use crate::{
    cli,
    workspace::{
        WorkspaceConfig, cache_path, result_cache::WorkspaceResults, stateless_queries,
        test_fixture::Fixture,
    },
};
use serde_json::Value;
use std::fs;
use std::time::Duration;
use tp_core::{identity::ContentRevision, query_deadline::QueryDeadline};
use tp_semantic::SemanticSession;

pub(super) struct PreparedWorkspace {
    config: WorkspaceConfig,
    cache: std::path::PathBuf,
    results: WorkspaceResults,
    options: cli::Arguments,
    context: tp_core::request_context::RequestContext,
}

impl PreparedWorkspace {
    pub(super) fn new(fixture: &Fixture, words: &[&str]) -> Self {
        let mut arguments = vec![
            "--workspace".to_owned(),
            fixture.config.display().to_string(),
            "--root".to_owned(),
            fixture.root.path().join("engine").display().to_string(),
            "--cache".to_owned(),
            fixture.cache.path().display().to_string(),
            "--no-daemon".to_owned(),
            "--diagnostics".to_owned(),
            "off".to_owned(),
            "--budget".to_owned(),
            "6000".to_owned(),
            "--limit".to_owned(),
            "20".to_owned(),
        ];
        arguments.extend(words.iter().map(|word| (*word).to_owned()));
        let options = cli::parse(&arguments).unwrap();
        let context = cli::request_context(&options).unwrap();
        let config = WorkspaceConfig::load(&fixture.config).unwrap();
        let cache = cache_path(&config, Some(fixture.cache.path())).unwrap();
        let results = WorkspaceResults::open(&cache, &QueryDeadline::start()).unwrap();
        Self {
            config,
            cache,
            results,
            options,
            context,
        }
    }

    pub(super) fn request(&self) -> WorkspaceQuery<'_> {
        WorkspaceQuery {
            config: &self.config,
            cache: &self.cache,
            results: &self.results,
            options: &self.options,
            context: &self.context,
            deadline: QueryDeadline::start(),
            policy: HomeIndexPolicy::AwaitDaemon(Duration::ZERO),
        }
    }
}

pub(super) fn indexed_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.json("engine", &["search", "sym:SharedThing", "ws:all"]);
    fixture
}

pub(super) fn append_current_references(fixture: &Fixture) {
    for member in ["engine", "pack", "upstream"] {
        let source = fixture.root.path().join(member).join("lib.rs");
        let mut bytes = fs::read(&source).unwrap();
        bytes.extend_from_slice(b"\npub fn use_shared(_: &SharedThing) {}\n");
        fs::write(source, bytes).unwrap();
    }
}

pub(super) fn replaced_member() -> SearchedMember {
    SearchedMember {
        answer: Err(anyhow::anyhow!(coded!(
            RouterBinaryReplaced,
            "workspace member owner was replaced"
        ))),
        progress_source: None,
    }
}

pub(super) fn direct_source_fallback(
    request: &WorkspaceQuery<'_>,
    member: &crate::workspace::member_root::MemberRoot,
) -> anyhow::Result<stateless_queries::RouterReplacementAnswer> {
    stateless_queries::run_after_router_replacement(
        request.config,
        request.options,
        &request.deadline,
        member,
    )
}

pub(super) fn member_rows(page: &Value) -> Vec<String> {
    page["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["member"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn implicit_home_replacement_uses_complete_current_source_with_read_handles() {
    let fixture = indexed_fixture();
    let current_source = b"pub struct SharedThing { pub current_only_marker: u8 }\n";
    let source_path = fixture.root.path().join("engine/lib.rs");
    fs::write(&source_path, current_source).unwrap();
    let prepared = PreparedWorkspace::new(&fixture, &["search", "sym:SharedThing"]);
    let request = prepared.request();
    let mut session = SemanticSession::default();
    let output = search_with_policy_using(
        &request,
        &mut session,
        |_request, _command: &MemberCommand<'_>, member, _first, _map_miss| {
            assert_eq!(member.name(), "engine");
            Ok(replaced_member())
        },
        direct_source_fallback,
    )
    .unwrap();
    let page: Value = serde_json::from_str(&output).unwrap();

    assert_eq!(page["source"], "current_source");
    assert_eq!(page["router_fallback"]["cause"], "router_binary_replaced");
    assert_eq!(page["router_fallback"]["member"], "engine");
    assert_eq!(page["router_fallback"]["coverage"], "complete");
    assert_eq!(member_rows(&page), ["engine"]);
    let hit = &page["hits"][0];
    let handle = hit["handle"].as_str().unwrap();
    assert!(handle.starts_with("read:path:"), "{handle}");
    let shown: Value =
        serde_json::from_str(&fixture.run("engine", &["show", handle]).unwrap()).unwrap();
    assert_eq!(shown["verified"], false, "{shown}");
    assert_eq!(shown["source"], "current_file", "{shown}");
    assert_eq!(
        shown["revision"],
        ContentRevision::of(current_source).to_string(),
        "{shown}"
    );
    assert!(shown.to_string().contains("current_only_marker"), "{shown}");
    assert!(!shown.to_string().contains("engine_marker"), "{shown}");

    fs::write(
        &source_path,
        "pub struct SharedThing { pub changed_again: u8 }\n",
    )
    .unwrap();
    let stale = fixture
        .run("engine", &["show", handle])
        .unwrap_err()
        .to_string();
    assert!(stale.contains("current path changed"), "{stale}");
}

#[test]
fn incomplete_router_retry_preserves_healthy_indexed_siblings() {
    let fixture = indexed_fixture();
    let prepared = PreparedWorkspace::new(&fixture, &["search", "sym:SharedThing", "ws:all"]);
    let request = prepared.request();
    let mut session = SemanticSession::default();
    let output = search_with_policy_using(
        &request,
        &mut session,
        |request, command: &MemberCommand<'_>, member, first, map_miss| {
            if member.name() == "pack" {
                Ok(replaced_member())
            } else {
                search_member(request, command, member, first, map_miss)
            }
        },
        |_request, _member| {
            Ok(stateless_queries::RouterReplacementAnswer {
                output: String::new(),
                complete: false,
                incomplete_reason: Some("timed_out: current-source scan expired".to_owned()),
            })
        },
    )
    .unwrap();
    let page: Value = serde_json::from_str(&output).unwrap();

    let members: Vec<_> = page["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["member"].as_str().unwrap())
        .collect();
    assert!(members.contains(&"engine"), "{page}");
    assert!(members.contains(&"upstream"), "{page}");
    assert!(!members.contains(&"pack"), "{page}");
    let pack = page["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["member"] == "pack")
        .unwrap();
    assert_eq!(pack["state"], "unavailable");
    assert!(
        pack["reason"]
            .as_str()
            .unwrap()
            .contains("router_binary_replaced")
    );
    assert_eq!(pack["issues"]["router_fallback"]["status"], "incomplete");
    assert!(
        pack["issues"]["router_fallback"]["reason"]
            .as_str()
            .unwrap()
            .contains("timed_out")
    );
}

#[test]
fn ordinary_member_unavailable_remains_partial_coverage() {
    let fixture = indexed_fixture();
    let prepared = PreparedWorkspace::new(&fixture, &["search", "sym:SharedThing", "ws:all"]);
    let request = prepared.request();
    let mut session = SemanticSession::default();
    let output = search_with_policy_using(
        &request,
        &mut session,
        |request, command: &MemberCommand<'_>, member, first, map_miss| {
            if member.name() == "pack" {
                Ok(SearchedMember {
                    answer: Err(anyhow::anyhow!(coded!(
                        MemberUnavailable,
                        "member index is unavailable"
                    ))),
                    progress_source: None,
                })
            } else {
                search_member(request, command, member, first, map_miss)
            }
        },
        |_request, _member| panic!("ordinary member unavailability is not a router refusal"),
    )
    .unwrap();
    let page: Value = serde_json::from_str(&output).unwrap();

    let pack = page["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["member"] == "pack")
        .unwrap();
    assert_eq!(pack["state"], "unavailable");
    assert!(pack.get("issues").is_none());
    assert!(
        page["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["member"] == "engine")
    );
    assert!(
        page["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["member"] == "upstream")
    );
}

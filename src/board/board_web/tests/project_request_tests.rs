use super::*;
use crate::board::{board_ids::PlanId, repo_identity::tests::GitFixture};
use crate::workspace::config::WorkspaceConfig;
use serde_json::{Value, json};
use std::fs;

struct WebProjectFixture {
    store: WebStore,
    config: BoardConfig,
    directory: tempfile::TempDir,
    _repos: [GitFixture; 3],
    ids: [String; 3],
    plans: [PlanId; 4],
}

impl WebProjectFixture {
    fn new() -> Self {
        let directory = crate::board::board_test_support::scratch("web-project-request-");
        let repos = std::array::from_fn(|index| {
            let git = GitFixture::new();
            git.commit(&format!("web project {index} root"));
            git
        });
        let paths: Vec<_> = repos
            .iter()
            .enumerate()
            .map(|(index, git)| {
                let path = directory.path().join(format!("project-{index}.toml"));
                fs::write(
                    &path,
                    format!(
                        "[workspace]\nname = \"Project{index}\"\n[members.repo]\npath = {}\n",
                        serde_json::to_string(&git.root).unwrap()
                    ),
                )
                .unwrap();
                path
            })
            .collect();
        let ids = std::array::from_fn(|index| WorkspaceConfig::load(&paths[index]).unwrap().id);
        let registry = directory.path().join("workspaces.toml");
        fs::write(
            &registry,
            format!("workspaces = {}\n", serde_json::to_string(&paths).unwrap()),
        )
        .unwrap();
        let config = BoardConfig::for_database(directory.path().join("board.sqlite3"));
        let store = WebStore::open_at(
            BoardConfigCache::with_config(config.clone()),
            directory.path().join("runtime"),
        )
        .unwrap();
        *store.projects.lock().unwrap() = ProjectResolver::with_registry(registry);
        let plans = std::array::from_fn(|index| {
            let request: WebRequest = serde_json::from_value(json!({
                "api":BOARD_API, "op":{"op":"new", "title":format!("Plan {index}"),
                "body":"", "steward":null, "repo_key":null}
            }))
            .unwrap();
            let reply =
                web_ops::execute(&store, request, Instant::now() + Duration::from_secs(5)).unwrap();
            let BoardResult::Change(change) = reply.result else {
                panic!("plan creation");
            };
            change.plan.unwrap()
        });
        let conn = rusqlite::Connection::open(&config.db_path).unwrap();
        for (plan, git) in plans.iter().zip(&repos) {
            let key = git.registration().repo_key;
            conn.execute("INSERT INTO repos(repo_key) VALUES(?1)", [key.as_str()])
                .unwrap();
            conn.execute(
                "INSERT INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
                rusqlite::params![plan.get() as i64, key.as_str()],
            )
            .unwrap();
        }
        Self {
            store,
            config,
            directory,
            _repos: repos,
            ids,
            plans,
        }
    }

    fn execute(&self, project: Option<&str>, op: Value) -> Result<BoardReply, BoardError> {
        let request =
            serde_json::from_value(json!({"api":BOARD_API, "project":project, "op":op})).unwrap();
        web_ops::execute(
            &self.store,
            request,
            Instant::now() + Duration::from_secs(5),
        )
    }
}

#[test]
fn web_project_id_and_name_resolve_before_local_dispatch() {
    let fixture = WebProjectFixture::new();
    for (selector, expected) in [
        (&fixture.ids[1][..], fixture.plans[1]),
        ("Project0", fixture.plans[0]),
    ] {
        let reply = fixture
            .execute(
                Some(selector),
                json!({
                    "op":"overview", "scope":"all", "after":null, "through":null, "limit":100
                }),
            )
            .unwrap();
        let BoardResult::Overview(overview) = reply.result else {
            panic!("overview result");
        };
        assert_eq!(
            overview
                .plans
                .iter()
                .map(|row| row.plan.id)
                .collect::<Vec<_>>(),
            vec![expected]
        );
        assert!(matches!(overview.scope, ReadScope::Keys(_)));
    }
    let reply = fixture.execute(None, json!({"op":"projects"})).unwrap();
    let wire = serde_json::to_value(&reply).unwrap();
    assert_eq!(wire["api"], BOARD_API);
    let BoardResult::Projects(projects) = reply.result else {
        panic!("projects result");
    };
    assert_eq!(projects.len(), 3);
    assert!(projects.iter().all(|row| row.plan_count == 1));
    let reply = fixture
        .execute(
            Some(&fixture.ids[0]),
            json!({
                "op":"feed", "plan":fixture.plans[2], "scope":"all", "after":null,
                "through":null, "limit":100
            }),
        )
        .unwrap();
    let BoardResult::Feed(feed) = reply.result else {
        panic!("feed result");
    };
    assert!(feed.events.is_empty());
    let reply = fixture
        .execute(
            None,
            json!({
                "op":"overview", "scope":"unscoped", "after":null, "through":null, "limit":100
            }),
        )
        .unwrap();
    let BoardResult::Overview(overview) = reply.result else {
        panic!("overview result");
    };
    assert_eq!(overview.plans[0].plan.id, fixture.plans[3]);
    assert_eq!(overview.plans.len(), 1);
}

#[test]
fn web_project_rejects_unsupported_and_conflicting_scopes_before_registry_read() {
    let fixture = WebProjectFixture::new();
    let corrupt = fixture.directory.path().join("corrupt-workspaces.toml");
    fs::write(&corrupt, "workspaces = [").unwrap();
    *fixture.store.projects.lock().unwrap() = ProjectResolver::with_registry(corrupt);
    for scope in [
        json!("unscoped"),
        json!({"keys":[]}),
        json!({"repo":fixture._repos[0].registration().repo_key}),
    ] {
        let error = fixture
            .execute(
                Some(&fixture.ids[0]),
                json!({
                    "op":"overview", "scope":scope, "after":null, "through":null, "limit":100
                }),
            )
            .unwrap_err();
        assert_eq!(error.code, BoardErrorCode::InvalidOptions);
        assert!(error.message.starts_with("project cannot be combined"));
    }
    let error = fixture
        .execute(Some(&fixture.ids[0]), json!({"op":"projects"}))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(error.message, "project requires a scoped read");
    *fixture.store.projects.lock().unwrap() =
        ProjectResolver::with_registry(fixture.directory.path().join("workspaces.toml"));
    let before = fs::read(&fixture.config.db_path).unwrap();
    let error = fixture
        .execute(
            Some("Unknown"),
            json!({
                "op":"overview", "scope":"all", "after":null, "through":null, "limit":100
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(error.message, "unknown project Unknown");
    assert_eq!(fs::read(&fixture.config.db_path).unwrap(), before);
}

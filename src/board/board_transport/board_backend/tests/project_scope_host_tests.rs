use super::*;
use crate::board::{
    board_projects::ProjectResolver, board_protocol::ReadScope, repo_identity::tests::GitFixture,
};
use serde_json::json;
use std::fs;
mod host_project_fixture;
use host_project_fixture::HostProjectFixture;

#[test]
fn projects_counts_shared_plans_once_without_mutating_storage() {
    let first = GitFixture::new();
    first.commit("project count first root");
    let second = GitFixture::new();
    second.commit("project count second root");
    let mut fixture = HostProjectFixture::new();
    fixture.workspace("one", "One", &first.root);
    let two = fixture.workspace("two", "Two", &second.root);
    fs::write(
        &two.path,
        format!(
            "[workspace]\nname = \"Two\"\n[members.a]\npath = {}\n[members.b]\npath = {}\n",
            serde_json::to_string(&first.root).unwrap(),
            serde_json::to_string(&second.root).unwrap()
        ),
    )
    .unwrap();
    let shared = fixture.plan("Shared");
    let other = fixture.plan("Second only");
    fixture.plan("Unlinked");
    let conn = rusqlite::Connection::open(&fixture.config.db_path).unwrap();
    for (plan, key) in [
        (shared, first.registration().repo_key),
        (shared, second.registration().repo_key),
        (other, second.registration().repo_key),
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO repos(repo_key) VALUES(?1)",
            [key.as_str()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
            rusqlite::params![plan.get() as i64, key.as_str()],
        )
        .unwrap();
    }
    let counts = || {
        ["actors", "events", "repos", "repo_paths", "plan_repos"].map(|table| {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        })
    };
    let before = counts();
    let reply = fixture.wire_projects();
    let snapshot = fixture.host.max_seq_by(QueryDeadline::start()).unwrap();
    assert_eq!(reply.snapshot_seq, Some(snapshot));
    let wire = serde_json::to_value(&reply).unwrap();
    assert_eq!(wire["result"]["result"], "projects");
    let BoardResult::Projects(projects) = reply.result else {
        panic!("projects result");
    };
    assert_eq!(
        projects
            .iter()
            .find(|row| row.name == "One")
            .unwrap()
            .plan_count,
        1
    );
    assert_eq!(
        projects
            .iter()
            .find(|row| row.name == "Two")
            .unwrap()
            .plan_count,
        2
    );
    assert_eq!(counts(), before);
}

#[test]
fn projects_absent_database_returns_zero_without_initializing_writer() {
    let git = GitFixture::new();
    git.commit("project with absent database");
    let mut fixture = HostProjectFixture::new();
    fixture.workspace("fresh", "Fresh", &git.root);
    let reply = fixture.wire_projects();
    assert_eq!(reply.snapshot_seq, Some(EventSeq::new(0)));
    let BoardResult::Projects(projects) = reply.result else {
        panic!("projects result");
    };
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].plan_count, 0);
    assert!(!fixture.config.db_path.parent().unwrap().exists());
    assert!(fixture.host.inner.backend.lock().unwrap().is_none());
}

#[test]
fn projects_wire_encodes_unavailable_non_utf8_member_path() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = HostProjectFixture::new();
    let base = fixture
        .directory
        .path()
        .join(std::ffi::OsString::from_vec(b"workspace-\xff".to_vec()));
    fs::create_dir(&base).unwrap();
    fs::write(
        base.join("trufflepig.workspace.toml"),
        "[workspace]\nname = \"Unavailable\"\n[members.missing]\npath = \"missing\"\n",
    )
    .unwrap();
    let registry = base.join("workspaces.toml");
    fs::write(&registry, "workspaces = [\"trufflepig.workspace.toml\"]\n").unwrap();
    *fixture.host.inner.projects.lock().unwrap() = ProjectResolver::with_registry(registry);
    let reply = fixture.wire_projects();
    let wire =
        serde_json::to_value(&reply).expect("Projects JSON preserves unavailable Unix paths");
    let expected = crate::store::encode_path(&base.join("missing"));
    assert_eq!(
        wire["result"]["data"][0]["unavailable"][0]["root"],
        expected
    );
    assert!(expected.contains("%FF"));
    let lines = crate::board::board_render::render_reply(
        &reply,
        &crate::output::OutputBudget::new(4000)
            .unwrap()
            .with_format(crate::output::OutputFormat::Lines),
    )
    .unwrap()
    .text;
    assert!(lines.contains(&expected));
    assert!(!fixture.config.db_path.exists());
}

#[test]
fn host_project_selection_uses_id_first_and_rejects_sorted_name_ambiguity() {
    let git = GitFixture::new();
    git.commit("project selection root");
    let mut fixture = HostProjectFixture::new();
    let first = fixture.workspace("first", "Shared", &git.root);
    let second = fixture.workspace("second", "Shared", &git.root);
    let shadow = GitFixture::new();
    shadow.commit("project raw name shadows an ID");
    fixture.workspace("shadow", &first.id, &shadow.root);
    let mut op = BoardOp::Overview {
        scope: ReadScope::All,
        after: None,
        through: None,
        limit: 10,
    };
    let error = fixture
        .host
        .select_project_scope(&mut op, "Shared", "fixture-host", QueryDeadline::start())
        .unwrap_err();
    let mut ids = [first.id.clone(), second.id];
    ids.sort();
    assert_eq!(
        error.to_string(),
        format!(
            "invalid_options: project Shared matches {}, {}",
            ids[0], ids[1]
        )
    );
    fixture
        .host
        .select_project_scope(&mut op, &first.id, "fixture-host", QueryDeadline::start())
        .unwrap();
    assert!(
        matches!(op, BoardOp::Overview { scope: ReadScope::Keys(ref keys), .. }
        if *keys == BTreeSet::from([git.registration().repo_key]))
    );
    let mut explicit = BoardOp::Overview {
        scope: ReadScope::Unscoped,
        after: None,
        through: None,
        limit: 10,
    };
    let error = fixture
        .host
        .select_project_scope(
            &mut explicit,
            &first.id,
            "fixture-host",
            QueryDeadline::start(),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("invalid_options: project cannot be combined")
    );
    let mut unsupported = BoardOp::Projects;
    let error = fixture
        .host
        .select_project_scope(
            &mut unsupported,
            &first.id,
            "fixture-host",
            QueryDeadline::start(),
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid_options: project requires a scoped read"
    );
    assert!(!fixture.config.db_path.exists());
}

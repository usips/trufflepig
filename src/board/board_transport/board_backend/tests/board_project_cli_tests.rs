use super::*;
use crate::board::repo_identity::tests::GitFixture;
use crate::workspace::config::WorkspaceConfig;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

mod project_cli_read_tests;

const FIXTURE_ROOT: &str = "TRUFFLEPIG_BOARD_PROJECT_CLI_TEST_ROOT";

struct ProjectCliFixture {
    repos: [GitFixture; 3],
    root: PathBuf,
    host: BoardHost,
    project_ids: [String; 2],
}

impl ProjectCliFixture {
    fn new(root: &Path, names: [&str; 2]) -> Self {
        let repos = std::array::from_fn(|index| {
            let repo = GitFixture::new();
            repo.commit(&format!("project {index}"));
            repo.git(&[
                "config",
                "remote.origin.url",
                &format!("https://example.test/project-{index}"),
            ]);
            repo
        });
        let member_b = repos[1].root.join("src");
        std::fs::create_dir(&member_b).unwrap();
        let paths = std::array::from_fn::<_, 2, _>(|index| {
            let directory = root.join(format!("workspace-{index}"));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("trufflepig.workspace.toml");
            let members = if index == 0 {
                format!(
                    "[members.a]\npath = {:?}\n[members.b]\npath = {:?}\n",
                    repos[0].root, member_b
                )
            } else {
                format!("[members.c]\npath = {:?}\n", repos[2].root)
            };
            std::fs::write(
                &path,
                format!("[workspace]\nname = {:?}\n{members}", names[index]),
            )
            .unwrap();
            path
        });
        let registry_directory = root.join("config/trufflepig");
        std::fs::create_dir_all(&registry_directory).unwrap();
        std::fs::write(
            registry_directory.join("workspaces.toml"),
            format!("workspaces = [{:?}, {:?}]\n", paths[0], paths[1]),
        )
        .unwrap();
        let project_ids = paths.map(|path| WorkspaceConfig::load(&path).unwrap().id);
        let root = root.to_path_buf();
        let host = BoardHost::with_config(BoardConfig::for_database(root.join("board.sqlite3")));
        Self {
            repos,
            root,
            host,
            project_ids,
        }
    }

    fn run(&self, root: &Path, words: &[&str]) -> anyhow::Result<String> {
        self.run_as(root, words, "codex")
    }

    fn run_as(&self, root: &Path, words: &[&str], client: &str) -> anyhow::Result<String> {
        let mut args = vec![
            "--root".to_owned(),
            root.to_string_lossy().into_owned(),
            "--json".to_owned(),
            "-b10000".to_owned(),
        ];
        args.extend(words.iter().map(|word| (*word).to_owned()));
        self.host.run(
            &crate::cli::parse(&args)?,
            &RequestContext::new(Some("project-cli-reader".into()), Some(client.into())),
            QueryDeadline::start(),
        )
    }

    fn seed(&self) {
        for (index, repo) in self.repos.iter().enumerate() {
            self.run(&repo.root, &["board", "new", &format!("Repo {index}")])
                .unwrap();
        }
        self.run(&self.root, &["board", "new", "Unscoped"]).unwrap();
    }

    fn seed_questions(&self) {
        self.seed();
        for index in 0..4 {
            let root = self
                .repos
                .get(index)
                .map_or(self.root.as_path(), |repo| repo.root.as_path());
            self.run_as(
                root,
                &[
                    "board",
                    "post",
                    &format!("P{}", index + 1),
                    "question",
                    &format!("Question {index}"),
                    "--to",
                    "codex",
                ],
                "claude",
            )
            .unwrap();
        }
    }
}

fn isolated(test: &str, scenario: impl FnOnce(&Path)) {
    if let Some(root) = std::env::var_os(FIXTURE_ROOT) {
        scenario(Path::new(&root));
        return;
    }
    let directory = crate::board::board_test_support::scratch("board-cli-project-");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([test, "--nocapture"])
        .env(FIXTURE_ROOT, directory.path())
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .env("XDG_DATA_HOME", directory.path().join("data"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated {test}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn overview_plan_ids(output: &str) -> Vec<String> {
    serde_json::from_str::<Value>(output).unwrap()["result"]["data"]["plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|plan| plan["plan"]["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn show_project_lists_only_member_repo_plans() {
    isolated("show_project_lists_only_member_repo_plans", |root| {
        let fixture = ProjectCliFixture::new(root, ["W1", "W2"]);
        fixture.seed();
        let output = fixture
            .run(
                &fixture.repos[2].root,
                &["board", "show", "--project", "W1"],
            )
            .expect("board show --project must accept a registered workspace name");
        assert_eq!(overview_plan_ids(&output), ["P1", "P2"]);
    });
}

#[test]
fn ambiguous_project_name_is_refused() {
    isolated("ambiguous_project_name_is_refused", |root| {
        let fixture = ProjectCliFixture::new(root, ["Shared", "Shared"]);
        fixture.seed();
        let error = fixture
            .run(
                &fixture.repos[0].root,
                &["board", "show", "--project", "Shared"],
            )
            .unwrap_err();
        let mut ids = fixture.project_ids.clone();
        ids.sort();
        assert_eq!(
            error.to_string(),
            format!(
                "invalid_options: project Shared matches {}, {}",
                ids[0], ids[1]
            )
        );
    });
}

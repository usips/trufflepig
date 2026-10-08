//! Explicit-plan Done validates project membership before the unchanged Tasks read.
use super::*;
use crate::board::{
    board_projects::project_read_helpers::select_project, board_protocol::TaskOrder,
    board_vocabulary::TaskColumn, local_board::LocalBoard,
};

impl BoardHost {
    pub(super) fn validate_done_project(
        &self,
        op: &BoardOp,
        selector: &str,
        host: &str,
        deadline: QueryDeadline,
    ) -> Result<bool> {
        let BoardOp::Tasks {
            plan,
            column: Some(TaskColumn::Done),
            order: TaskOrder::RecentFirst,
            ..
        } = op
        else {
            return Ok(false);
        };
        let project_keys = if selector == "unscoped" {
            None
        } else {
            let projects = self.projects(host, deadline)?;
            let project = select_project(selector, &projects)?;
            if project.project.repo_keys.is_empty() {
                bail!("invalid_options: project {selector:?} has no usable repository identities");
            }
            Some(project.project.repo_keys.clone())
        };
        check_deadline(deadline)?;
        let config = self.config()?;
        let reader =
            LocalBoard::open_read_with_timeout(&config, deadline.cap(Duration::from_millis(100)))?;
        let keys = reader.plan_repo_keys(*plan)?;
        let matches = project_keys.as_ref().map_or_else(
            || keys.is_empty(),
            |selected| keys.iter().any(|key| selected.contains(key)),
        );
        check_deadline(deadline)?;
        if !matches {
            bail!("invalid_options: plan {plan} is outside project {selector:?}");
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{
        board_backend::BoardBackend,
        board_config::BoardConfig,
        board_ids::RepoKey,
        board_projects::ProjectResolver,
        board_vocabulary::{PlanText, PlanTitle},
        repo_identity::tests::GitFixture,
    };

    struct DoneFixture {
        git: GitFixture,
        host: BoardHost,
        conn: rusqlite::Connection,
        project: String,
    }

    impl DoneFixture {
        fn new() -> Self {
            let git = GitFixture::new();
            git.commit("Done project root");
            let config = BoardConfig::for_database(git.directory.path().join("board.sqlite3"));
            let actor = config.actor(None, Some("done-project-fixture")).unwrap();
            let mut board = LocalBoard::open(&config).unwrap();
            for title in ["Selected", "Outside", "Unscoped"] {
                board
                    .handle(&BoardRequest::new(
                        actor.clone(),
                        BoardOp::New {
                            title: PlanTitle::new(title).unwrap(),
                            body: PlanText::new("").unwrap(),
                            steward: None,
                            repo_key: None,
                        },
                    ))
                    .unwrap();
            }
            drop(board);
            let workspace = git.directory.path().join("trufflepig.workspace.toml");
            std::fs::write(
                &workspace,
                format!(
                    "[workspace]\nname = \"done_fleet\"\n[members.repo]\npath = {}\n",
                    serde_json::to_string(&git.root).unwrap(),
                ),
            )
            .unwrap();
            let registry = git.directory.path().join("workspaces.toml");
            std::fs::write(
                &registry,
                format!(
                    "workspaces = {}\n",
                    serde_json::to_string(&[workspace]).unwrap()
                ),
            )
            .unwrap();
            let host = BoardHost::with_config(config.clone());
            *host.inner.projects.lock().unwrap() = ProjectResolver::with_registry(registry);
            let projects = host.projects(&actor.host, QueryDeadline::start()).unwrap();
            let project = &projects[0].project;
            let selected = project.repo_keys.iter().next().unwrap();
            let outside = RepoKey::parse(&"f".repeat(40)).unwrap();
            assert_ne!(selected, &outside);
            let conn = rusqlite::Connection::open(&config.db_path).unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO repos(repo_key) VALUES(?1),(?2)",
                rusqlite::params![selected.as_str(), outside.as_str()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO plan_repos VALUES(1,?1),(2,?2)",
                rusqlite::params![selected.as_str(), outside.as_str()],
            )
            .unwrap();
            conn.execute_batch(
                "INSERT INTO tasks VALUES(1,1,'Selected first','done',NULL,NULL,3),
                 (1,2,'Selected second','done',NULL,NULL,3),
                 (2,1,'Outside completed','done',NULL,NULL,3),
                 (3,1,'Unscoped completed','done',NULL,NULL,3);",
            )
            .unwrap();
            Self {
                git,
                host,
                conn,
                project: project.id.clone(),
            }
        }

        fn run(&self, words: &[&str]) -> Result<String> {
            self.run_at(&self.git.root, words)
        }

        fn run_at(&self, root: &std::path::Path, words: &[&str]) -> Result<String> {
            let mut args = vec!["--root".to_owned(), root.to_string_lossy().into_owned()];
            args.extend(words.iter().map(|word| (*word).to_owned()));
            self.host.run(
                &crate::cli::parse(&args).unwrap(),
                &RequestContext::new(None, None),
                QueryDeadline::after(Duration::from_secs(2)),
            )
        }

        fn counts(&self) -> [i64; 6] {
            [
                "actors",
                "agent_sessions",
                "events",
                "repos",
                "repo_paths",
                "plan_repos",
            ]
            .map(|table| {
                self.conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap()
            })
        }
    }

    #[test]
    fn done_plan_project_membership_is_checked_without_a_writer() {
        let fixture = DoneFixture::new();
        let held = fixture.host.inner.backend.lock().unwrap();
        let before = fixture.counts();
        let selected = fixture
            .run(&["board", "done", "P1", "--project", "done_fleet", "-n1"])
            .unwrap();
        assert!(selected.contains("Selected second"));
        let outside = fixture
            .run(&["board", "done", "P2", "--project", &fixture.project])
            .unwrap_err()
            .to_string();
        assert!(outside.starts_with("invalid_options:"));
        assert!(outside.contains("P2") && outside.contains(&fixture.project));
        assert!(
            fixture
                .run(&["board", "done", "P3", "--project", "unscoped"])
                .unwrap()
                .contains("Unscoped completed")
        );
        let scoped = fixture
            .run(&["board", "done", "P1", "--project", "unscoped"])
            .unwrap_err()
            .to_string();
        assert!(
            scoped.starts_with("invalid_options:")
                && scoped.contains("P1")
                && scoped.contains("unscoped")
        );
        assert_eq!(fixture.counts(), before);
        assert!(held.is_none());
    }

    #[test]
    fn done_board_cli_uses_root_repo_all_project_and_unscoped() {
        let fixture = DoneFixture::new();
        let before = fixture.counts();
        let root = fixture.run(&["board", "done"]).unwrap();
        assert!(root.contains("Selected second") && root.contains("Unscoped completed"));
        assert!(!root.contains("Outside completed"));
        let unidentified = fixture
            .run_at(fixture.git.directory.path(), &["board", "done"])
            .unwrap();
        assert!(
            unidentified.contains("Selected second")
                && unidentified.contains("Outside completed")
                && unidentified.contains("Unscoped completed")
        );
        let all = fixture.run(&["board", "done", "--all"]).unwrap();
        assert!(
            all.contains("Selected second")
                && all.contains("Outside completed")
                && all.contains("Unscoped completed")
        );
        let project = fixture
            .run(&["board", "done", "--project", &fixture.project])
            .unwrap();
        assert!(project.contains("Selected second"));
        assert!(!project.contains("Outside completed") && !project.contains("Unscoped completed"));
        let unscoped = fixture
            .run(&["board", "done", "--project", "unscoped"])
            .unwrap();
        assert!(unscoped.contains("Unscoped completed") && !unscoped.contains("Selected second"));
        assert_eq!(fixture.counts(), before);
        assert!(fixture.host.inner.backend.lock().unwrap().is_none());
    }
}

use super::*;

pub(super) struct ProjectFixture {
    pub directory: tempfile::TempDir,
    pub registry: PathBuf,
    pub config: BoardConfig,
    pub resolver: ProjectResolver,
    paths: Vec<PathBuf>,
}

impl ProjectFixture {
    pub fn new() -> Self {
        let directory = crate::board::board_test_support::scratch("board-projects-");
        let registry = directory.path().join("workspaces.toml");
        let config = BoardConfig::for_database(directory.path().join("board.sqlite3"));
        Self {
            directory,
            registry: registry.clone(),
            config,
            resolver: ProjectResolver::with_registry(registry),
            paths: Vec::new(),
        }
    }

    pub fn workspace(
        &mut self,
        directory: &str,
        name: &str,
        members: &[(&str, &Path)],
    ) -> WorkspaceConfig {
        let path = self
            .directory
            .path()
            .join(directory)
            .join("trufflepig.workspace.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = format!(
            "[workspace]\nname = {}\n",
            serde_json::to_string(name).unwrap()
        );
        for (name, root) in members {
            body.push_str(&format!(
                "[members.{name}]\npath = {}\n",
                serde_json::to_string(root).unwrap()
            ));
        }
        fs::write(&path, body).unwrap();
        self.paths.push(path.clone());
        self.write_registry();
        WorkspaceConfig::load(&path).unwrap()
    }

    pub fn write_registry(&self) {
        fs::write(
            &self.registry,
            format!(
                "workspaces = {}\n",
                serde_json::to_string(&self.paths).unwrap()
            ),
        )
        .unwrap();
    }

    pub fn stored_key(&self, root: &Path, host: &str, key: &RepoKey) {
        let board = LocalBoard::open(&self.config).unwrap();
        let conn = rusqlite::Connection::open(board.path()).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO repos(repo_key) VALUES(?1)",
            [key.as_str()],
        )
        .unwrap();
        let common = crate::history::git::common_dir(root).unwrap();
        conn.execute(
            "INSERT INTO repo_paths(repo_key,host,common_dir,root_commits_json) VALUES(?1,?2,?3,'[]')",
            rusqlite::params![key.as_str(), host, common.to_string_lossy().as_ref()],
        ).unwrap();
    }

    pub fn board_counts(&self) -> [i64; 5] {
        let conn = rusqlite::Connection::open(&self.config.db_path).unwrap();
        ["actors", "events", "repos", "repo_paths", "plan_repos"].map(|table| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        })
    }

    pub fn resolve(&mut self, host: &str) -> Vec<ResolvedProject> {
        self.resolver
            .resolve(&self.config, host, QueryDeadline::start())
            .unwrap()
    }
}

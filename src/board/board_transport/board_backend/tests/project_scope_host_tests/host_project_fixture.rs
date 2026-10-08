use super::*;
use crate::board::{
    board_ids::PlanId,
    board_protocol::BOARD_API,
    board_vocabulary::{PlanText, PlanTitle},
};
use crate::workspace::config::WorkspaceConfig;
use std::path::Path;

pub(super) struct HostProjectFixture {
    pub(super) host: BoardHost,
    pub(super) config: BoardConfig,
    pub(super) directory: tempfile::TempDir,
    registry: std::path::PathBuf,
    workspaces: Vec<std::path::PathBuf>,
}

impl HostProjectFixture {
    pub(super) fn new() -> Self {
        let directory = crate::board::board_test_support::scratch("project-host-");
        let config = BoardConfig::for_database(directory.path().join("storage/board.sqlite3"));
        let registry = directory.path().join("workspaces.toml");
        let host = BoardHost::with_config(config.clone());
        *host.inner.projects.lock().unwrap() = ProjectResolver::with_registry(&registry);
        Self {
            host,
            config,
            directory,
            registry,
            workspaces: Vec::new(),
        }
    }

    pub(super) fn workspace(&mut self, slug: &str, name: &str, root: &Path) -> WorkspaceConfig {
        let path = self
            .directory
            .path()
            .join(slug)
            .join("trufflepig.workspace.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "[workspace]\nname = {}\n[members.repo]\npath = {}\n",
                serde_json::to_string(name).unwrap(),
                serde_json::to_string(root).unwrap()
            ),
        )
        .unwrap();
        self.workspaces.push(path.clone());
        fs::write(
            &self.registry,
            format!(
                "workspaces = {}\n",
                serde_json::to_string(&self.workspaces).unwrap()
            ),
        )
        .unwrap();
        WorkspaceConfig::load(&path).unwrap()
    }

    pub(super) fn wire_projects(&self) -> BoardReply {
        let actor = self
            .config
            .actor(Some("human"), Some("project-reader"))
            .unwrap();
        let request: BoardRequest = serde_json::from_value(json!({
            "api":BOARD_API, "actor":actor, "op":{"op":"projects"}
        }))
        .unwrap();
        self.host
            .handle_by(&request, QueryDeadline::start())
            .unwrap()
    }

    pub(super) fn plan(&self, title: &str) -> PlanId {
        let actor = self
            .config
            .actor(Some("human"), Some("project-owner"))
            .unwrap();
        let reply = self
            .host
            .handle_by(
                &BoardRequest::new(
                    actor,
                    BoardOp::New {
                        title: PlanTitle::new(title).unwrap(),
                        body: PlanText::new("").unwrap(),
                        steward: None,
                        repo_key: None,
                    },
                ),
                QueryDeadline::start(),
            )
            .unwrap();
        let BoardResult::Change(change) = reply.result else {
            panic!("plan creation");
        };
        change.plan.unwrap()
    }
}

//! Serving-host projects derived from registered workspaces and read-only Git identity.
//! Resolution never creates board storage or persists project information.

pub(crate) mod project_read_helpers;
#[cfg(test)]
mod tests;

pub(crate) use project_read_helpers::{project_reply, select_scope, validate_scope_selection};

use super::{
    board_config::BoardConfig, board_ids::RepoKey, board_protocol::BoardError,
    local_board::LocalBoard, repo_identity::RepoIdentityCache,
};
use crate::{
    daemon::deadline::QueryDeadline,
    history::git::common_dir_bounded,
    workspace::config::{
        RegisteredWorkspace, RegisteredWorkspaceMember, registered_workspaces_from,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

const PROJECT_CACHE_TTL: Duration = Duration::from_secs(60);
const PROJECT_GIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub repo_keys: BTreeSet<RepoKey>,
    pub unavailable: Vec<RegisteredWorkspaceMember>,
}

/// Raw names remain available for selector ambiguity while display names are unique.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProject {
    pub project: Project,
    pub raw_name: String,
}

#[derive(Default)]
pub struct ProjectResolver {
    registry: Option<PathBuf>,
    registrations: RepoIdentityCache,
    cached: Option<CachedProjects>,
}

struct CachedProjects {
    registry: Option<(PathBuf, FileStamp)>,
    configs: Vec<(PathBuf, FileStamp)>,
    host: String,
    database: PathBuf,
    database_present: bool,
    overrides: BTreeMap<String, RepoKey>,
    expires: Instant,
    projects: Vec<ResolvedProject>,
}

#[derive(Eq, PartialEq)]
enum FileStamp {
    Available(SystemTime, u64),
    Missing,
    Unreadable(std::io::ErrorKind),
}

impl ProjectResolver {
    /// An explicit registry keeps embedded callers and fixtures independent of env.
    pub fn with_registry(registry: impl Into<PathBuf>) -> Self {
        Self {
            registry: Some(registry.into()),
            ..Self::default()
        }
    }

    pub fn resolve(
        &mut self,
        config: &BoardConfig,
        host: &str,
        deadline: QueryDeadline,
    ) -> Result<Vec<ResolvedProject>, BoardError> {
        check_deadline(deadline)?;
        config.ensure_local().map_err(BoardError::from)?;
        let registry = self
            .registry
            .clone()
            .or_else(crate::workspace::config::registry_path);
        let registry_stamp = registry
            .as_ref()
            .map(|path| (path.clone(), file_stamp(path)));
        let database_present = match fs::symlink_metadata(&config.db_path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(BoardError::from(anyhow::Error::new(error))),
        };
        if let Some(cached) = &self.cached {
            if cached.expires > Instant::now()
                && cached.registry == registry_stamp
                && cached.host == host
                && cached.database == config.db_path
                && cached.database_present == database_present
                && cached.overrides == config.repos
                && cached
                    .configs
                    .iter()
                    .all(|(path, stamp)| file_stamp(path) == *stamp)
            {
                check_deadline(deadline)?;
                return Ok(cached.projects.clone());
            }
        }
        let workspaces = registry
            .as_deref()
            .map(registered_workspaces_from)
            .transpose()
            .map_err(BoardError::from)?
            .unwrap_or_default();
        check_deadline(deadline)?;
        let configs = workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.config_path.clone(),
                    file_stamp(&workspace.config_path),
                )
            })
            .collect();
        let reader = database_present
            .then(|| LocalBoard::open_read_with_timeout(config, deadline.cap(PROJECT_GIT_TIMEOUT)))
            .transpose()?;
        let mut projects = Vec::with_capacity(workspaces.len());
        for workspace in workspaces {
            check_deadline(deadline)?;
            projects.push(self.resolve_workspace(
                workspace,
                reader.as_ref(),
                config,
                host,
                deadline,
            )?);
        }
        disambiguate_names(&mut projects);
        check_deadline(deadline)?;
        self.cached = Some(CachedProjects {
            registry: registry_stamp,
            configs,
            host: host.to_owned(),
            database: config.db_path.clone(),
            database_present,
            overrides: config.repos.clone(),
            expires: Instant::now() + PROJECT_CACHE_TTL,
            projects: projects.clone(),
        });
        Ok(projects)
    }

    fn resolve_workspace(
        &mut self,
        workspace: RegisteredWorkspace,
        reader: Option<&LocalBoard>,
        config: &BoardConfig,
        host: &str,
        deadline: QueryDeadline,
    ) -> Result<ResolvedProject, BoardError> {
        let mut project = Project {
            id: workspace.id,
            name: workspace.name.clone(),
            repo_keys: BTreeSet::new(),
            unavailable: Vec::with_capacity(if workspace.error.is_some() {
                1
            } else {
                workspace.members.len()
            }),
        };
        if workspace.error.is_some() {
            // The configuration path identifies an unavailable workspace whose members are unknown.
            project.unavailable.push(RegisteredWorkspaceMember {
                name: workspace.name.clone(),
                root: workspace.config_path,
                available: false,
            });
        } else {
            for mut member in workspace.members {
                check_deadline(deadline)?;
                let key = if member.available {
                    self.member_key(&member.root, reader, config, host, deadline)?
                } else {
                    None
                };
                if let Some(key) = key {
                    project.repo_keys.insert(key);
                } else {
                    member.available = false;
                    project.unavailable.push(member);
                }
            }
        }
        Ok(ResolvedProject {
            project,
            raw_name: workspace.name,
        })
    }

    fn member_key(
        &mut self,
        root: &Path,
        reader: Option<&LocalBoard>,
        config: &BoardConfig,
        host: &str,
        deadline: QueryDeadline,
    ) -> Result<Option<RepoKey>, BoardError> {
        let common_dir = common_dir_bounded(root, deadline.cap(PROJECT_GIT_TIMEOUT));
        check_deadline(deadline)?;
        let Ok(common_dir) = common_dir else {
            return Ok(None);
        };
        if let Some(reader) = reader {
            if let Some(key) = reader.repo_key_at(host, &common_dir)? {
                return Ok(Some(key));
            }
        }
        let probe = self.registrations.register(
            root,
            host,
            None,
            &config.repos,
            deadline.cap(PROJECT_GIT_TIMEOUT),
        );
        check_deadline(deadline)?;
        Ok(probe
            .ok()
            .and_then(|probe| probe.registration.map(|registration| registration.repo_key)))
    }
}

fn disambiguate_names(projects: &mut [ResolvedProject]) {
    let mut counts = BTreeMap::new();
    for project in &*projects {
        *counts.entry(project.raw_name.clone()).or_insert(0usize) += 1;
    }
    for project in projects {
        if counts[&project.raw_name] > 1 {
            let suffix = project.project.id.get(..6).unwrap_or(&project.project.id);
            project.project.name = format!("{}·{suffix}", project.raw_name);
        }
    }
}

fn file_stamp(path: &Path) -> FileStamp {
    match fs::metadata(path) {
        Ok(metadata) => match metadata.modified() {
            Ok(modified) => FileStamp::Available(modified, metadata.len()),
            Err(error) => FileStamp::Unreadable(error.kind()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileStamp::Missing,
        Err(error) => FileStamp::Unreadable(error.kind()),
    }
}

fn check_deadline(deadline: QueryDeadline) -> Result<(), BoardError> {
    if deadline.expired() {
        return Err(BoardError::from(anyhow::anyhow!(
            "timed_out: project resolution deadline expired"
        )));
    }
    Ok(())
}

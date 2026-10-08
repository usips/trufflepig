//! Read registered workspace facts without selecting a retrieval workspace.

use super::{CONFIG_NAME, Member, RegistryDocument, WorkspaceConfig, config_id, read_document};
use super::{Path, PathBuf, resolve_path};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RegisteredWorkspaceMember {
    pub name: String,
    pub root: PathBuf,
    pub available: bool,
}

impl From<Member> for RegisteredWorkspaceMember {
    fn from(member: Member) -> Self {
        let available = member.verify_identity().is_ok();
        Self {
            name: member.name,
            root: member.root,
            available,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RegisteredWorkspace {
    pub name: String,
    pub id: String,
    pub config_path: PathBuf,
    pub members: Vec<RegisteredWorkspaceMember>,
    pub error: Option<String>,
}

impl RegisteredWorkspace {
    fn load(candidate: &Path, base: &Path) -> Self {
        let config_path = match resolve_path(candidate, base) {
            Ok(path) => path,
            Err(error) => return Self::unavailable(base.join(candidate), error),
        };
        match WorkspaceConfig::load(&config_path) {
            Ok(config) => Self {
                name: config.name,
                id: config.id,
                config_path: config.path,
                members: config.members.into_iter().map(Into::into).collect(),
                error: None,
            },
            Err(error) => Self::unavailable(config_path, error),
        }
    }

    fn unavailable(config_path: PathBuf, error: anyhow::Error) -> Self {
        let name = if config_path
            .file_name()
            .is_some_and(|name| name == CONFIG_NAME)
        {
            config_path.parent().and_then(Path::file_name)
        } else {
            config_path.file_stem()
        }
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| config_path.display().to_string());
        Self {
            name,
            id: config_id(&config_path),
            config_path,
            members: Vec::new(),
            error: Some(format!("{error:#}")),
        }
    }
}

/// The registry selected by XDG configuration, falling back to the user's home.
pub fn registry_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|base| base.join("trufflepig/workspaces.toml"))
}

/// List distinct registered configurations in registry order, retaining config errors.
/// An absent registry is empty; an unreadable or malformed registry is an error.
pub fn registered_workspaces() -> Result<Vec<RegisteredWorkspace>> {
    match registry_path() {
        Some(path) => registered_workspaces_from(&path),
        None => Ok(Vec::new()),
    }
}

/// Read one registry using the same path and configuration semantics as discovery.
pub fn registered_workspaces_from(registry_path: &Path) -> Result<Vec<RegisteredWorkspace>> {
    let path = if registry_path.is_absolute() {
        registry_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(registry_path)
    };
    if !path.try_exists().context("inspect workspace registry")? {
        return Ok(Vec::new());
    }
    let registry: RegistryDocument = read_document(&path)?;
    let base = path.parent().context("workspace registry has no parent")?;
    let mut workspaces = Vec::with_capacity(registry.workspaces.len());
    let mut seen = BTreeSet::new();
    for candidate in registry.workspaces {
        let workspace = RegisteredWorkspace::load(&candidate, base);
        if seen.insert(workspace.id.clone()) {
            workspaces.push(workspace);
        }
    }
    Ok(workspaces)
}

#[cfg(test)]
mod tests;

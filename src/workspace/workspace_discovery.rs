//! Selects the workspace for a request and maps its members onto the current
//! checkout: explicit, nearest-ancestor, then registry discovery.

use super::member_root::{self, MemberRoot};
use crate::workspace_config::{
    CONFIG_NAME, RegistryDocument, WorkspaceConfig, read_document, registry_path, resolve_path,
};
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeSet;
use std::path::Path;

/// The workspace selected by `--workspace`, the nearest ancestor configuration,
/// or the registry; `None` outside every workspace or with `--no-workspace`.
pub fn discover(
    start: &Path,
    explicit: Option<&Path>,
    no_workspace: bool,
    explicit_root: bool,
) -> Result<Option<WorkspaceConfig>> {
    ensure!(
        !(no_workspace && explicit.is_some()),
        "--workspace conflicts with --no-workspace"
    );
    if no_workspace {
        return Ok(None);
    }
    let start = resolve_path(start, &std::env::current_dir()?)?;
    if let Some(path) = explicit {
        let config = WorkspaceConfig::load(path)?;
        ensure!(
            !explicit_root || accepts_explicit_root(&config, &start)?,
            "explicit root is not an exact workspace member; use --no-workspace for a subtree"
        );
        return Ok(Some(config));
    }
    for ancestor in start.ancestors() {
        let path = ancestor.join(CONFIG_NAME);
        if path
            .try_exists()
            .context("inspect ancestor workspace configuration")?
        {
            let config = WorkspaceConfig::load(&path)?;
            return accept_root(config, &start, explicit_root);
        }
    }
    let Some(path) = registry_path() else {
        return Ok(None);
    };
    discover_registry(&path, &start, explicit_root)
}

/// Member roots of a configured workspace as seen from a starting path.
pub(crate) trait WorkspaceMembers {
    fn home_root(&self, start: &Path) -> Result<Option<MemberRoot>>;
    fn member_roots(&self, start: &Path) -> Result<Vec<MemberRoot>>;
}

impl WorkspaceMembers for WorkspaceConfig {
    /// The member whose checkout or linked worktree contains `start`.
    fn home_root(&self, start: &Path) -> Result<Option<MemberRoot>> {
        let start = resolve_path(start, &std::env::current_dir()?)?;
        Ok(
            member_root::home_root(&self.members, &start, member_root::GIT_PROBE_TIMEOUT)?.map(
                |mut home| {
                    home.is_home = true;
                    home
                },
            ),
        )
    }

    /// Every member with its effective root for `start`; only home is substituted.
    fn member_roots(&self, start: &Path) -> Result<Vec<MemberRoot>> {
        let home = self.home_root(start)?;
        Ok(self
            .members
            .iter()
            .map(|member| match &home {
                Some(home) if home.member.name == member.name => home.clone(),
                _ => MemberRoot::configured(member),
            })
            .collect())
    }
}

/// An explicit root must name a member root or a linked worktree of one.
fn accepts_explicit_root(config: &WorkspaceConfig, start: &Path) -> Result<bool> {
    if config.members.iter().any(|member| member.root == start) {
        return Ok(true);
    }
    Ok(config
        .home_root(start)?
        .is_some_and(|home| home.worktree.is_some() && home.root == start))
}

fn accept_root(
    config: WorkspaceConfig,
    start: &Path,
    explicit_root: bool,
) -> Result<Option<WorkspaceConfig>> {
    if explicit_root && !accepts_explicit_root(&config, start)? {
        return Ok(None);
    }
    Ok(Some(config))
}

fn discover_registry(
    path: &Path,
    start: &Path,
    explicit_root: bool,
) -> Result<Option<WorkspaceConfig>> {
    if !path.try_exists().context("inspect workspace registry")? {
        return Ok(None);
    }
    let registry: RegistryDocument = read_document(path)?;
    let base = path.parent().context("workspace registry has no parent")?;
    let mut matched = None;
    let mut seen = BTreeSet::new();
    for candidate in registry.workspaces {
        let path = resolve_path(&candidate, base)?;
        let config = WorkspaceConfig::load(&path).with_context(|| {
            format!(
                "invalid workspace registry entry {}; fix or remove it",
                path.display()
            )
        })?;
        if !seen.insert(config.id.clone()) || config.home_root(start)?.is_none() {
            continue;
        }
        if let Some(config) = accept_root(config, start, explicit_root)? {
            if let Some(previous) = &matched {
                let previous: &WorkspaceConfig = previous;
                bail!(
                    "ambiguous workspace membership: {} and {}; select --workspace FILE",
                    previous.path.display(),
                    config.path.display()
                );
            }
            matched = Some(config);
        }
    }
    Ok(matched)
}

#[cfg(test)]
mod tests;

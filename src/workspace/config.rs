//! Explicit workspace membership and stable configuration identity.
//! Member roots retain filesystem identity so persisted navigation cannot retarget.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

const CONFIG_LIMIT: u64 = 256 * 1024;
const MEMBER_LIMIT: usize = 32;
pub const CONFIG_NAME: &str = "trufflepig.workspace.toml";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MemberIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub root: PathBuf,
    pub identity: Option<MemberIdentity>,
}

impl Member {
    /// Verify the configured checkout still exists with its captured identity.
    pub fn verify_identity(&self) -> Result<()> {
        let metadata = fs::metadata(&self.root)
            .with_context(|| format!("workspace member {} unavailable", self.name))?;
        ensure!(
            metadata.is_dir(),
            "workspace member {} is not a directory",
            self.name
        );
        ensure!(
            self.root.canonicalize()? == self.root,
            "workspace member {} canonical path changed",
            self.name
        );
        let identity = self.identity.as_ref().with_context(|| {
            format!(
                "workspace member {} was unavailable when captured",
                self.name
            )
        })?;
        ensure!(
            identity.device == metadata.dev() && identity.inode == metadata.ino(),
            "workspace member {} checkout was replaced",
            self.name
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkspaceConfig {
    pub name: String,
    pub id: String,
    pub path: PathBuf,
    pub members: Vec<Member>,
    pub semantic: SemanticConfig,
    pub output: OutputConfig,
}

/// Persistent workspace-level response defaults applied when a request
/// does not pass the corresponding flag explicitly.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct OutputConfig {
    /// Default `o200k_base` token budget for serialized responses.
    pub budget: Option<usize>,
}

/// Persistent workspace-level semantic retrieval preference.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SemanticConfig {
    pub enabled: bool,
    pub rerank: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigDocument {
    workspace: WorkspaceSection,
    members: BTreeMap<String, MemberSection>,
    semantic: Option<SemanticSection>,
    output: Option<OutputSection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputSection {
    budget: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceSection {
    name: String,
    members: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberSection {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticSection {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    rerank: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    workspaces: Vec<PathBuf>,
}

impl WorkspaceConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let path = resolve_path(path, &std::env::current_dir()?)?
            .canonicalize()
            .with_context(|| format!("workspace configuration unavailable: {}", path.display()))?;
        let document: ConfigDocument = read_document(&path)?;
        ensure!(
            valid_name(&document.workspace.name),
            "invalid workspace name"
        );
        ensure!(
            !document.members.is_empty(),
            "workspace must contain members"
        );
        ensure!(
            document.members.len() <= MEMBER_LIMIT,
            "workspace exceeds 32 members"
        );
        let names = if let Some(names) = document.workspace.members {
            let unique: BTreeSet<_> = names.iter().collect();
            ensure!(
                unique.len() == names.len(),
                "duplicate workspace member selection"
            );
            ensure!(
                unique.len() == document.members.len()
                    && unique
                        .iter()
                        .all(|name| document.members.contains_key(*name)),
                "workspace.members must list every declared member exactly once"
            );
            names
        } else {
            document.members.keys().cloned().collect()
        };
        let base = path
            .parent()
            .context("workspace configuration has no parent")?;
        let mut members: Vec<Member> = Vec::with_capacity(names.len());
        for name in names {
            ensure!(valid_name(&name), "invalid workspace member name: {name}");
            let root = resolve_path(&document.members[&name].path, base)?;
            let identity = match fs::metadata(&root) {
                Ok(metadata) => {
                    ensure!(
                        metadata.is_dir(),
                        "workspace member {name} is not a directory"
                    );
                    Some(MemberIdentity {
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error).context("inspect workspace member"),
            };
            for other in &members {
                ensure!(
                    !root.starts_with(&other.root) && !other.root.starts_with(&root),
                    "workspace members {name} and {} have duplicate or overlapping roots",
                    other.name
                );
            }
            members.push(Member {
                name,
                root,
                identity,
            });
        }
        let id = blake3::hash(path.as_os_str().as_bytes())
            .to_hex()
            .to_string();
        Ok(Self {
            name: document.workspace.name,
            id,
            path,
            members,
            semantic: document
                .semantic
                .map(|semantic| SemanticConfig {
                    enabled: semantic.enabled,
                    rerank: semantic.rerank,
                })
                .unwrap_or_default(),
            output: {
                let budget = document.output.and_then(|output| output.budget);
                if let Some(budget) = budget {
                    ensure!(
                        (1..=crate::cli::MAX_BUDGET).contains(&budget),
                        "invalid workspace output budget: expected 1..{}",
                        crate::cli::MAX_BUDGET
                    );
                }
                OutputConfig { budget }
            },
        })
    }

    pub fn discover(
        start: &Path,
        explicit: Option<&Path>,
        no_workspace: bool,
        explicit_root: bool,
    ) -> Result<Option<Self>> {
        ensure!(
            !(no_workspace && explicit.is_some()),
            "--workspace conflicts with --no-workspace"
        );
        if no_workspace {
            return Ok(None);
        }
        let start = resolve_path(start, &std::env::current_dir()?)?;
        if let Some(path) = explicit {
            let config = Self::load(path)?;
            ensure!(
                !explicit_root || config.members.iter().any(|member| member.root == start),
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
                let config = Self::load(&path)?;
                return Ok(config.accept_root(&start, explicit_root));
            }
        }
        let Some(base) = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        else {
            return Ok(None);
        };
        discover_registry(
            &base.join("trufflepig/workspaces.toml"),
            &start,
            explicit_root,
        )
    }

    pub fn home(&self, start: &Path) -> Option<&Member> {
        let start = resolve_path(start, &std::env::current_dir().ok()?).ok()?;
        self.members
            .iter()
            .find(|member| start.starts_with(&member.root))
    }

    fn accept_root(self, start: &Path, explicit_root: bool) -> Option<Self> {
        (!explicit_root || self.members.iter().any(|member| member.root == start)).then_some(self)
    }
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
        if !seen.insert(config.id.clone()) || config.home(start).is_none() {
            continue;
        }
        if let Some(config) = config.accept_root(start, explicit_root) {
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

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Resolve config-relative paths and existing ancestors without creating directories.
pub fn resolve_path(path: &Path, base: &Path) -> Result<PathBuf> {
    let mut components = path.components();
    let expanded = if components.next() == Some(Component::Normal(std::ffi::OsStr::new("~"))) {
        PathBuf::from(std::env::var_os("HOME").context("HOME is required to expand ~")?)
            .join(components.as_path())
    } else {
        ensure!(
            !path.as_os_str().as_bytes().starts_with(b"~"),
            "only ~/ paths support tilde expansion"
        );
        path.to_path_buf()
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    ensure!(
        absolute.is_absolute(),
        "workspace path base must be absolute"
    );
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => {
                resolved.push(other.as_os_str());
                match resolved.canonicalize() {
                    Ok(canonical) => resolved = canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error).context("resolve workspace path"),
                }
            }
        }
    }
    Ok(resolved)
}

fn read_document<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .take(CONFIG_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= CONFIG_LIMIT,
        "workspace configuration exceeds 256 KiB"
    );
    let contents = std::str::from_utf8(&bytes).context("workspace configuration is not UTF-8")?;
    toml::from_str(contents)
        .with_context(|| format!("parse workspace configuration {}", path.display()))
}

pub fn load(path: &Path) -> Result<WorkspaceConfig> {
    WorkspaceConfig::load(path)
}

pub fn discover(
    start: &Path,
    explicit: Option<&Path>,
    no_workspace: bool,
    explicit_root: bool,
) -> Result<Option<WorkspaceConfig>> {
    WorkspaceConfig::discover(start, explicit, no_workspace, explicit_root)
}

#[cfg(test)]
mod tests;

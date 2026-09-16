//! A member's effective root: the configured checkout, or a linked Git worktree
//! of it that the invocation directory lives in. Membership of a worktree is
//! decided by canonical Git common directory, never by path prefix alone.

#[cfg(test)]
mod tests;

use super::config::{Member, MemberIdentity};
use crate::history::git;
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// One member as seen from an invocation directory.
#[derive(Clone, Debug)]
pub struct MemberRoot {
    pub member: Member,
    /// Canonical root that owns this member's index for the request.
    pub root: PathBuf,
    pub identity: Option<MemberIdentity>,
    /// Worktree directory name when `root` is a linked worktree of the member.
    pub worktree: Option<String>,
    /// True for the member whose root contains the invocation directory.
    pub is_home: bool,
}

impl MemberRoot {
    pub fn configured(member: &Member) -> Self {
        Self {
            member: member.clone(),
            root: member.root.clone(),
            identity: member.identity.clone(),
            worktree: None,
            is_home: false,
        }
    }

    /// Substitutes a linked worktree top for the member's configured root.
    pub fn linked(member: &Member, root: PathBuf) -> Result<Self> {
        let metadata = fs::metadata(&root).context("inspect linked worktree root")?;
        ensure!(metadata.is_dir(), "linked worktree root is not a directory");
        let label = root
            .file_name()
            .and_then(|name| name.to_str())
            .context("linked worktree root has no UTF-8 name")?
            .to_owned();
        Ok(Self {
            member: member.clone(),
            root,
            identity: Some(MemberIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            }),
            worktree: Some(label),
            is_home: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.member.name
    }

    /// `member` for a configured root, `member@worktree` for a substitute.
    pub fn display_name(&self) -> String {
        match &self.worktree {
            Some(label) => format!("{}@{label}", self.member.name),
            None => self.member.name.clone(),
        }
    }

    /// Verifies the effective root still exists with its captured identity.
    pub fn verify_identity(&self) -> Result<()> {
        if self.worktree.is_none() {
            return self.member.verify_identity();
        }
        let metadata = fs::metadata(&self.root)
            .with_context(|| format!("workspace member {} unavailable", self.display_name()))?;
        ensure!(
            metadata.is_dir(),
            "workspace member {} is not a directory",
            self.display_name()
        );
        ensure!(
            self.root.canonicalize()? == self.root,
            "workspace member {} canonical path changed",
            self.display_name()
        );
        let identity = self.identity.as_ref().with_context(|| {
            format!(
                "workspace member {} was unavailable when captured",
                self.display_name()
            )
        })?;
        ensure!(
            identity.device == metadata.dev() && identity.inode == metadata.ino(),
            "workspace member {} checkout was replaced",
            self.display_name()
        );
        Ok(())
    }
}

/// Nearest ancestor of `start` (inclusive) whose `.git` entry is a file, which
/// marks a linked worktree top; `None` once a `.git` directory or `/` is met.
pub(crate) fn linked_worktree_top(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        match fs::metadata(ancestor.join(".git")) {
            Ok(metadata) if metadata.is_file() => return Some(ancestor.to_path_buf()),
            Ok(_) => return None,
            Err(_) => {}
        }
    }
    None
}

/// The member's Git common directory, without a subprocess for a plain checkout.
pub(crate) fn member_common_dir(member: &Member) -> Option<PathBuf> {
    let dot_git = member.root.join(".git");
    match fs::metadata(&dot_git) {
        Ok(metadata) if metadata.is_dir() => dot_git.canonicalize().ok(),
        Ok(_) => git::common_dir(&member.root).ok(),
        Err(_) => None,
    }
}

/// `Some(top)` when `candidate` is a linked worktree top sharing `member`'s
/// common directory; a configured root or an unrelated checkout yields `None`.
pub(crate) fn linked_root_of_member(member: &Member, candidate: &Path) -> Option<PathBuf> {
    let candidate = candidate.canonicalize().ok()?;
    if linked_worktree_top(&candidate)? != candidate || candidate == member.root {
        return None;
    }
    let common = git::common_dir(&candidate).ok()?;
    (member_common_dir(member) == Some(common)).then_some(candidate)
}

/// Resolves the home member for `start`, substituting a linked worktree.
pub(crate) fn home_root(members: &[Member], start: &Path) -> Result<Option<MemberRoot>> {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let top = linked_worktree_top(&start);
    if let Some(member) = members
        .iter()
        .find(|member| start.starts_with(&member.root))
    {
        return Ok(Some(match top {
            Some(top) if top != member.root && linked_root_of_member(member, &top).is_some() => {
                MemberRoot::linked(member, top)?
            }
            _ => MemberRoot::configured(member),
        }));
    }
    let Some(top) = top else {
        return Ok(None);
    };
    let Ok(common) = git::common_dir(&top) else {
        return Ok(None);
    };
    for member in members {
        if member_common_dir(member) == Some(common.clone()) {
            return Ok(Some(MemberRoot::linked(member, top)?));
        }
    }
    Ok(None)
}

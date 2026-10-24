//! Portable root-commit identity and host-local repository registration.
use crate::board::board_ids::{PlanId, RepoKey};
use crate::board::board_protocol::RepoRegistration;
use crate::coded_error::CodedContext;
use crate::git_runner::{GitCommandFailed, common_dir_bounded, run_bounded_strict as run_bounded};
use crate::identity::GitOid;
use anyhow::{Result, ensure};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod registration_cache;
pub use registration_cache::{RegistrationProbe, RepoIdentityCache};

const ORIGIN_QUERY: &str = "^(remote\\.origin\\.url|core\\.repositoryformatversion)$";

/// Canonical checkout root; a bare repository, or a directory outside any
/// work tree, has no working root.
pub fn repository_root(directory: &Path, timeout: Duration) -> Result<Option<PathBuf>> {
    let deadline = Instant::now() + timeout;
    match run_bounded(
        directory,
        &["rev-parse", "--is-inside-work-tree"],
        remaining(deadline)?,
    ) {
        Ok(inside) if inside == b"true\n" => {}
        Ok(_) => return Ok(None),
        Err(error) if found_no_repository(&error) => return Ok(None),
        Err(error) => return Err(error),
    }
    let bytes = run_bounded(
        directory,
        &["rev-parse", "--show-toplevel"],
        remaining(deadline)?,
    )?;
    output_path(bytes)
        .canonicalize()
        .map(Some)
        .with_coded_context(|| coded!(BoardScan, "canonicalize working root"))
}

fn found_no_repository(error: &anyhow::Error) -> bool {
    GitCommandFailed::from_error(error).is_some_and(GitCommandFailed::found_no_repository)
}

/// `symbolic-ref -q` exits 1, silently, when HEAD is detached.
fn head_is_detached(error: &anyhow::Error) -> bool {
    GitCommandFailed::from_error(error).is_some_and(|failure| failure.exit_code() == Some(1))
}

/// Nonrepositories and unborn HEADs have no identity and return `None`.
pub fn register_repository(
    directory: &Path,
    host: &str,
    plan_id: Option<PlanId>,
    timeout: Duration,
) -> Result<Option<RepoRegistration>> {
    register_repository_with_overrides(directory, host, plan_id, &BTreeMap::new(), timeout)
}

fn register_repository_with_overrides(
    directory: &Path,
    host: &str,
    plan_id: Option<PlanId>,
    overrides: &BTreeMap<String, RepoKey>,
    timeout: Duration,
) -> Result<Option<RepoRegistration>> {
    let deadline = Instant::now() + timeout;
    let directory = directory
        .canonicalize()
        .with_coded_context(|| coded!(BoardScan, "canonicalize repository root"))?;
    let common_dir = match common_dir_bounded(&directory, remaining(deadline)?) {
        Ok(path) => path,
        Err(error) if found_no_repository(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(head) = resolve_head(&directory, deadline)? else {
        return Ok(None);
    };
    let config = run_bounded(
        &directory,
        &["config", "--null", "--get-regexp", ORIGIN_QUERY],
        remaining(deadline)?,
    )?;
    let origin_label = origin_from_config(&config)?;
    let override_key = origin_label
        .as_ref()
        .and_then(|origin| overrides.get(origin));
    let shallow = run_bounded(
        &directory,
        &["rev-parse", "--is-shallow-repository"],
        remaining(deadline)?,
    )? == b"true\n";
    ensure!(
        !shallow || override_key.is_some(),
        coded!(
            BoardScan,
            "shallow repository requires a [repos] origin override for portable identity"
        )
    );
    let root_commits = if shallow {
        Vec::new()
    } else {
        let roots = run_bounded(
            &directory,
            &[
                "rev-list",
                "--max-parents=0",
                "--exclude=refs/notes/*",
                "--exclude=refs/stash",
                "--all",
                head.as_str(),
            ],
            remaining(deadline)?,
        )?;
        std::str::from_utf8(&roots)?
            .lines()
            .map(GitOid::parse)
            .collect::<Result<Vec<_>>>()?
    };
    let repo_key = match override_key {
        Some(key) => key.clone(),
        None => RepoKey::from_roots(root_commits.iter().copied())?,
    };
    Ok(Some(RepoRegistration {
        repo_key,
        origin_label,
        host: host.to_owned(),
        common_dir,
        plan_id,
        root_commits,
        registration_error: None,
        origin_override: override_key.cloned(),
    }))
}

/// True when a stored binding and a live registration share no root commit,
/// meaning a different repository sits behind the host/common-directory pair.
/// An empty set cannot prove divergence and keeps the stored identity.
pub fn root_sets_diverged(stored: &[GitOid], current: &[GitOid]) -> bool {
    !stored.is_empty() && !current.is_empty() && stored.iter().all(|root| !current.contains(root))
}

fn resolve_head(directory: &Path, deadline: Instant) -> Result<Option<GitOid>> {
    match run_bounded(
        directory,
        &["symbolic-ref", "-q", "HEAD"],
        remaining(deadline)?,
    ) {
        Ok(reference) => {
            let reference = std::str::from_utf8(&reference)?.trim();
            ensure!(
                reference.starts_with("refs/") && !reference.chars().any(char::is_whitespace),
                coded!(BoardScan, "invalid symbolic HEAD")
            );
            let refs = run_bounded(
                directory,
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)",
                    reference,
                ],
                remaining(deadline)?,
            )?;
            for entry in refs
                .split(|byte| *byte == b'\n')
                .filter(|entry| !entry.is_empty())
            {
                let (name, oid) = entry
                    .split_once_byte(0)
                    .with_coded_context(|| coded!(BoardScan, "invalid Git reference record"))?;
                if name == reference.as_bytes() {
                    return GitOid::parse(std::str::from_utf8(oid)?).map(Some);
                }
            }
            Ok(None)
        }
        Err(error) if head_is_detached(&error) => {
            let head = run_bounded(
                directory,
                &["rev-parse", "--verify", "HEAD^{commit}"],
                remaining(deadline)?,
            )?;
            GitOid::parse(std::str::from_utf8(&head)?.trim()).map(Some)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn detached_head(git_dir: &Path, deadline: Instant) -> Result<Option<GitOid>> {
    match run_bounded(
        git_dir,
        &["--git-dir=.", "symbolic-ref", "-q", "HEAD"],
        remaining(deadline)?,
    ) {
        Ok(_) => Ok(None),
        Err(error) if head_is_detached(&error) => {
            let head = run_bounded(
                git_dir,
                &["--git-dir=.", "rev-parse", "--verify", "HEAD^{commit}"],
                remaining(deadline)?,
            )?;
            GitOid::parse(std::str::from_utf8(&head)?.trim()).map(Some)
        }
        Err(error) => Err(error),
    }
}

fn origin_from_config(bytes: &[u8]) -> Result<Option<String>> {
    for item in bytes
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
    {
        let (key, value) = item
            .split_once_byte(b'\n')
            .with_coded_context(|| coded!(BoardScan, "invalid origin configuration"))?;
        if key == b"remote.origin.url" {
            let value = std::str::from_utf8(value)?.trim();
            return if value.is_empty() {
                Ok(None)
            } else {
                normalize_origin_label(value).map(Some)
            };
        }
    }
    Ok(None)
}

pub fn normalize_origin_label(value: &str) -> Result<String> {
    let value = value.trim();
    ensure!(
        !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control),
        coded!(InvalidOptions, "invalid repository origin label")
    );
    Ok(strip_origin_userinfo(value))
}

fn strip_origin_userinfo(value: &str) -> String {
    if let Some((scheme, rest)) = value.split_once("://") {
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        if let Some((_, host)) = authority.rsplit_once('@') {
            return format!("{scheme}://{host}{}", &rest[end..]);
        }
    } else if let Some((user, host_path)) = value.split_once('@') {
        if !user.contains('/') && host_path.contains(':') {
            return host_path.to_owned();
        }
    }
    value.to_owned()
}

pub(super) fn remaining(deadline: Instant) -> Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    ensure!(
        !remaining.is_zero(),
        coded!(BoardScan, "Git scan timed out")
    );
    Ok(remaining)
}

fn output_path(mut bytes: Vec<u8>) -> PathBuf {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    PathBuf::from(OsString::from_vec(bytes))
}

trait ByteSplit {
    fn split_once_byte(&self, delimiter: u8) -> Option<(&[u8], &[u8])>;
}
impl ByteSplit for [u8] {
    fn split_once_byte(&self, delimiter: u8) -> Option<(&[u8], &[u8])> {
        let index = self.iter().position(|byte| *byte == delimiter)?;
        Some((&self[..index], &self[index + 1..]))
    }
}

#[cfg(test)]
pub(crate) mod tests;

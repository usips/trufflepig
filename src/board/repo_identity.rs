//! Portable root-commit identity and host-local repository registration.
use super::board_ids::{PlanId, RepoKey};
use super::board_protocol::RepoRegistration;
use crate::history::git::{common_dir_bounded, run_bounded_strict as run_bounded};
use crate::identity::GitOid;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod registration_cache;
pub use registration_cache::{RegistrationProbe, RepoIdentityCache};

const ORIGIN_QUERY: &str = "^(remote\\.origin\\.url|core\\.repositoryformatversion)$";

/// Canonical checkout root; a bare repository has no working root.
pub fn repository_root(directory: &Path, timeout: Duration) -> Result<Option<PathBuf>> {
    match run_bounded(directory, &["rev-parse", "--show-toplevel"], timeout) {
        Ok(bytes) => output_path(bytes)
            .canonicalize()
            .map(Some)
            .context("board_scan: canonicalize working root"),
        Err(error)
            if error.to_string().contains("not a git repository")
                || error.to_string().contains("must be run in a work tree") =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
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
        .context("board_scan: canonicalize repository root")?;
    let common_dir = match common_dir_bounded(&directory, remaining(deadline)?) {
        Ok(path) => path,
        Err(error) if error.to_string().contains("not a git repository") => return Ok(None),
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
        "board_scan: shallow repository requires a [repos] origin override for portable identity"
    );
    let root_commits = if shallow {
        Vec::new()
    } else {
        let roots = run_bounded(
            &directory,
            &["rev-list", "--max-parents=0", "--all", head.as_str()],
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

fn resolve_head(directory: &Path, deadline: Instant) -> Result<Option<GitOid>> {
    match run_bounded(directory, &["symbolic-ref", "HEAD"], remaining(deadline)?) {
        Ok(reference) => {
            let reference = std::str::from_utf8(&reference)?.trim();
            ensure!(
                reference.starts_with("refs/") && !reference.chars().any(char::is_whitespace),
                "board_scan: invalid symbolic HEAD"
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
                    .context("board_scan: invalid Git reference record")?;
                if name == reference.as_bytes() {
                    return GitOid::parse(std::str::from_utf8(oid)?).map(Some);
                }
            }
            Ok(None)
        }
        Err(error) if error.to_string().contains("not a symbolic ref") => {
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
        &["--git-dir=.", "symbolic-ref", "HEAD"],
        remaining(deadline)?,
    ) {
        Ok(_) => Ok(None),
        Err(error) if error.to_string().contains("not a symbolic ref") => {
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
            .context("board_scan: invalid origin configuration")?;
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
        "invalid_options: invalid repository origin label"
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
    ensure!(!remaining.is_zero(), "board_scan: Git scan timed out");
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
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;
    use std::process::Command;

    pub(crate) struct GitFixture {
        pub directory: tempfile::TempDir,
        pub root: PathBuf,
        clock: Cell<i64>,
    }

    impl GitFixture {
        pub fn new() -> Self {
            let directory = crate::board::board_test_support::scratch("board-fixture-");
            let root = directory.path().join("repo");
            std::fs::create_dir(&root).unwrap();
            let fixture = Self {
                directory,
                root,
                clock: Cell::new(1_700_000_000),
            };
            fixture.git(&["init", "--quiet", "--initial-branch=main"]);
            fixture
        }

        pub fn git(&self, args: &[&str]) -> String {
            self.git_in(&self.root, args)
        }

        pub fn git_in(&self, directory: &Path, args: &[&str]) -> String {
            let mut command = Command::new("git");
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("GIT_") {
                    command.env_remove(key);
                }
            }
            let stamp = format!("{} +0000", self.clock.get());
            command
                .current_dir(directory)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.test",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_DATE", &stamp)
                .env("GIT_COMMITTER_DATE", stamp)
                .args(args);
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "Git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        }

        pub fn commit(&self, message: &str) -> GitOid {
            self.clock.set(self.clock.get() + 1);
            self.git(&["commit", "--allow-empty", "--quiet", "-m", message]);
            GitOid::parse(self.git(&["rev-parse", "HEAD"]).trim()).unwrap()
        }

        pub fn registration(&self) -> RepoRegistration {
            register_repository(&self.root, "fixture-host", None, Duration::from_secs(5))
                .unwrap()
                .unwrap()
        }
    }

    #[test]
    fn registration_is_portable_across_clones_and_linked_worktrees() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        fixture.git(&[
            "remote",
            "add",
            "origin",
            "https://example.test/repository.git",
        ]);
        let registration = fixture.registration();
        let clone = fixture.directory.path().join("clone");
        fixture.git(&[
            "clone",
            "--quiet",
            fixture.root.to_str().unwrap(),
            clone.to_str().unwrap(),
        ]);
        let clone_registration =
            register_repository(&clone, "another-host", None, Duration::from_secs(5))
                .unwrap()
                .unwrap();
        assert_eq!(clone_registration.repo_key, registration.repo_key);
        let linked = fixture.directory.path().join("linked");
        fixture.git(&[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().unwrap(),
        ]);
        let linked_registration =
            register_repository(&linked, "fixture-host", None, Duration::from_secs(5))
                .unwrap()
                .unwrap();
        assert_eq!(linked_registration.common_dir, registration.common_dir);
        assert_eq!(linked_registration.repo_key, registration.repo_key);
        assert_eq!(
            registration.origin_label.as_deref(),
            Some("https://example.test/repository.git")
        );
    }

    #[test]
    fn origin_label_removes_url_credentials() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        fixture.git(&[
            "remote",
            "add",
            "origin",
            "https://user:secret@example.test/path@revision.git",
        ]);
        assert_eq!(
            fixture.registration().origin_label.as_deref(),
            Some("https://example.test/path@revision.git")
        );
    }

    #[test]
    fn reftable_repository_registration_resolves_symbolic_head() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        fixture.git(&["refs", "migrate", "--ref-format=reftable"]);
        assert!(
            register_repository(&fixture.root, "fixture-host", None, Duration::from_secs(5))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn nonrepository_and_unborn_repository_have_no_portable_identity() {
        let fixture = GitFixture::new();
        assert!(
            register_repository(&fixture.root, "host", None, Duration::from_secs(5))
                .unwrap()
                .is_none()
        );
        assert!(
            register_repository(Path::new("/"), "host", None, Duration::from_secs(5))
                .unwrap()
                .is_none()
        );
        fixture.commit("root");
        assert!(fixture.registration().origin_label.is_none());
    }
}

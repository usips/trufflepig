//! Cached Git registration, invalidated by local HEAD and ref storage metadata.
use super::*;
use std::collections::{BTreeSet, HashMap};
use std::time::SystemTime;

const REGISTRATION_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug)]
pub struct RegistrationProbe {
    pub registration: Option<RepoRegistration>,
    pub warning: Option<String>,
    pub status: Option<String>,
}

#[derive(Default)]
pub struct RepoIdentityCache {
    entries: HashMap<PathBuf, CachedRegistration>,
    warned: BTreeSet<PathBuf>,
}

struct CachedRegistration {
    fingerprint: HeadFingerprint,
    overrides: BTreeMap<String, RepoKey>,
    expires: Instant,
    registration: Option<RepoRegistration>,
    status: Option<String>,
}

type HeadFingerprint = Vec<(PathBuf, Option<(SystemTime, u64)>)>;

impl RepoIdentityCache {
    pub fn register(
        &mut self,
        directory: &Path,
        host: &str,
        plan_id: Option<PlanId>,
        overrides: &BTreeMap<String, RepoKey>,
        timeout: Duration,
    ) -> Result<RegistrationProbe> {
        let root = directory
            .canonicalize()
            .context("board_scan: canonicalize repository root")?;
        let fingerprint = head_fingerprint(&root);
        let fresh = self.entries.get(&root).is_some_and(|entry| {
            entry.expires > Instant::now()
                && entry.fingerprint == fingerprint
                && entry.overrides == *overrides
        });
        if !fresh {
            let (registration, status) =
                match register_repository_with_overrides(&root, host, None, overrides, timeout) {
                    Ok(registration) => (registration, None),
                    Err(error) => (None, Some(format!("repository registration: {error:#}"))),
                };
            self.entries.insert(
                root.clone(),
                CachedRegistration {
                    fingerprint,
                    overrides: overrides.clone(),
                    expires: Instant::now() + REGISTRATION_TTL,
                    registration,
                    status,
                },
            );
        }
        let entry = self
            .entries
            .get(&root)
            .expect("registration cache initialized");
        let mut registration = entry.registration.clone();
        if let Some(registration) = &mut registration {
            registration.host = host.to_owned();
            registration.plan_id = plan_id;
        }
        let status = entry.status.clone();
        let warning = status
            .as_ref()
            .filter(|_| self.warned.insert(root))
            .cloned();
        Ok(RegistrationProbe {
            registration,
            warning,
            status,
        })
    }
}

fn head_fingerprint(root: &Path) -> HeadFingerprint {
    let Some(git_dir) = local_git_dir(root) else {
        return Vec::new();
    };
    let common = bounded_metadata(&git_dir.join("commondir"))
        .map(output_path)
        .map(|path| git_dir.join(path))
        .and_then(|path| path.canonicalize().ok())
        .unwrap_or_else(|| git_dir.clone());
    let mut paths = vec![
        git_dir.join("HEAD"),
        common.join("packed-refs"),
        common.join("config"),
        common.join("shallow"),
        common.join("reftable/tables.list"),
    ];
    if let Some(head) =
        bounded_metadata(&git_dir.join("HEAD")).and_then(|bytes| String::from_utf8(bytes).ok())
    {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            paths.push(common.join(reference));
        }
    }
    paths
        .into_iter()
        .map(|path| {
            let stamp = std::fs::metadata(&path).ok().and_then(|metadata| {
                metadata
                    .modified()
                    .ok()
                    .map(|modified| (modified, metadata.len()))
            });
            (path, stamp)
        })
        .collect()
}

fn local_git_dir(root: &Path) -> Option<PathBuf> {
    for ancestor in root.ancestors() {
        let path = ancestor.join(".git");
        if path.is_dir() {
            return path.canonicalize().ok();
        }
        if path.is_file() {
            let bytes = bounded_metadata(&path)?;
            let git_dir = output_path(bytes.strip_prefix(b"gitdir: ")?.to_vec());
            return ancestor.join(git_dir).canonicalize().ok();
        }
    }
    root.join("HEAD").is_file().then(|| root.to_owned())
}

fn bounded_metadata(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let mut bytes = Vec::with_capacity(128);
    std::fs::File::open(path)
        .ok()?
        .take(4097)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= 4096).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::repo_identity::tests::GitFixture;

    #[test]
    fn oversized_head_metadata_has_bounded_cached_diagnostics() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        std::fs::write(fixture.root.join(".git/HEAD"), "a".repeat(8192)).unwrap();
        assert!(bounded_metadata(&fixture.root.join(".git/HEAD")).is_none());
        let mut cache = RepoIdentityCache::default();
        let first = cache
            .register(
                &fixture.root,
                "fixture-host",
                None,
                &BTreeMap::new(),
                Duration::from_secs(1),
            )
            .unwrap();
        assert!(first.registration.is_none());
        assert!(first.warning.is_some());
        assert!(first.status.unwrap().len() < 4096);
        let second = cache
            .register(
                &fixture.root,
                "fixture-host",
                None,
                &BTreeMap::new(),
                Duration::from_secs(1),
            )
            .unwrap();
        assert!(second.warning.is_none());
        assert!(second.status.is_some());
    }

    #[test]
    fn registration_cache_invalidates_head_and_accepts_shallow_origin_override() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        fixture.git(&["remote", "add", "origin", "https://example.test/repo.git"]);
        let mut cache = RepoIdentityCache::default();
        let initial = cache
            .register(
                &fixture.root,
                "fixture-host",
                None,
                &BTreeMap::new(),
                Duration::from_secs(5),
            )
            .unwrap()
            .registration
            .unwrap();
        fixture.git(&["switch", "--orphan", "other-root"]);
        fixture.commit("other root");
        let changed = cache
            .register(
                &fixture.root,
                "fixture-host",
                None,
                &BTreeMap::new(),
                Duration::from_secs(5),
            )
            .unwrap()
            .registration
            .unwrap();
        assert_ne!(initial.repo_key, changed.repo_key);
        std::fs::write(
            fixture.root.join(".git/shallow"),
            format!("{}\n", changed.root_commits[0]),
        )
        .unwrap();
        let first = cache
            .register(
                &fixture.root,
                "fixture-host",
                None,
                &BTreeMap::new(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(first.registration.is_none());
        assert!(first.warning.is_some());
        let second = cache
            .register(
                &fixture.root,
                "fixture-host",
                None,
                &BTreeMap::new(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(second.warning.is_none());
        assert_eq!(first.status, second.status);
        let overrides = BTreeMap::from([(
            "https://example.test/repo.git".into(),
            initial.repo_key.clone(),
        )]);
        let recovered = cache
            .register(
                &fixture.root,
                "other-host",
                Some(PlanId::new(1).unwrap()),
                &overrides,
                Duration::from_secs(5),
            )
            .unwrap()
            .registration
            .unwrap();
        assert_eq!(recovered.repo_key, initial.repo_key);
        assert_eq!(recovered.host, "other-host");
        assert_eq!(recovered.plan_id, Some(PlanId::new(1).unwrap()));
    }
}

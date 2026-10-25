//! Help locate a missing source path without widening source reads.

use std::{
    error::Error as StdError,
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use crate::query_deadline::QueryDeadline;
use anyhow::Error;

use super::member_root::MemberRoot;
use crate::workspace_config::WorkspaceConfig;

const MAX_MEMBER_ROOT_CHECKS: usize = 32;

pub(super) fn decorate_source_unavailable(
    config: &WorkspaceConfig,
    member: &MemberRoot,
    target: &str,
    error: Error,
    deadline: &QueryDeadline,
) -> Error {
    if !is_source_unavailable(&error) {
        return error;
    }
    if deadline.expired() || member.verify_identity().is_err() || deadline.expired() {
        return error;
    }
    let Some(relative) = relative_target(target) else {
        return error;
    };
    if inspect_direct_file(&member.root, &relative, deadline) != DirectFile::Missing
        || deadline.expired()
    {
        return error;
    }

    let mut other_members = Vec::with_capacity(config.members.len().min(MAX_MEMBER_ROOT_CHECKS));
    for candidate in config.members.iter().take(MAX_MEMBER_ROOT_CHECKS) {
        if deadline.expired() {
            return error;
        }
        if candidate.name == member.name() {
            continue;
        }
        if candidate.verify_identity().is_err() || deadline.expired() {
            return error;
        }
        match inspect_direct_file(&candidate.root, &relative, deadline) {
            DirectFile::Present => other_members.push(candidate.name.as_str()),
            DirectFile::DeadlineExpired => return error,
            DirectFile::Missing | DirectFile::Unsafe => {}
        }
        if deadline.expired() {
            return error;
        }
    }
    if deadline.expired() {
        return error;
    }

    let mut help = format!(
        "selected member `{}` root `{}`; path values are relative to this root",
        member.display_name(),
        member.root.display()
    );
    if !other_members.is_empty() {
        let switches = other_members
            .iter()
            .map(|name| format!("`--member {name}`"))
            .collect::<Vec<_>>()
            .join(" or ");
        help.push_str(&format!("; retry with {switches}"));
    }
    if relative.components().count() == 1 {
        if let Some(recipe) = owner_search_recipe(&relative) {
            help.push_str(&format!("; discover its owner with {recipe}"));
        }
    }
    if deadline.expired() {
        return error;
    }
    Error::new(CurrentPathHelp {
        source: error,
        help,
    })
}

fn is_source_unavailable(error: &Error) -> bool {
    crate::coded_error::ErrorCode::SourceUnavailable.is_anywhere_in(error)
}

fn relative_target(target: &str) -> Option<PathBuf> {
    let has_path_prefix = target.starts_with("path:");
    let mut encoded_path = target.strip_prefix("path:").unwrap_or(target);
    if encoded_path.is_empty()
        || encoded_path.starts_with("read:")
        || encoded_path.starts_with("cursor:")
    {
        return None;
    }
    if encoded_path.contains(':') {
        if !has_path_prefix {
            return None;
        }
        let (path, range) = encoded_path.rsplit_once(':')?;
        let (first, last) = range.split_once('-')?;
        let first = first.parse::<usize>().ok()?;
        let last = last.parse::<usize>().ok()?;
        if path.is_empty() || first == 0 || last < first {
            return None;
        }
        encoded_path = path;
    }

    let relative = crate::path_codec::decode_path(encoded_path).ok()?;
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(relative)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectFile {
    Missing,
    Present,
    Unsafe,
    DeadlineExpired,
}

fn inspect_direct_file(root: &Path, relative: &Path, deadline: &QueryDeadline) -> DirectFile {
    if deadline.expired() {
        return DirectFile::DeadlineExpired;
    }
    let Ok(canonical_root) = root.canonicalize() else {
        return DirectFile::Unsafe;
    };
    if canonical_root != root {
        return DirectFile::Unsafe;
    }

    let mut path = canonical_root.clone();
    let mut components = relative.components().peekable();
    while let Some(Component::Normal(component)) = components.next() {
        path.push(component);
        if deadline.expired() {
            return DirectFile::DeadlineExpired;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return DirectFile::Missing;
            }
            Err(_) => return DirectFile::Unsafe,
        };
        if metadata.file_type().is_symlink() {
            return DirectFile::Unsafe;
        }
        if components.peek().is_some() {
            if !metadata.is_dir() {
                return DirectFile::Unsafe;
            }
        } else {
            if !metadata.is_file() {
                return DirectFile::Unsafe;
            }
            if deadline.expired() {
                return DirectFile::DeadlineExpired;
            }
            return match path.canonicalize() {
                Ok(canonical) if canonical.starts_with(&canonical_root) => DirectFile::Present,
                _ => DirectFile::Unsafe,
            };
        }
    }
    DirectFile::Unsafe
}

fn owner_search_recipe(relative: &Path) -> Option<String> {
    let basename = relative.file_name()?.to_str()?;
    if !basename
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return None;
    }
    Some(format!("`search 'file:{basename} kind:file ws:all'`"))
}

#[derive(Debug)]
struct CurrentPathHelp {
    source: Error,
    help: String,
}

impl fmt::Display for CurrentPathHelp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}; {}", self.source, self.help)
    }
}

impl StdError for CurrentPathHelp {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_config::{Member, MemberIdentity, OutputConfig, SemanticConfig};
    use std::{
        fs,
        os::unix::fs::{MetadataExt, symlink},
    };

    fn configured(name: &str, root: &Path) -> Member {
        let root = root.canonicalize().unwrap();
        let metadata = fs::metadata(&root).unwrap();
        Member {
            name: name.to_owned(),
            root,
            identity: Some(MemberIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            }),
        }
    }

    fn config(members: Vec<Member>) -> WorkspaceConfig {
        WorkspaceConfig {
            name: "test-workspace".to_owned(),
            id: "test-workspace".to_owned(),
            path: PathBuf::new(),
            members,
            semantic: SemanticConfig::default(),
            output: OutputConfig::default(),
        }
    }

    fn missing() -> Error {
        anyhow::anyhow!(coded!(SourceUnavailable, "path missing or unsafe"))
    }

    fn decorate(
        config: &WorkspaceConfig,
        member: &MemberRoot,
        target: &str,
        error: Error,
    ) -> Error {
        decorate_source_unavailable(config, member, target, error, &QueryDeadline::start())
    }

    #[test]
    fn suggests_verified_owner_and_excludes_same_named_main_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let main_root = directory.path().join("main");
        let worktree_root = directory.path().join("worktree");
        let owner_root = directory.path().join("owner");
        for root in [&main_root, &worktree_root, &owner_root] {
            fs::create_dir_all(root).unwrap();
        }
        fs::write(main_root.join("entity_completions.rs"), "main checkout\n").unwrap();
        fs::write(owner_root.join("entity_completions.rs"), "").unwrap();
        let configured_main = configured("project", &main_root);
        let mut selected = MemberRoot::configured(&configured_main);
        selected.root = worktree_root.canonicalize().unwrap();
        let metadata = fs::metadata(&selected.root).unwrap();
        selected.identity = Some(MemberIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        });
        selected.worktree = Some("feature".to_owned());
        let config = config(vec![configured_main, configured("owner", &owner_root)]);
        let original = missing();
        let result = decorate(&config, &selected, "entity_completions.rs", original);
        let message = result.to_string();

        assert!(message.starts_with("source_unavailable:"));
        assert!(message.contains("selected member `project@feature` root"));
        assert!(message.contains("`--member owner`"));
        assert!(!message.contains("`--member project`"));
        assert!(message.contains("search 'file:entity_completions.rs kind:file ws:all'"));
        assert_eq!(
            result.chain().nth(1).unwrap().to_string(),
            "source_unavailable: path missing or unsafe"
        );
    }

    #[test]
    fn ignores_traversal_handles_symlink_escapes_and_unrelated_errors() {
        let directory = tempfile::tempdir().unwrap();
        let selected_root = directory.path().join("selected");
        let other_root = directory.path().join("other");
        let outside_root = directory.path().join("outside");
        fs::create_dir_all(&selected_root).unwrap();
        fs::create_dir_all(other_root.join("nested")).unwrap();
        fs::create_dir_all(&outside_root).unwrap();
        fs::write(outside_root.join("x"), "outside\n").unwrap();
        symlink(outside_root.join("x"), selected_root.join("link")).unwrap();
        symlink(outside_root.join("x"), other_root.join("nested/link")).unwrap();
        let selected_config = configured("selected", &selected_root);
        let config = config(vec![
            selected_config.clone(),
            configured("other", &other_root),
        ]);
        let member = MemberRoot::configured(&selected_config);

        for target in [
            "../outside.rs",
            "path:src/%2e%2e/outside.rs",
            "resultset:1",
            "read:path:root-digest:file.rs:0-20@revision",
            "cursor:opaque",
            "link",
        ] {
            let original = missing();
            let expected = original.to_string();
            assert_eq!(
                decorate(&config, &member, target, original).to_string(),
                expected,
                "target: {target}"
            );
        }
        let unrelated = anyhow::anyhow!(coded!(InvalidPath, "traversal is unsupported"));
        let expected = unrelated.to_string();
        assert_eq!(
            decorate(&config, &member, "valid.rs", unrelated).to_string(),
            expected
        );
        let expired = QueryDeadline::after(std::time::Duration::ZERO);
        let original = missing();
        let expected = original.to_string();
        assert_eq!(
            decorate_source_unavailable(&config, &member, "valid.rs", original, &expired)
                .to_string(),
            expected
        );
        let message = decorate(&config, &member, "nested/link", missing()).to_string();
        assert!(!message.contains("`--member other`"));
    }
}

//! Locates the main checkout whose cache seeds a linked Git worktree's index.

use std::path::{Path, PathBuf};

/// Returns the main checkout of a linked worktree (`root/.git` is a file whose
/// common directory is `<main>/.git`); `None` for plain roots and Git failures.
pub(crate) fn linked_worktree_main_checkout(root: &Path) -> Option<PathBuf> {
    if !root.join(".git").is_file() {
        return None;
    }
    let common_dir = crate::history::git::common_dir(root).ok()?;
    if common_dir.file_name()? != ".git" {
        return None;
    }
    let main_checkout = common_dir.parent()?.to_owned();
    if !main_checkout.is_dir() || root.canonicalize().ok()? == main_checkout {
        return None;
    }
    Some(main_checkout)
}

#[cfg(test)]
mod tests {
    use super::linked_worktree_main_checkout;
    use std::fs;
    use std::path::Path;

    fn git(root: &Path, arguments: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(["-c", "user.name=Seed Test"])
            .args(["-c", "user.email=seed@example.invalid"])
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn linked_worktree_main_checkout_names_the_main_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let main = directory.path().join("main");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q"]);
        fs::write(main.join("lib.rs"), "fn main_only() {}\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "initial"]);
        let linked = directory.path().join("linked");
        let linked_arg = linked.to_str().unwrap();
        git(&main, &["worktree", "add", "-q", "--detach", linked_arg]);
        assert_eq!(
            linked_worktree_main_checkout(&linked),
            Some(main.canonicalize().unwrap())
        );
        assert_eq!(linked_worktree_main_checkout(&main), None);
        assert_eq!(linked_worktree_main_checkout(directory.path()), None);
    }
}

use super::*;
use std::process::Command;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn repository() -> (tempfile::TempDir, Member) {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("main");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("lib.rs"), "fn main_only() {}\n").unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let root = root.canonicalize().unwrap();
    let metadata = fs::metadata(&root).unwrap();
    let member = Member {
        name: "main".into(),
        root,
        identity: Some(MemberIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }),
    };
    (scratch, member)
}

#[test]
fn member_root_label_is_worktree_basename() {
    let (scratch, member) = repository();
    let worktree = scratch.path().join("elsewhere").join("feature-x");
    git(
        &member.root,
        &["worktree", "add", "--detach", worktree.to_str().unwrap()],
    );
    let worktree = worktree.canonicalize().unwrap();
    let linked = MemberRoot::linked(&member, worktree.clone()).unwrap();
    assert_eq!(linked.display_name(), "main@feature-x");
    assert_eq!(linked.root, worktree);
    assert!(linked.verify_identity().is_ok());
    assert_eq!(MemberRoot::configured(&member).display_name(), "main");
}

#[test]
fn home_root_substitutes_external_and_in_repo_worktrees() {
    let (scratch, member) = repository();
    let external = scratch.path().join("wt-external");
    let inside = member.root.join(".worktrees").join("wt-inside");
    for path in [&external, &inside] {
        git(
            &member.root,
            &["worktree", "add", "--detach", path.to_str().unwrap()],
        );
    }
    let members = vec![member.clone()];
    let home = home_root(&members, &external.join("src")).unwrap().unwrap();
    assert_eq!(home.worktree.as_deref(), Some("wt-external"));
    assert_eq!(home.root, external.canonicalize().unwrap());
    let home = home_root(&members, &inside).unwrap().unwrap();
    assert_eq!(home.worktree.as_deref(), Some("wt-inside"));
    let home = home_root(&members, &member.root.join("lib.rs"))
        .unwrap()
        .unwrap();
    assert!(home.worktree.is_none());
    assert_eq!(home.root, member.root);
    assert!(home_root(&members, scratch.path()).unwrap().is_none());
    assert!(linked_root_of_member(&member, &member.root).is_none());
    assert_eq!(
        linked_root_of_member(&member, &external),
        Some(external.canonicalize().unwrap())
    );
}

#[test]
fn home_root_ignores_unrelated_repositories_and_missing_git() {
    let (scratch, member) = repository();
    let other = scratch.path().join("other");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("README"), "other\n").unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "init"]);
    let foreign = scratch.path().join("foreign-wt");
    git(
        &other,
        &["worktree", "add", "--detach", foreign.to_str().unwrap()],
    );
    let members = vec![member];
    assert!(home_root(&members, &foreign).unwrap().is_none());
    assert!(home_root(&members, &other).unwrap().is_none());
    assert!(linked_worktree_top(&other).is_none());
    assert!(linked_worktree_top(scratch.path()).is_none());
}

#[test]
fn home_root_keeps_configured_root_for_nested_repository() {
    let (_scratch, member) = repository();
    let nested = member.root.join("vendor").join("nested");
    fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-q", "-b", "main"]);
    let members = vec![member.clone()];
    let home = home_root(&members, &nested.join("src")).unwrap().unwrap();
    assert!(home.worktree.is_none());
    assert_eq!(home.root, member.root);
}

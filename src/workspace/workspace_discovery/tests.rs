use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use tp_core::workspace_config::registered_workspaces_from;

fn fixture() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn write_config(base: &Path, body: &str) -> PathBuf {
    let path = base.join(CONFIG_NAME);
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn workspace_config_captures_missing_members_and_stable_identity() {
    let directory = fixture();
    fs::create_dir(directory.path().join("engine")).unwrap();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='porting'\n[members.engine]\npath='engine'\n[members.pack]\npath='missing/../pack'\n",
    );
    let config = WorkspaceConfig::load(&path).unwrap();
    assert_eq!(config.members.len(), 2);
    assert!(config.members[0].identity.is_some());
    assert!(config.members[1].identity.is_none());
    assert_eq!(config.members[1].root, directory.path().join("pack"));
    assert_eq!(config.id, WorkspaceConfig::load(&path).unwrap().id);
    assert_eq!(
        config
            .home_root(&directory.path().join("engine/src"))
            .unwrap()
            .unwrap()
            .name(),
        "engine"
    );
    assert!(config.home_root(directory.path()).unwrap().is_none());
    let alias = directory.path().join("alias.toml");
    symlink(&path, &alias).unwrap();
    assert_eq!(config.id, WorkspaceConfig::load(&alias).unwrap().id);
}

#[test]
fn workspace_config_subtree_does_not_expand_explicit_root() {
    let directory = fixture();
    let root = directory.path().join("engine");
    fs::create_dir_all(root.join("src")).unwrap();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='work'\n[members.engine]\npath='engine'\n",
    );
    assert!(
        discover(&root.join("src"), None, false, false)
            .unwrap()
            .is_some()
    );
    assert!(
        discover(&root.join("src"), None, false, true)
            .unwrap()
            .is_none()
    );
    assert!(discover(&root.join("src"), Some(&path), false, true).is_err());
    assert!(discover(&root, Some(&path), false, true).unwrap().is_some());
    assert!(discover(&root, None, true, false).unwrap().is_none());
    assert!(discover(&root, Some(&path), true, false).is_err());
}

#[test]
fn workspace_config_explicit_selection_precedes_nearest_ancestor() {
    let directory = fixture();
    let root = directory.path().join("engine");
    fs::create_dir_all(root.join("src")).unwrap();
    let outer = write_config(
        directory.path(),
        "[workspace]\nname='outer'\n[members.engine]\npath='engine'\n",
    );
    write_config(
        &root,
        "[workspace]\nname='inner'\n[members.source]\npath='src'\n",
    );
    let start = root.join("src");
    assert_eq!(
        discover(&start, None, false, false).unwrap().unwrap().name,
        "inner"
    );
    assert_eq!(
        discover(&start, Some(&outer), false, false)
            .unwrap()
            .unwrap()
            .name,
        "outer"
    );
}

#[test]
fn workspace_registry_rejects_ambiguous_membership_and_stale_entries() {
    let directory = fixture();
    let root = directory.path().join("engine");
    fs::create_dir(&root).unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let body = "[workspace]\nname='work'\n[members.engine]\npath='../engine'\n";
    write_config(&first, body);
    write_config(&second, body);
    let registry = directory.path().join("workspaces.toml");
    fs::write(
        &registry,
        "workspaces=['first/trufflepig.workspace.toml', 'second/trufflepig.workspace.toml']",
    )
    .unwrap();
    assert!(
        discover_registry(&registry, &root, false)
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    fs::write(&registry, "workspaces=['first/trufflepig.workspace.toml']").unwrap();
    assert!(
        discover_registry(&registry, &root, false)
            .unwrap()
            .is_some()
    );
    fs::write(&registry, "workspaces=['missing.toml']").unwrap();
    assert!(
        discover_registry(&registry, &root, false)
            .unwrap_err()
            .to_string()
            .contains("fix or remove")
    );
}

#[test]
fn registered_workspace_reader_resolves_entries_from_registry_location() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path();
    fs::create_dir_all(base.join("host/repo")).unwrap();
    fs::create_dir(base.join("elsewhere")).unwrap();
    let config = base.join("host/local.toml");
    fs::write(
        &config,
        "[workspace]\nname='host'\n[members.repo]\npath='repo'\n",
    )
    .unwrap();
    let target = base.join("elsewhere/workspaces.toml");
    fs::write(&target, "workspaces=['local.toml']").unwrap();
    let registry = base.join("host/workspaces.toml");
    symlink(&target, &registry).unwrap();

    let discovered = discover_registry(&registry, &base.join("host/repo"), false)
        .unwrap()
        .unwrap();
    let rows = registered_workspaces_from(&registry).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, discovered.name);
    assert_eq!(rows[0].config_path, config);
    assert_eq!(rows[0].id, discovered.id);
    assert!(rows[0].error.is_none());
}

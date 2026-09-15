use super::*;
use std::os::unix::fs::symlink;

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
            .home(&directory.path().join("engine/src"))
            .unwrap()
            .name,
        "engine"
    );
    assert!(config.home(directory.path()).is_none());
    let alias = directory.path().join("alias.toml");
    symlink(&path, &alias).unwrap();
    assert_eq!(config.id, WorkspaceConfig::load(&alias).unwrap().id);
}

#[test]
fn workspace_config_reads_persistent_semantic_opt_in() {
    let directory = fixture();
    fs::create_dir(directory.path().join("engine")).unwrap();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='porting'\n[semantic]\nenabled=true\n[members.engine]\npath='engine'\n",
    );
    let config = WorkspaceConfig::load(&path).unwrap();
    assert!(config.semantic.enabled);
}

#[test]
fn workspace_config_semantics_default_to_disabled() {
    let directory = fixture();
    fs::create_dir(directory.path().join("engine")).unwrap();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='porting'\n[members.engine]\npath='engine'\n",
    );
    let config = WorkspaceConfig::load(&path).unwrap();
    assert!(!config.semantic.enabled);
}

#[test]
fn workspace_config_rejects_overlap_and_unknown_roles() {
    let directory = fixture();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='work'\n[members.a]\npath='engine'\n[members.b]\npath='engine/nested'\n",
    );
    assert!(
        WorkspaceConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("overlapping")
    );
    fs::write(
        &path,
        "[workspace]\nname='work'\n[members.a]\npath='engine'\nrole='engine'\n",
    )
    .unwrap();
    assert!(WorkspaceConfig::load(&path).is_err());
}

#[test]
fn workspace_config_rejects_duplicate_symlink_roots() {
    let directory = fixture();
    fs::create_dir(directory.path().join("engine")).unwrap();
    symlink(
        directory.path().join("engine"),
        directory.path().join("alias"),
    )
    .unwrap();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='work'\n[members.a]\npath='engine'\n[members.b]\npath='alias'\n",
    );
    assert!(WorkspaceConfig::load(&path).is_err());
}

#[test]
fn workspace_config_resolves_parent_after_symlink() {
    let directory = fixture();
    fs::create_dir_all(directory.path().join("nested/engine")).unwrap();
    symlink(
        directory.path().join("nested/engine"),
        directory.path().join("alias"),
    )
    .unwrap();
    assert_eq!(
        resolve_path(Path::new("alias/../missing"), directory.path()).unwrap(),
        directory.path().join("nested/missing")
    );
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
        WorkspaceConfig::discover(&root.join("src"), None, false, false)
            .unwrap()
            .is_some()
    );
    assert!(
        WorkspaceConfig::discover(&root.join("src"), None, false, true)
            .unwrap()
            .is_none()
    );
    assert!(WorkspaceConfig::discover(&root.join("src"), Some(&path), false, true).is_err());
    assert!(
        WorkspaceConfig::discover(&root, Some(&path), false, true)
            .unwrap()
            .is_some()
    );
    assert!(
        WorkspaceConfig::discover(&root, None, true, false)
            .unwrap()
            .is_none()
    );
    assert!(WorkspaceConfig::discover(&root, Some(&path), true, false).is_err());
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
        WorkspaceConfig::discover(&start, None, false, false)
            .unwrap()
            .unwrap()
            .name,
        "inner"
    );
    assert_eq!(
        WorkspaceConfig::discover(&start, Some(&outer), false, false)
            .unwrap()
            .unwrap()
            .name,
        "outer"
    );
}

#[test]
fn workspace_config_member_replacement_is_unavailable() {
    let directory = fixture();
    let root = directory.path().join("engine");
    fs::create_dir(&root).unwrap();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='work'\n[members.engine]\npath='engine'\n",
    );
    let config = WorkspaceConfig::load(&path).unwrap();
    config.members[0].verify_identity().unwrap();
    fs::rename(&root, directory.path().join("old-engine")).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(
        config.members[0]
            .verify_identity()
            .unwrap_err()
            .to_string()
            .contains("replaced")
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
fn workspace_config_enforces_schema_and_size_bounds() {
    let directory = fixture();
    for body in [
        "[workspace]\nname='work'\n[members]\n",
        "[workspace]\nname='bad name'\n[members.a]\npath='a'\n",
        "[workspace]\nname='work'\nmembers=['a','a']\n[members.a]\npath='a'\n",
        "[workspace]\nname='work'\nmembers=['missing']\n[members.a]\npath='a'\n",
        "[workspace]\nname='work'\n[members.'bad name']\npath='a'\n",
    ] {
        let path = write_config(directory.path(), body);
        assert!(WorkspaceConfig::load(&path).is_err(), "{body}");
    }
    let mut body = String::from("[workspace]\nname='work'\n");
    for index in 0..33 {
        body.push_str(&format!("[members.m{index}]\npath='m{index}'\n"));
    }
    let path = write_config(directory.path(), &body);
    assert!(
        WorkspaceConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("32 members")
    );
    fs::write(&path, vec![b' '; CONFIG_LIMIT as usize + 1]).unwrap();
    assert!(
        WorkspaceConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("256 KiB")
    );
}

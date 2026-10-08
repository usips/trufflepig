use super::*;
use std::{fs, os::unix::fs::symlink};

#[test]
fn registered_workspace_reader_preserves_identity_and_unavailable_members() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path();
    fs::create_dir(base.join("root")).unwrap();
    let valid = base.join("valid.toml");
    fs::write(
        &valid,
        "[workspace]\nname='shared'\n[members.live]\npath='root'\n[members.missing]\npath='missing'\n",
    )
    .unwrap();
    symlink(&valid, base.join("alias.toml")).unwrap();
    let other = base.join("other.toml");
    fs::write(
        &other,
        "[workspace]\nname='shared'\n[members.live]\npath='root'\n",
    )
    .unwrap();
    let registry = base.join("workspaces.toml");
    fs::write(
        &registry,
        "workspaces=['valid.toml', 'alias.toml', 'other.toml']",
    )
    .unwrap();

    let rows = registered_workspaces_from(&registry).unwrap();
    assert_eq!(
        rows.len(),
        2,
        "canonical aliases share a workspace identity"
    );
    assert_eq!(rows[0].id, WorkspaceConfig::load(&valid).unwrap().id);
    assert_eq!(rows[0].config_path, valid);
    assert!(rows[0].error.is_none());
    assert_eq!(rows[0].members.len(), 2);
    assert_eq!(rows[0].members[0].name, "live");
    assert!(rows[0].members[0].available);
    assert_eq!(rows[0].members[1].name, "missing");
    assert_eq!(rows[0].members[1].root, base.join("missing"));
    assert!(!rows[0].members[1].available);
    assert_eq!(rows[0].name, rows[1].name);
    assert_ne!(
        rows[0].id, rows[1].id,
        "names do not collapse distinct configs"
    );
}

#[test]
fn registered_workspace_reader_retains_each_config_error() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path();
    let stale = base.join("deleted.toml");
    fs::write(
        &stale,
        "[workspace]\nname='old-name'\n[members.missing]\npath='missing'\n",
    )
    .unwrap();
    let stale_id = WorkspaceConfig::load(&stale).unwrap().id;
    fs::remove_file(&stale).unwrap();
    let invalid = base.join("invalid.toml");
    fs::write(
        &invalid,
        "[workspace]\nname='bad name'\n[members.a]\npath='missing'\n",
    )
    .unwrap();
    let registry = base.join("workspaces.toml");
    fs::write(
        &registry,
        "workspaces=['deleted.toml', 'invalid.toml', '~unsupported/path']",
    )
    .unwrap();

    let rows = registered_workspaces_from(&registry).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].name, "deleted");
    assert_eq!(rows[0].config_path, stale);
    assert_eq!(rows[0].id, stale_id);
    assert!(rows[0].error.as_deref().unwrap().contains("unavailable"));
    assert_eq!(rows[1].name, "invalid");
    assert_eq!(rows[1].config_path, invalid);
    assert!(
        rows[1]
            .error
            .as_deref()
            .unwrap()
            .contains("invalid workspace name")
    );
    assert_eq!(rows[2].config_path, base.join("~unsupported/path"));
    assert!(
        rows[2]
            .error
            .as_deref()
            .unwrap()
            .contains("tilde expansion")
    );
    assert!(rows.iter().all(|row| row.members.is_empty()));
}

#[test]
fn registered_workspace_reader_distinguishes_missing_and_malformed_registry() {
    let directory = tempfile::tempdir().unwrap();
    let registry = directory.path().join("workspaces.toml");
    assert!(registered_workspaces_from(&registry).unwrap().is_empty());
    fs::write(&registry, "workspaces=4").unwrap();
    assert!(
        registered_workspaces_from(&registry)
            .unwrap_err()
            .to_string()
            .contains("parse workspace configuration")
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

    let discovered =
        crate::workspace::config::discover_registry(&registry, &base.join("host/repo"), false)
            .unwrap()
            .unwrap();
    let rows = registered_workspaces_from(&registry).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, discovered.name);
    assert_eq!(rows[0].config_path, config);
    assert_eq!(rows[0].id, discovered.id);
    assert!(rows[0].error.is_none());
}

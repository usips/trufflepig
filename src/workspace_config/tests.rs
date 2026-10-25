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
fn workspace_config_reads_persistent_semantic_opt_in() {
    let directory = fixture();
    fs::create_dir(directory.path().join("engine")).unwrap();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='porting'\n[semantic]\nenabled=true\n[members.engine]\npath='engine'\n",
    );
    let config = WorkspaceConfig::load(&path).unwrap();
    assert!(config.semantic.enabled);
    assert!(!config.semantic.rerank);
}

#[test]
fn workspace_config_reads_persistent_rerank_opt_in() {
    let directory = fixture();
    fs::create_dir(directory.path().join("engine")).unwrap();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='porting'\n[semantic]\nenabled=true\nrerank=true\n[members.engine]\npath='engine'\n",
    );
    let config = WorkspaceConfig::load(&path).unwrap();
    assert!(config.semantic.enabled);
    assert!(config.semantic.rerank);
}

#[test]
fn workspace_config_reads_output_budget_default_and_bounds() {
    let directory = fixture();
    fs::create_dir(directory.path().join("engine")).unwrap();
    let path = write_config(
        directory.path(),
        "[workspace]\nname='porting'\n[output]\nbudget=1200\n[members.engine]\npath='engine'\n",
    );
    let config = WorkspaceConfig::load(&path).unwrap();
    assert_eq!(config.output.budget, Some(1200));
    let absent = write_config(
        directory.path(),
        "[workspace]\nname='porting'\n[members.engine]\npath='engine'\n",
    );
    assert_eq!(WorkspaceConfig::load(&absent).unwrap().output.budget, None);
    let zero = write_config(
        directory.path(),
        "[workspace]\nname='porting'\n[output]\nbudget=0\n[members.engine]\npath='engine'\n",
    );
    assert!(
        WorkspaceConfig::load(&zero)
            .unwrap_err()
            .to_string()
            .contains("invalid workspace output budget")
    );
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
    assert!(!config.semantic.rerank);
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

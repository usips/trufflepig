use super::super::run;
use crate::workspace::config::CONFIG_NAME;
use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};

const CHILD_CASE: &str = "TRUFFLEPIG_WORKSPACE_LIST_CASE";
const FIXTURE_DIR: &str = "TRUFFLEPIG_WORKSPACE_LIST_FIXTURE";

fn in_child(case: &str) -> bool {
    if std::env::var(CHILD_CASE).as_deref() == Ok(case) {
        return true;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("cli::tests::workspace_list_tests::{case}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_CASE, case)
        .env(FIXTURE_DIR, directory.path())
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}{stderr}");
    assert!(output.status.success(), "registry CLI child failed");
    assert!(stdout.contains("running 1 test"));
    assert!(stdout.contains("1 passed; 0 failed; 0 ignored"));
    false
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(std::env::var_os(FIXTURE_DIR).unwrap())
}

fn registry_fixture() -> PathBuf {
    let registry = fixture_dir().join("config/trufflepig/workspaces.toml");
    let base = registry.parent().unwrap();
    for name in ["first", "second", "stale"] {
        let directory = base.join(name);
        fs::create_dir_all(directory.join("root")).unwrap();
        fs::write(
            directory.join(CONFIG_NAME),
            format!("[workspace]\nname='{name}'\n[members.{name}]\npath='root'\n"),
        )
        .unwrap();
    }
    fs::write(
        &registry,
        "workspaces=['first/trufflepig.workspace.toml', \
        'second/trufflepig.workspace.toml', 'stale/trufflepig.workspace.toml']",
    )
    .unwrap();
    fs::remove_file(base.join("stale").join(CONFIG_NAME)).unwrap();
    registry
}

fn list(extra: &[&str]) -> anyhow::Result<String> {
    let mut args = vec![
        "--root".to_owned(),
        fixture_dir().display().to_string(),
        "--no-daemon".into(),
        "--diagnostics".into(),
        "off".into(),
        "ws".into(),
        "list".into(),
    ];
    if !extra.contains(&"--budget") {
        args.extend(["--budget".into(), "10000".into()]);
    }
    args.extend(extra.iter().map(|word| (*word).to_owned()));
    run(&args)
}

#[test]
fn registry_lists_valid_and_stale_workspaces() {
    if !in_child("registry_lists_valid_and_stale_workspaces") {
        return;
    }
    let registry = registry_fixture();
    let output = list(&["--json"]).expect("list must retain a deleted registry entry");
    let value: Value = serde_json::from_str(&output).unwrap();
    let rows = value["workspaces"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    for (index, name) in ["first", "second"].into_iter().enumerate() {
        assert_eq!(rows[index]["name"], name);
        assert_eq!(rows[index]["members"][0]["name"], name);
        assert_eq!(rows[index]["members"][0]["available"], true);
        assert!(rows[index]["error"].is_null());
    }
    assert_ne!(rows[0]["id"], rows[1]["id"]);
    let stale = &rows[2];
    assert_eq!(stale["name"], "stale");
    assert_eq!(
        stale["config_path"],
        registry
            .parent()
            .unwrap()
            .join("stale")
            .join(CONFIG_NAME)
            .to_str()
            .unwrap()
    );
    assert_eq!(stale["id"].as_str().unwrap().len(), 64);
    assert!(stale["members"].as_array().unwrap().is_empty());
    assert!(stale["error"].as_str().unwrap().contains("unavailable"));
}

#[test]
fn workspace_list_lines_report_member_and_config_availability() {
    if !in_child("workspace_list_lines_report_member_and_config_availability") {
        return;
    }
    let registry = registry_fixture();
    fs::remove_dir_all(registry.parent().unwrap().join("second/root")).unwrap();
    let output = list(&[]).unwrap();
    assert!(output.contains("first\t"), "{output}");
    assert!(output.contains("second\t"), "{output}");
    assert!(output.contains("stale\t"), "{output}");
    assert!(output.contains("member\tfirst\t"), "{output}");
    let second = output
        .lines()
        .find(|line| line.starts_with("  member\tsecond\t"))
        .unwrap();
    assert!(second.ends_with("\tunavailable"), "{output}");
    assert!(output.contains("available"), "{output}");
    assert!(output.contains("unavailable"), "{output}");
    assert!(output.contains("error:"), "{output}");
}

#[test]
fn workspace_list_json_encodes_non_utf8_member_paths() {
    use std::{
        ffi::OsString,
        os::unix::{ffi::OsStringExt, fs::symlink},
    };

    if !in_child("workspace_list_json_encodes_non_utf8_member_paths") {
        return;
    }
    let registry = registry_fixture();
    let base = registry.parent().unwrap().join("first");
    let root = base.join(OsString::from_vec(b"checkout\xff".to_vec()));
    fs::create_dir(&root).unwrap();
    fs::remove_dir(base.join("root")).unwrap();
    symlink(&root, base.join("root")).unwrap();
    let output = list(&["--json"]).unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        value["workspaces"][0]["members"][0]["root"],
        crate::store::encode_path(&root)
    );
    assert_eq!(value["workspaces"][0]["members"][0]["available"], true);
}

#[test]
fn workspace_list_does_not_require_a_registry_or_workspace() {
    if !in_child("workspace_list_does_not_require_a_registry_or_workspace") {
        return;
    }
    let output = list(&["--json"]).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap()["workspaces"],
        serde_json::json!([])
    );
    assert!(
        list(&["extra"])
            .unwrap_err()
            .to_string()
            .contains("usage: ws list")
    );
    assert!(
        list(&["--json", "--budget", "1"])
            .unwrap_err()
            .to_string()
            .contains("budget_too_small")
    );
}

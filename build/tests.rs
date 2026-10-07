include!("tool_probe.rs");

#[test]
fn real_probe_parses_versions_and_rejects_invalid_output() {
    assert_eq!(
        tool_probe::parse_git_version(b"git version 2.55.0.vendor\n"),
        Some((2, 55))
    );
    assert_eq!(tool_probe::parse_git_version(b"git version two.55"), None);
    assert_eq!(tool_probe::parse_git_version(&[0xff]), None);
    for major in [20, 21, 22] {
        assert_eq!(
            tool_probe::parse_node_version(format!("v{major}.1.0\n").as_bytes()),
            Some(major)
        );
    }
    assert_eq!(tool_probe::parse_node_version(b"22.1.0"), None);
    assert_eq!(tool_probe::parse_node_version(&[0xff]), None);
}

#[test]
fn real_probe_skips_empty_path_entries() {
    use std::{env, path::PathBuf};
    let path =
        env::join_paths([PathBuf::new(), PathBuf::from("relative"), PathBuf::new()]).unwrap();
    assert_eq!(
        tool_probe::path_search_dirs(Some(&path)),
        vec![PathBuf::from("relative")]
    );
    assert!(tool_probe::path_search_dirs(None).is_empty());
}

#[cfg(unix)]
mod path_watches {
    use super::tool_probe;
    use std::{
        env, fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    const CHILD_TOOL: &str = "TRUFFLEPIG_PATH_WATCH_CHILD_TOOL";
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct ProbeFixture {
        root: PathBuf,
    }

    impl ProbeFixture {
        fn new() -> Self {
            let scratch = env::var_os("TMPDIR").expect("TMPDIR points at disk-backed scratch");
            let root = PathBuf::from(scratch).join(format!(
                "tool-probe-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("create isolated probe fixture");
            Self { root }
        }

        fn directory(&self, name: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir(&path).expect("create PATH fixture directory");
            path
        }

        fn install(&self, dir: &Path, name: &str, executable: bool) -> PathBuf {
            let path = dir.join(name);
            let version = if name == "git" {
                "git version 2.55.0"
            } else {
                "v22.20.0"
            };
            fs::write(&path, format!("#!/bin/sh\nprintf '{version}\\n'\n")).unwrap();
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
            )
            .unwrap();
            path
        }

        fn output(&self, test: &str, tool: &str, dirs: &[PathBuf]) -> String {
            let output = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    &format!("tests::path_watches::{test}"),
                    "--nocapture",
                ])
                .current_dir(&self.root)
                .env("PATH", env::join_paths(dirs).unwrap())
                .env(CHILD_TOOL, tool)
                .output()
                .expect("run isolated real build probe");
            assert!(
                output.status.success(),
                "child probe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(
                stdout.contains("\nrunning 1 test\n")
                    && stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
                "isolated --exact probe must run one test:\n{stdout}"
            );
            println!("{tool} child stdout:\n{stdout}");
            stdout
        }
    }

    impl Drop for ProbeFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove isolated probe fixture");
        }
    }

    fn emit_child_probe() -> bool {
        let Some(tool) = env::var_os(CHILD_TOOL) else {
            return false;
        };
        match tool.to_str().unwrap() {
            "git" => {
                super::super::emit_git_cfgs();
            }
            "node" => {
                super::super::emit_node_cfgs();
            }
            other => panic!("unexpected fixture tool: {other}"),
        }
        true
    }

    fn watched_paths(stdout: &str) -> Vec<PathBuf> {
        stdout
            .lines()
            .filter_map(|line| line.strip_prefix("cargo::rerun-if-changed="))
            .map(PathBuf::from)
            .collect()
    }

    #[test]
    fn earlier_path_dir_triggers_rerun() {
        if emit_child_probe() {
            return;
        }
        let fixture = ProbeFixture::new();
        let earlier = fixture.directory("earlier empty");
        let second = fixture.directory("second empty");
        let resolved = fixture.directory("resolved");
        let later = fixture.directory("later");
        fixture.directory("relative");
        let missing = fixture.root.join("missing");
        let dirs = [
            earlier.clone(),
            missing,
            PathBuf::from("relative"),
            PathBuf::new(),
            second.clone(),
            resolved.clone(),
            later,
        ];
        let mut missing_watches = Vec::new();
        for tool in ["git", "node"] {
            let executable = fixture.install(&resolved, tool, true);
            let stdout = fixture.output("earlier_path_dir_triggers_rerun", tool, &dirs);
            let watched = watched_paths(&stdout);
            for dir in [&earlier, &second] {
                if !watched.contains(dir) {
                    missing_watches.push(format!("{tool}: {}", dir.display()));
                }
            }
            assert!(
                watched.contains(&executable),
                "resolved {tool} binary is watched"
            );
            assert!(
                watched
                    .iter()
                    .all(|path| path.is_absolute() && path.exists())
            );
            if missing_watches.is_empty() {
                assert_eq!(
                    watched,
                    vec![earlier.clone(), second.clone(), executable],
                    "watch only the PATH prefix and selected {tool} binary"
                );
            }
        }
        assert!(
            missing_watches.is_empty(),
            "earlier PATH directories lack rerun watches: {missing_watches:?}"
        );
    }

    #[test]
    fn missing_tools_watch_only_existing_absolute_path_dirs() {
        if emit_child_probe() {
            return;
        }
        let fixture = ProbeFixture::new();
        let existing = fixture.directory("existing");
        fixture.directory("relative");
        let dirs = [
            existing.clone(),
            fixture.root.join("missing"),
            PathBuf::from("relative"),
        ];
        for tool in ["git", "node"] {
            let output = fixture.output(
                "missing_tools_watch_only_existing_absolute_path_dirs",
                tool,
                &dirs,
            );
            assert_eq!(watched_paths(&output), vec![existing.clone()]);
        }
    }

    #[test]
    fn real_probe_requires_executable_files_and_writes_report() {
        let fixture = ProbeFixture::new();
        let first = fixture.directory("first");
        let second = fixture.directory("second");
        let rejected = fixture.install(&first, "git", false);
        let executable = fixture.install(&second, "git", true);
        assert!(!tool_probe::is_executable_file(&rejected));
        assert_eq!(
            tool_probe::resolve_tool_in_dirs(&[first, second], "git"),
            Some(executable)
        );
        let report = tool_probe::format_probe_report(&[("git", "found"), ("node", "missing")]);
        let path = tool_probe::write_probe_report(&fixture.root, &report).unwrap();
        assert_eq!(path.file_name().unwrap(), tool_probe::REPORT_FILENAME);
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "git=found\nnode=missing\n"
        );
    }
}

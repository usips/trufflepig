use super::emission::*;
use std::io::{self, Write};

fn args(cache: &std::path::Path, extra: &[&str]) -> Vec<String> {
    [
        vec![
            "--no-daemon".into(),
            "--root".into(),
            cache.parent().unwrap().join("root").display().to_string(),
            "--cache".into(),
            cache.display().to_string(),
        ],
        extra.iter().map(|s| s.to_string()).collect(),
    ]
    .concat()
}

#[test]
fn emission_handles_help_version_usage_and_zero_budget() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::create_dir(scratch.path().join("root")).unwrap();
    let cache = scratch.path().join("cache");
    for (extra, expected_code) in [(vec!["--unknown-option"], 2), (vec!["show"], 2)] {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            execute(&args(&cache, &extra), &mut stdout, &mut stderr),
            expected_code
        );
        let response: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert!(response.get("error").is_some());
        assert!(stdout.ends_with(b"\n"));
    }
    // Help is complete plain text at the default budget, including the query summary.
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        execute(&args(&cache, &["--help"]), &mut stdout, &mut stderr),
        0
    );
    let help = String::from_utf8(stdout).unwrap();
    assert!(!help.starts_with('{'));
    assert!(help.contains("Maximum serialized output budget"));
    assert!(help.contains("Run locally without starting or using a daemon"));
    assert!(help.contains("sym:NAME"));
    assert!(help.contains("show HANDLE"));
    assert!(help.ends_with('\n'));
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        execute(&args(&cache, &["--version"]), &mut stdout, &mut stderr),
        0
    );
    assert!(
        String::from_utf8(stdout)
            .unwrap()
            .starts_with("trufflepig ")
    );
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    execute(
        &args(&cache, &["--budget", "0", "show"]),
        &mut stdout,
        &mut stderr,
    );
    assert!(stdout.is_empty());
}

#[test]
fn emission_budget_error_is_empty_on_stdout_and_actionable_on_stderr() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::create_dir(scratch.path().join("root")).unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());

    assert_eq!(
        execute(
            &args(&scratch.path().join("cache"), &["--budget", "1"]),
            &mut stdout,
            &mut stderr,
        ),
        2
    );
    assert!(stdout.is_empty());
    let message = String::from_utf8(stderr).unwrap();
    assert!(message.contains("budget_too_small"));
    assert!(message.contains("retry with -b 2000"));
}

#[test]
fn emission_unknown_command_is_usage_failure() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::create_dir(scratch.path().join("root")).unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());

    assert_eq!(
        execute(
            &args(&scratch.path().join("cache"), &["bogus"]),
            &mut stdout,
            &mut stderr,
        ),
        2
    );
    let response: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("unknown_command")
    );
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .contains("unknown_command")
    );
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        execute(
            &args(&scratch.path().join("cache"), &["search", "bogus"]),
            &mut stdout,
            &mut stderr
        ),
        0
    );
    let response: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(response["hits"].as_array().unwrap().is_empty());
    assert!(stderr.is_empty());
}

struct PartialWriter {
    accepted: usize,
}
impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.accepted > 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let count = bytes.len().min(7);
        self.accepted += count;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn emission_broken_pipe_returns_failure_without_retry() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::create_dir(scratch.path().join("root")).unwrap();
    let mut writer = PartialWriter { accepted: 0 };
    assert_eq!(
        execute(
            &args(&scratch.path().join("cache"), &["--version"]),
            &mut writer,
            &mut Vec::new()
        ),
        2
    );
    assert_eq!(writer.accepted, 7);
}

#[test]
fn lines_format_errors_stay_json() {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::create_dir(scratch.path().join("root")).unwrap();
    let cache = scratch.path().join("cache");
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = execute(
        &args(&cache, &["--format", "lines", "show"]),
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(code, 2);
    let response: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(response["error"].as_str().unwrap().contains("usage"));
}

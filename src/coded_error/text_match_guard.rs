//! Fails when non-test code branches on rendered error text or raises a
//! code as plain text instead of through [`coded!`](crate::coded_error).
use super::ErrorCode;
use std::path::{Path, PathBuf};

/// Third-party text with no typed form; each entry names why it stays.
const ALLOWED: &[(&str, &str)] = &[
    // SQLite reports these schema faults only as SQLITE_ERROR plus English text.
    (
        "src/store/index_checkpoint/checkpoint_storage.rs",
        "cause.to_string().to_ascii_lowercase()",
    ),
    // BoardError flattens SQLite and OS causes into its wire message.
    (
        "src/board/board_transport/board_client_transport/local_access.rs",
        "cause.to_string().to_ascii_lowercase()",
    ),
    // Provider and runtime failures arrive as text across the worker process boundary.
    (
        "src/semantic/preparation/worker.rs",
        "format!(\"{error:#}\").to_ascii_lowercase()",
    ),
];

const TEXT_BRANCHES: &[&str] = &[
    ".to_string().starts_with(",
    ".to_string().contains(",
    ".to_string().strip_prefix(",
    ".to_string().ends_with(",
    ".to_string().to_ascii_lowercase()",
    "format!(\"{error:#}\").to_ascii_lowercase()",
    "format!(\"{error:#}\").contains(",
    "format!(\"{error:#}\").starts_with(",
];

/// anyhow's own context hides a coded layer from wrapping errors' chains.
const HIDDEN_CODES: &[&str] = &[".context(coded!(", "with_context(|| coded!("];

const RAISERS: &[&str] = &[
    "bail!(",
    "anyhow!(",
    "ensure!(",
    ".context(",
    "with_context(",
];

#[test]
fn non_test_code_matches_codes_not_rendered_text() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    for path in rust_sources(&root.join("src")) {
        let relative = path.strip_prefix(root).unwrap().display().to_string();
        if is_test_file(&relative) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (number, line) in non_test_lines(&text) {
            let compact: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
            let allowed = ALLOWED
                .iter()
                .any(|(file, snippet)| *file == relative && compact.contains(snippet));
            for pattern in TEXT_BRANCHES.iter().chain(HIDDEN_CODES) {
                if compact.contains(pattern) && !allowed {
                    violations.push(format!("{relative}:{number}: {}", line.trim()));
                }
            }
            if RAISERS.iter().any(|raiser| compact.contains(raiser))
                && ErrorCode::ALL
                    .iter()
                    .any(|code| compact.contains(&format!("\"{}: ", code.as_str())))
            {
                violations.push(format!("{relative}:{number}: {}", line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "match error codes with ErrorCode and raise them with coded!:\n{}",
        violations.join("\n")
    );
}

fn rust_sources(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn is_test_file(relative: &str) -> bool {
    relative.ends_with("/tests.rs")
        || relative.ends_with("_tests.rs")
        || relative.contains("/tests/")
        || relative.ends_with("/text_match_guard.rs")
}

/// Lines outside `#[cfg(test)]` items, numbered from 1.
fn non_test_lines(text: &str) -> Vec<(usize, &str)> {
    let mut kept = Vec::new();
    let mut depth = 0i64;
    let mut test_depth = None;
    let mut pending_test = false;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if test_depth.is_none() {
            if trimmed.starts_with("#[cfg(test)]") {
                pending_test = true;
                continue;
            }
            if pending_test && !trimmed.starts_with('#') {
                pending_test = false;
                if !trimmed.ends_with(';') && trimmed.contains('{') {
                    test_depth = Some(depth);
                }
            } else if !pending_test {
                kept.push((index + 1, line));
            }
        }
        depth += braces(line);
        if test_depth.is_some_and(|start| depth <= start) {
            test_depth = None;
        }
    }
    kept
}

fn braces(line: &str) -> i64 {
    let opened = line.matches('{').count() as i64;
    let closed = line.matches('}').count() as i64;
    opened - closed
}

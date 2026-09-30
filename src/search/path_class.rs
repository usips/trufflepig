//! Path classes shared by ranking: tests and fixtures, and Markdown docs
//! (including agent pointer files). Matching is ASCII case-insensitive.

/// Test sources and fixtures by convention: a `tests/`, `test/`, `__tests__/`,
/// `fixtures/` or `testdata/` directory; a `tests`/`test` stem (`tests.rs`);
/// `test_*`, `*_test.*`, `*_tests.*`, `*.test.*` or `*.spec.*` files.
pub(crate) fn is_test_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let (directories, file) = path.rsplit_once('/').unwrap_or(("", &path));
    let stem = file.split('.').next().unwrap_or(file);
    directories.split('/').any(|directory| {
        matches!(
            directory,
            "tests" | "test" | "__tests__" | "fixtures" | "testdata"
        )
    }) || matches!(stem, "tests" | "test")
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || [".spec.", ".test.", "_test.", "_tests."]
            .iter()
            .any(|marker| file.contains(marker))
}

/// Markdown documentation, including agent pointer files (`AGENTS.md`, `CLAUDE.md`).
pub(crate) fn is_doc_pointer_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.ends_with(".md") || path.ends_with(".markdown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_and_doc_paths_are_classified_by_convention() {
        for path in [
            "src/sim/tests.rs",
            "crates/core/tests/motion.rs",
            "tests/bodies/organ_threshold_test.luau",
            "web/pack-ui.spec.mjs",
            "web/__tests__/hover.ts",
            "py/test_steer.py",
            "go/parse_test.go",
            "src/Tests/Motion.cs",
            "crates/server/tests/fixtures/pack/bodies/felled.luau",
            "src/fixtures/world.ron",
            "internal/testdata/a.json",
            "src/sim/tests/",
        ] {
            assert!(is_test_path(path), "{path}");
        }
        for path in [
            "src/sim/testing_support.rs",
            "src/latest.rs",
            "src/contest/mod.rs",
            "src/attest.rs",
        ] {
            assert!(!is_test_path(path), "{path}");
        }
        assert!(is_doc_pointer_path("AGENTS.md") && is_doc_pointer_path("docs/a/flat.md"));
        assert!(!is_doc_pointer_path("src/markdown.rs"));
    }
}

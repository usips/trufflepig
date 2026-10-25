//! Path classes shared by ranking: test sources and Markdown docs (including
//! agent pointer files). Matching is ASCII case-insensitive.

/// Test sources by convention: a `tests/`, `__tests__/`, `spec/` or
/// `testdata/` directory, or a `test/` directory that is not a crate root
/// (`test/src/`); a `tests` stem (`tests.rs`, `tests.py`) or `*_tests` stem;
/// `*.test.*` and `*.spec.*` files; `*_test.go`, `*_test.py`, `test_*.py` and
/// `*_spec.rb`. A bare `test` stem, a generic `*_test` stem and `fixtures/`
/// are production names (`Test.java`, `ab_test.rs`, content fixtures).
pub(crate) fn is_test_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let (directories, file) = path.rsplit_once('/').unwrap_or(("", &path));
    let (stem, extension) = file.split_once('.').unwrap_or((file, ""));
    let components: Vec<&str> = path.split('/').collect();
    let test_directory =
        directories
            .split('/')
            .enumerate()
            .any(|(index, directory)| match directory {
                "tests" | "__tests__" | "spec" | "testdata" => true,
                "test" => components.get(index + 1) != Some(&"src"),
                _ => false,
            });
    test_directory
        || stem == "tests"
        || stem.ends_with("_tests")
        || file.contains(".test.")
        || file.contains(".spec.")
        || (stem.ends_with("_test") && matches!(extension, "go" | "py"))
        || (stem.starts_with("test_") && extension == "py")
        || (stem.ends_with("_spec") && extension == "rb")
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
            "web/hover.test.ts",
            "web/__tests__/hover.ts",
            "py/test_steer.py",
            "py/steer_test.py",
            "go/parse_test.go",
            "src/Tests/Motion.cs",
            "src/test/java/MotionTest.java",
            "crates/server/tests/fixtures/pack/bodies/felled.luau",
            "internal/testdata/a.json",
            "spec/models/user_spec.rb",
            "app/tests.py",
            "src/sim/movement_tests.rs",
            "src/sim/tests/",
        ] {
            assert!(is_test_path(path), "{path}");
        }
        for path in [
            "src/sim/testing_support.rs",
            "src/latest.rs",
            "src/contest/mod.rs",
            "src/attest.rs",
            "content/fixtures/airlock.luau",
            "src/main/java/Test.java",
            "test.config.js",
            "library/test/src/lib.rs",
            "src/ab_test.rs",
            "src/speed_test.rs",
            "src/test_harness.rs",
        ] {
            assert!(!is_test_path(path), "{path}");
        }
        assert!(is_doc_pointer_path("AGENTS.md") && is_doc_pointer_path("docs/a/flat.md"));
        assert!(!is_doc_pointer_path("src/markdown.rs"));
    }
}

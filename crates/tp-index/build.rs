//! Exposes TRUFFLEPIG_GRAMMAR_VERSIONS, the extraction-cache identity, from
//! the workspace lockfile (`docs/index-storage-contract.md`).
use std::{env, fs, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let lock_path = manifest.join("../../Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());
    let lock = fs::read_to_string(&lock_path)
        .unwrap_or_else(|error| panic!("read workspace {}: {error}", lock_path.display()));
    let versions = grammar_versions(&lock);
    assert!(
        versions.contains("tree-sitter="),
        "the workspace lockfile names no tree-sitter package"
    );
    println!("cargo:rustc-env=TRUFFLEPIG_GRAMMAR_VERSIONS={versions}");
}

/// `name=version` of every locked `tree-sitter*` package, sorted and
/// `;`-joined; extraction caches key on these, not the whole lockfile.
fn grammar_versions(lock: &str) -> String {
    let mut versions = Vec::new();
    let mut name = None;
    for line in lock.lines() {
        if line == "[[package]]" {
            name = None;
        } else if let Some(value) = line.strip_prefix("name = ") {
            name = Some(value.trim_matches('"'));
        } else if let (Some(package), Some(value)) = (name, line.strip_prefix("version = "))
            && package.starts_with("tree-sitter")
        {
            versions.push(format!("{package}={}", value.trim_matches('"')));
        }
    }
    versions.sort();
    versions.join(";")
}

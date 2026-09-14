use super::extract;

#[test]
fn pinned_queries_compile() {
    for path in ["a.rs", "a.ts", "a.tsx", "a.js", "a.luau"] {
        let (grammar, source) = super::syntax::grammar(path);
        tree_sitter::Query::new(&grammar, source).unwrap();
    }
}

#[test]
fn rust_definitions_and_direct_calls_preserve_original_bytes() {
    let source = b"\xef\xbb\xbffn helper(value: i32) -> i32 { value }\r\nfn caller() { let value = helper(3); value; }";
    let extracted = extract("a.rs", source);
    assert_eq!(extracted.status, "complete");
    let helper = extracted
        .definitions
        .iter()
        .position(|d| d.name == "helper")
        .unwrap();
    assert_eq!(extracted.definitions[helper].start, 3);
    let call = extracted
        .occurrences
        .iter()
        .find(|o| o.name == "helper" && o.role == "call")
        .unwrap();
    assert_eq!(call.target, Some(helper));
    assert_eq!(&source[call.start..call.end], b"helper");
    assert!(
        extracted
            .occurrences
            .iter()
            .filter(|o| o.name == "value" && o.role == "read")
            .all(|o| o.target.is_some())
    );
}

#[test]
fn rust_trait_methods_and_guessed_receivers_remain_distinct() {
    let source = b"trait A { fn run(&self); } trait B { fn run(&self); } struct S; impl A for S { fn run(&self) {} } impl B for S { fn run(&self) {} } fn f(s: S) { s.run(); }";
    let extracted = extract("a.rs", source);
    let definitions: Vec<_> = extracted
        .definitions
        .iter()
        .filter(|d| d.name == "run")
        .collect();
    assert_eq!(definitions.len(), 4);
    assert_ne!(definitions[2].container, definitions[3].container);
    let call = extracted
        .occurrences
        .iter()
        .find(|o| o.name == "run" && o.role == "call")
        .unwrap();
    assert_eq!(call.target, None);
    assert_eq!(call.candidates.len(), 4);
}

#[test]
fn typescript_arrows_fields_overloads_imports_and_shadowing() {
    let source = br#"import { readFile as read } from "node:fs";
function f(x: string): string;
function f(x: number): number;
function f(x: any) { return x; }
const arrow = (value: number) => value;
class C { field = 1; method() { return this.field; } }
function caller() { const x = 1; { const x = 2; arrow(x); } read("x"); f(1); }
"#;
    let extracted = extract("a.ts", source);
    assert_eq!(extracted.status, "complete");
    assert_eq!(
        extracted
            .definitions
            .iter()
            .filter(|d| d.name == "f")
            .count(),
        3
    );
    assert!(
        extracted
            .definitions
            .iter()
            .any(|d| d.name == "arrow" && d.kind == "function")
    );
    assert!(
        extracted
            .definitions
            .iter()
            .any(|d| d.name == "field" && d.kind == "field")
    );
    assert!(
        extracted
            .occurrences
            .iter()
            .any(|o| o.name == "read" && o.role == "call" && o.target.is_some())
    );
    assert!(
        extracted
            .occurrences
            .iter()
            .any(|o| o.name == "node:fs" && o.role == "import_path")
    );
    assert!(extracted.occurrences.iter().any(|o| o.name == "f"
        && o.role == "call"
        && o.target.is_none()
        && o.candidates.len() == 3));
}

#[test]
fn luau_local_bindings_requires_and_exports_are_observed() {
    let source = br#"local dep = require("@shared/thing")
local function twice(value: number): number
    return value + value
end
local exports = { twice = twice }
local result = twice(2)
return exports
"#;
    let extracted = extract("a.luau", source);
    assert_eq!(extracted.status, "complete");
    assert!(
        extracted
            .definitions
            .iter()
            .any(|d| d.name == "twice" && d.kind == "function")
    );
    assert!(
        extracted
            .occurrences
            .iter()
            .any(|o| o.name == "@shared/thing" && o.role == "require")
    );
    assert!(
        extracted
            .occurrences
            .iter()
            .any(|o| o.name == "twice" && o.role == "call" && o.target.is_some())
    );
    assert!(
        extracted
            .relationships
            .iter()
            .any(|r| r.kind == "export_candidate")
    );
    assert!(
        extracted
            .occurrences
            .iter()
            .filter(|o| o.name == "value" && o.role == "read")
            .all(|o| o.target.is_some())
    );
}

#[test]
fn comments_strings_and_broken_encoding_do_not_create_references() {
    let extracted = extract(
        "a.js",
        b"function real() {} // imaginary()\nconst x = 'imaginary'; real();",
    );
    assert!(!extracted.occurrences.iter().any(|o| o.name == "imaginary"));
    let invalid = extract("a.rs", b"fn x() {}\xff");
    assert_eq!(invalid.status, "invalid_encoding");
    assert!(invalid.definitions.is_empty());
}

#[test]
fn cancellation_publishes_no_structural_facts() {
    let source = "fn f() { let x = 1; }\n".repeat(10_000);
    let extracted =
        super::syntax::extract_with_deadline("a.rs", source.as_bytes(), std::time::Duration::ZERO);
    assert_eq!(extracted.status, "cancelled");
    assert!(extracted.definitions.is_empty());
    assert!(extracted.occurrences.is_empty());
    assert!(extracted.relationships.is_empty());
}

#[test]
fn conditional_definitions_and_late_bindings_never_false_resolve() {
    let rust = extract(
        "a.rs",
        b"#[cfg(feature=\"x\")] fn maybe() {} fn run() { maybe(); }",
    );
    assert!(
        rust.occurrences
            .iter()
            .any(|o| o.name == "maybe" && o.role == "call" && o.target.is_none())
    );
    let js = extract("a.js", b"let x = 0; function run(x) { return x; } late(); const late = () => 1; const id = arg => arg;");
    assert!(
        js.occurrences
            .iter()
            .any(|o| o.name == "late" && o.role == "call" && o.target.is_none())
    );
    for name in ["x", "arg"] {
        let reference = js
            .occurrences
            .iter()
            .find(|o| o.name == name && o.role == "read")
            .unwrap();
        assert_eq!(js.definitions[reference.target.unwrap()].kind, "parameter");
    }
}

#[test]
fn rust_import_aliases_and_grouped_names_are_bindings() {
    let extracted = extract("a.rs", b"use std::collections::{HashMap, HashSet as Set}; fn f() { let x: HashMap<i32,i32>; let y: Set<i32>; }");
    for name in ["HashMap", "Set"] {
        assert!(
            extracted
                .definitions
                .iter()
                .any(|d| d.name == name && d.kind == "import")
        );
        assert!(
            extracted
                .occurrences
                .iter()
                .any(|o| o.name == name && o.role == "type" && o.target.is_some())
        );
    }
}

#[test]
fn luau_static_instance_requires_retain_provenance() {
    let extracted = extract(
        "a.luau",
        b"local Module = require(game.ReplicatedStorage.Shared.Module)",
    );
    assert!(
        extracted
            .occurrences
            .iter()
            .any(|o| o.name == "game.ReplicatedStorage.Shared.Module"
                && o.role == "require"
                && o.provenance == "rojo_instance_path"
                && o.target.is_none())
    );
}

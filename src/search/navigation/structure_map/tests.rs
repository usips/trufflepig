use super::*;
use crate::search::tests::fixture;

fn outline(store: &Store, path: &str) -> Vec<String> {
    map(store, path)
        .unwrap()
        .hits
        .into_iter()
        .map(|hit| format!("{} {}", hit.kind, hit.name))
        .collect()
}

#[test]
fn file_map_lists_code_first_and_folds_fields_modules_and_ffi() {
    let (_root, _cache, store) = fixture(&[(
        "src/lib.rs",
        b"mod child;\nuse std::fmt;\npub struct Bucket {\n    tokens: u8,\n}\nextern \"C\" {\n    fn ffi_call();\n}\nimpl Bucket {\n    pub fn refill(&self) {}\n}\nconst CAP: u8 = 3;\n",
    )]);
    assert_eq!(
        outline(&store, "src/lib.rs"),
        [
            "struct Bucket",
            "method refill",
            "constant CAP",
            "module child",
            "field tokens",
        ]
    );
}

#[test]
fn directory_map_gives_each_file_one_row_of_top_level_names() {
    let many: String = (0..30)
        .map(|i| format!("pub fn function_number_{i}() {{}}\n"))
        .collect();
    let (_root, _cache, store) = fixture(&[
        (
            "src/a.rs",
            b"fn helper() {}\npub struct Alpha;\nimpl Alpha { fn method() {} }\n",
        ),
        ("src/b.rs", many.as_bytes()),
        ("src/notes.md", b"# notes\n"),
    ]);
    let rows = outline(&store, "src/");
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0], "file Alpha, helper");
    assert!(
        rows[1].starts_with("file function_number_0, function_number_1,"),
        "{rows:?}"
    );
    assert!(rows[1].ends_with(" +26"), "{rows:?}");
    assert_eq!(rows[2], "file ");
    let set = map(&store, "src/").unwrap();
    assert_eq!((set.hits[0].start_line, set.hits[0].end_line), (1, 3));
}

#[test]
fn missing_map_path_names_the_nearest_indexed_paths() {
    let (_root, _cache, store) = fixture(&[
        ("crates/core/src/luau/payload_heap.rs", b"fn heap() {}\n"),
        ("crates/core/src/script/host.rs", b"fn host() {}\n"),
    ]);
    let error = map(&store, "crates/core/src/script/contract/payload_heap.rs")
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "no_indexed_path: no indexed file under `crates/core/src/script/contract/payload_heap.rs`; nearest: crates/core/src/luau/payload_heap.rs"
    );
    let error = map(&store, "crates/core/src/scripts/")
        .unwrap_err()
        .to_string();
    assert!(
        error.ends_with("nearest: crates/core/src/script/, crates/core/src/luau/"),
        "{error}"
    );
}

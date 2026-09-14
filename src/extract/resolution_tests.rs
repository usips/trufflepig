use super::extract;

#[test]
fn destructured_javascript_bindings_shadow_outer_values() {
    let source = b"const value=0; function f({ key: value = 1, ...rest }) { return value; }";
    let extracted = extract("a.js", source);
    let usage = extracted
        .occurrences
        .iter()
        .find(|o| o.name == "value" && o.role == "read")
        .unwrap();
    assert_eq!(
        extracted.definitions[usage.target.unwrap()].kind,
        "parameter"
    );
}

#[test]
fn javascript_tdz_and_var_scopes_do_not_resolve_outer_bindings() {
    let tdz = extract(
        "a.js",
        b"const x=1; function f(){ console.log(x); let x=2; }",
    );
    let read = tdz
        .occurrences
        .iter()
        .find(|o| o.name == "x" && o.role == "read")
        .unwrap();
    assert!(read.target.is_none());
    let var = extract(
        "a.js",
        b"const x=1; function f(){ if(true){var x=2;} return x; }",
    );
    let read = var
        .occurrences
        .iter()
        .find(|o| o.name == "x" && o.role == "read")
        .unwrap();
    assert!(var.definitions[read.target.unwrap()].start > 20);
}

#[test]
fn rust_nested_functions_cannot_capture_outer_locals() {
    let extracted = extract("a.rs", b"fn f(){let x=1; fn g(){drop(x);} let c = || x; }");
    let reads: Vec<_> = extracted
        .occurrences
        .iter()
        .filter(|o| o.name == "x" && o.role == "read")
        .collect();
    assert!(reads[0].target.is_none());
    assert!(reads[1].target.is_some());
}

#[test]
fn locally_shadowed_require_is_visible_as_uncertain() {
    let extracted = extract("a.js", b"function f(require) { return require('./x'); }");
    assert!(
        extracted.occurrences.iter().any(|o| o.name == "./x"
            && o.provenance == "shadowed_require_path"
            && o.target.is_none())
    );
}

#[test]
fn type_and_macro_namespaces_do_not_resolve_to_values() {
    for (path, source, name) in [
        (
            "a.rs",
            "type Thing=u8; fn f(){ let Thing=1; let _: Thing=0; }",
            "Thing",
        ),
        (
            "a.ts",
            "interface Item {} const Item=1; let x: Item;",
            "Item",
        ),
    ] {
        let extracted = extract(path, source.as_bytes());
        let usage = extracted
            .occurrences
            .iter()
            .find(|o| o.name == name && o.role == "type")
            .unwrap();
        assert!(matches!(
            extracted.definitions[usage.target.unwrap()].kind.as_str(),
            "type" | "interface"
        ));
    }
    let macros = extract("a.rs", b"fn format() {} fn f(){ format!(\"x\"); }");
    assert!(
        macros
            .occurrences
            .iter()
            .any(|o| o.name == "format" && o.role == "macro" && o.target.is_none())
    );
}

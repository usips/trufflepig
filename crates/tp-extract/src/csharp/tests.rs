use super::extract;

#[test]
fn grammar_query_compiles() {
    let language: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    tree_sitter::Query::new(&language, super::REFERENCES).unwrap();
}

#[test]
fn unsupported_list_slice_property_pattern_keeps_references_unresolved() {
    let source = br#"class PatternProbe {
    static bool Matches(Value value) =>
        value is [Value.CountValue { Count: var cnt }, .. { } multis];
}"#;
    let extracted = extract(source);
    assert_eq!(extracted.status, "parse_error");
    let references: Vec<_> = extracted
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role != "declaration")
        .collect();
    assert!(!references.is_empty());
    assert!(
        references
            .iter()
            .all(|occurrence| occurrence.target.is_none())
    );
}

#[test]
fn declarations_keep_containers_and_original_byte_spans() {
    let source = br#"namespace BTCPay.Server {
public partial class Invoice : Entity {
    private int _amount;
    public Invoice(int amount) { _amount = amount; var copied = amount; }
    public int Amount { get; set; }
    public void Save(int id) { var local = id; Save(id); System.Func<int, int> mapper = input => input; }
    public void Use() { Invoice item = new Invoice(1); }
}
}
namespace BTCPay.Server { public partial class Invoice {} }
"#;
    let extracted = extract(source);
    assert_eq!(extracted.status, "complete");
    assert_eq!(
        extracted
            .definitions
            .iter()
            .filter(|definition| definition.name == "Invoice" && definition.kind == "class")
            .count(),
        2,
        "partial declarations stay distinct"
    );
    for (name, kind) in [
        ("_amount", "field"),
        ("Amount", "property"),
        ("Invoice", "constructor"),
        ("Save", "method"),
        ("amount", "parameter"),
        ("copied", "local"),
        ("local", "local"),
        ("input", "parameter"),
    ] {
        assert!(
            extracted
                .definitions
                .iter()
                .any(|definition| definition.name == name && definition.kind == kind),
            "missing {kind} {name}"
        );
    }
    let class = extracted
        .definitions
        .iter()
        .find(|definition| definition.name == "Invoice" && definition.kind == "class")
        .unwrap();
    assert_eq!(class.container.as_deref(), Some("BTCPay.Server"));
    assert!(source[class.start..class.end].starts_with(b"public partial class Invoice"));
    let partial_type_references: Vec<_> = extracted
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.name == "Invoice" && occurrence.role == "type")
        .collect();
    assert!(!partial_type_references.is_empty());
    let partial_definitions: Vec<_> = extracted
        .definitions
        .iter()
        .enumerate()
        .filter(|(_, definition)| definition.name == "Invoice" && definition.kind == "class")
        .map(|(index, _)| index)
        .collect();
    assert!(partial_type_references.iter().all(|occurrence| {
        occurrence.target.is_none()
            && partial_definitions
                .iter()
                .all(|definition| occurrence.candidates.contains(definition))
    }));
    let local = extracted
        .occurrences
        .iter()
        .find(|occurrence| occurrence.name == "local" && occurrence.role == "declaration")
        .unwrap();
    assert_eq!(&source[local.start..local.end], b"local");
    assert!(extracted.definitions.iter().any(|definition| {
        definition.name == "Save"
            && definition.container.as_deref() == Some("BTCPay.Server.Invoice")
    }));
    let lambda_read = extracted
        .occurrences
        .iter()
        .find(|occurrence| occurrence.name == "input" && occurrence.role == "read")
        .unwrap();
    assert_eq!(
        extracted.definitions[lambda_read.target.unwrap()].kind,
        "parameter"
    );
}

#[test]
fn overloads_receivers_and_inheritance_remain_candidates() {
    let source = br#"namespace Sample {
class Base { protected void Inherited() {} }
class Store : Base {
    void Save(int value) {}
    void Save(string value) {}
    void Run(Store receiver) { Save(1); receiver.Save(1); Inherited(); External.Run(); }
}
}"#;
    let extracted = extract(source);
    assert_eq!(extracted.status, "complete");
    let inheritance = extracted
        .occurrences
        .iter()
        .find(|occurrence| occurrence.name == "Base" && occurrence.role == "type")
        .unwrap();
    assert_eq!(inheritance.target, None);
    assert_eq!(inheritance.candidates.len(), 1);
    assert_eq!(inheritance.provenance, "unresolved_inheritance");

    let saves: Vec<_> = extracted
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.name == "Save" && occurrence.role == "call")
        .collect();
    assert_eq!(saves.len(), 2);
    assert!(saves.iter().all(|occurrence| occurrence.target.is_none()));
    assert!(
        saves
            .iter()
            .all(|occurrence| occurrence.candidates.len() == 2)
    );
    let inherited_call = extracted
        .occurrences
        .iter()
        .find(|occurrence| occurrence.name == "Inherited" && occurrence.role == "call")
        .unwrap();
    assert_eq!(inherited_call.target, None);
    assert!(!inherited_call.candidates.is_empty());
    let external = extracted
        .occurrences
        .iter()
        .find(|occurrence| occurrence.name == "Run" && occurrence.role == "call")
        .unwrap();
    assert_eq!(external.target, None);
}

#[test]
fn malformed_and_invalid_source_never_resolve_references() {
    let malformed = extract(b"class Broken { void Call() { Missing(); }");
    assert_eq!(malformed.status, "parse_error");
    assert!(
        malformed
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role != "declaration")
            .all(|occurrence| occurrence.target.is_none())
    );

    let invalid = extract(b"class Broken {}\xff");
    assert_eq!(invalid.status, "invalid_encoding");
    assert!(invalid.definitions.is_empty());
    assert!(invalid.occurrences.is_empty());
}

#[test]
fn foreach_binding_is_visible_only_inside_its_body() {
    let source = b"class C { void Use(int value) {} void Run() { foreach (var item in item) Use(item); Use(item); } }";
    let extracted = extract(source);
    assert_eq!(extracted.status, "complete");
    let items: Vec<_> = extracted
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.name == "item" && occurrence.role == "read")
        .collect();
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0].target, None,
        "the collection expression precedes the binding"
    );
    assert!(
        items[1].target.is_some(),
        "the foreach body sees its binding"
    );
    assert_eq!(
        items[2].target, None,
        "the binding ends with the foreach body"
    );
}

#[test]
fn conditional_definitions_do_not_resolve_outside_preprocessor_branches() {
    let source = br#"class C {
    void Run() {
#if DEBUG
        int maybe = 1;
#endif
        maybe = 2;
    }
}"#;
    let extracted = extract(source);
    assert_eq!(extracted.status, "complete");
    let reference = extracted
        .occurrences
        .iter()
        .find(|occurrence| occurrence.name == "maybe" && occurrence.role == "read")
        .unwrap();
    assert_eq!(reference.target, None);
    assert!(!reference.candidates.is_empty());
}

#[test]
fn cancellation_and_deep_trees_publish_no_structural_facts() {
    let cancelled =
        super::extract_with_deadline(b"class A { void F() {} }", std::time::Duration::ZERO);
    assert_eq!(cancelled.status, "cancelled");
    assert!(cancelled.definitions.is_empty());

    let mut source = String::from("class A { void F() {");
    for _ in 0..300 {
        source.push_str("if (true) {");
    }
    source.push_str("int value = 1;");
    for _ in 0..300 {
        source.push('}');
    }
    source.push_str("} }");
    let deep = extract(source.as_bytes());
    assert_eq!(deep.status, "depth_limit");
    assert!(deep.definitions.is_empty());
}

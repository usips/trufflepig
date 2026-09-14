use super::extract;

#[test]
fn dm_nested_types_procedures_verbs_and_bindings() {
    let source = b"/obj\n\twidget\n\t\tproc\n\t\t\tactivate(value)\n\t\t\t\tvar/copy = value\n\t\t\t\treturn copy\n\t\tverb\n\t\t\tinspect()\n\t\t\t\tactivate(1)\n";
    let output = extract(source);
    let activate = output
        .definitions
        .iter()
        .position(|d| d.name == "activate")
        .unwrap();
    assert_eq!(
        output.definitions[activate].container.as_deref(),
        Some("/obj/widget")
    );
    assert!(
        output
            .definitions
            .iter()
            .any(|d| d.name == "inspect" && d.kind == "verb")
    );
    for name in ["value", "copy"] {
        let reference = output.occurrences.iter().find(|o| o.name == name).unwrap();
        assert_eq!(output.definitions[reference.target.unwrap()].name, name);
    }
    let call = output
        .occurrences
        .iter()
        .find(|o| o.name == "activate")
        .unwrap();
    assert_eq!(call.target, None);
    assert_eq!(call.candidates, vec![activate]);
}

#[test]
fn dm_preserves_bom_crlf_invalid_bytes_and_ignores_fake_headers() {
    let source = b"\xef\xbb\xbf/* /obj/fake() */\r\n/obj/real()\r\n\tvar/text = \"/obj/also_fake()\" // \xff\r\n\treturn text\r\n";
    let output = extract(source);
    let definition = output
        .definitions
        .iter()
        .find(|d| d.name == "real")
        .unwrap();
    assert_eq!(
        &source[definition.start..definition.start + 11],
        b"/obj/real()"
    );
    assert!(!output.definitions.iter().any(|d| d.name.contains("fake")));
    let reference = output
        .occurrences
        .iter()
        .find(|o| o.name == "text")
        .unwrap();
    assert_eq!(&source[reference.start..reference.end], b"text");
    assert!(reference.target.is_some());
}

#[test]
fn dm_braces_and_multiline_strings_preserve_scope() {
    let output = extract(b"/obj/widget {\nproc/activate(value) {\nreturn value\n}\nverb/inspect() {\nvar/text = {\"\n/obj/fake()\n\"}\n}\n}\n");
    assert!(
        output
            .definitions
            .iter()
            .any(|d| d.name == "activate" && d.container.as_deref() == Some("/obj/widget"))
    );
    assert!(
        output
            .definitions
            .iter()
            .any(|d| d.name == "inspect" && d.kind == "verb")
    );
    assert!(!output.definitions.iter().any(|d| d.name == "fake"));
}

#[test]
fn dm_same_type_override_parent_is_ordered_but_conditional_is_ambiguous() {
    let output = extract(b"/obj/proc/run()\n\treturn 1\n/obj/run()\n\treturn ..()\n");
    let parent = output
        .occurrences
        .iter()
        .find(|o| o.role == "parent_call")
        .unwrap();
    assert_eq!(parent.target, Some(0));
    assert!(
        output
            .occurrences
            .iter()
            .any(|o| o.role == "override" && o.target == Some(0))
    );
    let conditional = extract(
        b"#ifdef FEATURE\n/obj/proc/run()\n\treturn 1\n#endif\n/obj/run()\n\treturn ..()\n",
    );
    let parent = conditional
        .occurrences
        .iter()
        .find(|o| o.role == "parent_call")
        .unwrap();
    assert_eq!(parent.target, None);
    assert!(!parent.candidates.is_empty());
}

#[test]
fn dm_explicit_inheritance_is_candidate_without_project_order() {
    let output = extract(b"/datum/base/proc/run()\n\treturn 1\n/obj/child\n\tparent_type = /datum/base\n\trun()\n\t\treturn ..()\n");
    let parent = output
        .occurrences
        .iter()
        .find(|o| o.role == "parent_call")
        .unwrap();
    assert_eq!(parent.target, None);
    assert_eq!(parent.candidates, vec![0]);
    assert!(
        output
            .occurrences
            .iter()
            .any(|o| o.role == "parent_type" && o.name == "/datum/base")
    );
    assert!(output.relationships.iter().any(|r| r.kind == "inheritance"));
}

#[test]
fn dm_macros_includes_and_signals_have_source_evidence() {
    let source = b"#define COMSIG_HIT \"hit\"\n#if ENABLED\n#include \"feature.dm\"\n#endif\n/obj/proc/run()\n\tSEND_SIGNAL(src, COMSIG_HIT)\n\tRegisterSignal(src, \"other\", PROC_REF(run))\n";
    let output = extract(source);
    assert!(
        output
            .definitions
            .iter()
            .any(|d| d.kind == "macro" && d.name == "COMSIG_HIT")
    );
    assert!(
        output
            .occurrences
            .iter()
            .any(|o| o.role == "include" && o.name == "feature.dm")
    );
    assert!(
        output
            .occurrences
            .iter()
            .any(|o| o.role == "signal" && o.name == "COMSIG_HIT")
    );
    assert!(
        output
            .occurrences
            .iter()
            .any(|o| o.role == "signal" && o.name == "other")
    );
    assert!(output.relationships.iter().any(|r| r.kind == "signal_call"));
    assert!(
        output
            .occurrences
            .iter()
            .all(|o| o.end <= source.len() && o.start < o.end)
    );
}

#[test]
fn dm_block_local_does_not_escape_its_scope() {
    let output =
        extract(b"/proc/run()\n\tif(TRUE)\n\t\tvar/item = 1\n\t\tconsume(item)\n\tconsume(item)\n");
    let references: Vec<_> = output
        .occurrences
        .iter()
        .filter(|o| o.name == "item")
        .collect();
    assert_eq!(references.len(), 2);
    assert!(references[0].target.is_some());
    assert!(references[1].target.is_none());
}

#[test]
fn dm_brace_local_does_not_escape_its_scope() {
    let output =
        extract(b"/proc/run() {\nif(TRUE) {\nvar/item = 1\nconsume(item)\n}\nconsume(item)\n}\n");
    let references: Vec<_> = output
        .occurrences
        .iter()
        .filter(|o| o.name == "item")
        .collect();
    assert_eq!(references.len(), 2);
    assert!(references[0].target.is_some());
    assert!(references[1].target.is_none());
}

#[test]
fn dm_unterminated_string_does_not_expose_fake_declarations() {
    let output = extract(b"/obj/real()\n\tvar/text = \"broken\n/obj/fake()\n\"\n");
    assert_eq!(output.status, "recovered_incomplete");
    assert!(!output.definitions.iter().any(|d| d.name == "fake"));
}

#[test]
fn dm_unterminated_comment_and_oversized_line_report_incompleteness() {
    assert_eq!(
        extract(b"/obj/real\n/* never closes").status,
        "recovered_incomplete"
    );
    let source = format!("/obj/{}\n", "a".repeat(16 * 1024));
    let output = extract(source.as_bytes());
    assert_eq!(output.status, "recovered_incomplete");
    assert!(output.definitions.is_empty());
}

#[test]
fn dm_candidate_fanout_and_fact_collection_are_bounded() {
    let mut source = String::with_capacity(600_000);
    for _ in 0..10_000 {
        source.push_str("/proc/repeated()\n\t..()\n");
    }
    source.push_str("/proc/caller()\n");
    for _ in 0..15_000 {
        source.push_str("\trepeated()\n");
    }
    let output = extract(source.as_bytes());
    let facts = output.definitions.len() + output.occurrences.len() + output.relationships.len();
    assert!(facts <= super::MAX_FACTS, "published {facts} facts");
    assert_eq!(output.status, "recovered_incomplete");
    assert!(
        output
            .occurrences
            .iter()
            .all(|o| o.candidates.len() <= super::MAX_CANDIDATES)
    );
    assert!(
        output
            .occurrences
            .iter()
            .any(|o| o.provenance.contains("candidates-truncated"))
    );
    assert!(output.occurrences.iter().all(|o| o.target.is_none()));
    assert!(output.relationships.iter().all(|r| r.target.is_none()));
    assert!(
        output
            .occurrences
            .iter()
            .flat_map(|o| &o.candidates)
            .all(|&i| i < output.definitions.len())
    );
}

#[test]
fn dm_name_candidates_are_bounded_even_without_fact_truncation() {
    let source = format!(
        "{}/proc/caller()\n\trepeated()\n",
        "/proc/repeated()\n".repeat(100)
    );
    let output = extract(source.as_bytes());
    let call = output
        .occurrences
        .iter()
        .find(|o| o.role == "call")
        .unwrap();
    assert_eq!(call.candidates.len(), super::MAX_CANDIDATES);
    assert!(call.provenance.contains("candidates-truncated"));
    assert_eq!(output.status, "recovered");
}

#[test]
fn dm_incomplete_and_conditional_evidence_has_no_static_targets() {
    for source in [
        "/proc/run(value)\n\treturn value\n/* incomplete",
        "/proc/run(value)\n#if FEATURE\n\treturn value\n#endif\n",
    ] {
        let output = extract(source.as_bytes());
        let reference = output
            .occurrences
            .iter()
            .find(|o| o.name == "value")
            .unwrap();
        assert!(reference.target.is_none());
    }
}

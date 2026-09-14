use tree_sitter::Node;

use super::bindings::text;
use super::{Extraction, Occurrence, Relationship};

pub(super) fn collect(node: Node<'_>, source: &[u8], result: &mut Extraction) {
    if node.kind() == "import_statement"
        && let Some(module) = node.child_by_field_name("source")
    {
        module_occurrence(module, "import_path", source, result);
    }
    if matches!(node.kind(), "function_call" | "call_expression") {
        let callee = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("function"));
        if callee.is_some_and(|name| text(name, source) == "require")
            && let Some(args) = node.child_by_field_name("arguments")
        {
            if let Some(module) = args
                .named_child(0)
                .filter(|child| matches!(child.kind(), "string" | "string_literal"))
            {
                module_occurrence(module, "require", source, result);
            }
            if result.language == "luau"
                && let Some(module) = args
                    .named_child(0)
                    .filter(|child| child.kind() == "dot_index_expression")
            {
                let name = text(module, source);
                if name.starts_with("game.")
                    && name.split('.').all(|part| {
                        !part.is_empty()
                            && part
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    })
                {
                    result.occurrences.push(Occurrence {
                        name: name.into(),
                        start: module.start_byte(),
                        end: module.end_byte(),
                        role: "require".into(),
                        target: None,
                        candidates: Vec::new(),
                        provenance: "rojo_instance_path".into(),
                    });
                }
            }
        }
    }
    if matches!(node.kind(), "return_statement" | "export_statement") {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let value = if child.kind() == "expression_list" {
                child.named_child(0).unwrap_or(child)
            } else {
                child
            };
            let name = text(value, source);
            let matches: Vec<_> = result
                .definitions
                .iter()
                .enumerate()
                .filter(|(_, d)| d.name == name && d.start < node.start_byte())
                .map(|(index, _)| index)
                .collect();
            if matches.len() == 1 {
                result.relationships.push(Relationship {
                    source: matches[0],
                    target: None,
                    kind: "export_candidate".into(),
                    evidence_start: value.start_byte(),
                    evidence_end: value.end_byte(),
                    provenance: "observed_return_or_export".into(),
                });
            }
        }
    }
    if matches!(
        node.kind(),
        "comment" | "line_comment" | "block_comment" | "string" | "string_literal"
    ) {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, source, result);
    }
}

fn module_occurrence(node: Node<'_>, role: &str, source: &[u8], result: &mut Extraction) {
    let raw = text(node, source);
    let Some(name) = raw
        .strip_prefix(['\'', '"'])
        .and_then(|value| value.strip_suffix(['\'', '"']))
    else {
        return;
    };
    let shadowed = role == "require"
        && node
            .parent()
            .and_then(|arguments| arguments.parent())
            .and_then(|call| {
                call.child_by_field_name("name")
                    .or_else(|| call.child_by_field_name("function"))
            })
            .is_some_and(|callee| {
                result.occurrences.iter().any(|occurrence| {
                    occurrence.start == callee.start_byte() && occurrence.target.is_some()
                })
            });
    result.occurrences.push(Occurrence {
        name: name.into(),
        start: node.start_byte(),
        end: node.end_byte(),
        role: role.into(),
        target: None,
        candidates: Vec::new(),
        provenance: if shadowed {
            "shadowed_require_path"
        } else {
            "literal_module_path"
        }
        .into(),
    });
}

use std::collections::HashMap;
use std::ops::Range;

use tree_sitter::Node;

use super::super::binding_index::BindingIndex;
use super::super::bindings::text;
use super::super::{Extraction, Occurrence, Relationship};
use super::scopes::ancestor;

pub(super) fn observe(
    node: Node<'_>,
    source: &[u8],
    bindings: &BindingIndex,
    declarations: &[Range<usize>],
    owners: &HashMap<usize, usize>,
    result: &mut Extraction,
) {
    let insertion = declarations.partition_point(|range| range.start <= node.start_byte());
    if insertion > 0 && node.end_byte() <= declarations[insertion - 1].end {
        return;
    }
    if ancestor(node, "comment") {
        return;
    }
    let inherited = ancestor(node, "base_list") || ancestor(node, "record_base");
    let import = ancestor(node, "using_directive") || ancestor(node, "extern_alias_directive");
    let call = call_name(node);
    let role = if import {
        "import"
    } else if call {
        "call"
    } else if type_reference(node) || inherited {
        "type"
    } else {
        "read"
    };
    let name = text(node, source);
    if name.is_empty() {
        return;
    }
    let qualified = ancestor(node, "qualified_name")
        || ancestor(node, "alias_qualified_name")
        || ancestor(node, "member_access_expression")
        || ancestor(node, "member_binding_expression");
    let namespace = if role == "type" { 2 } else { 1 };
    let (mut target, candidates, limited) = bindings.resolve(name, node, namespace);
    if qualified || inherited || import || result.status != "complete" {
        target = None;
    }
    let provenance = if inherited {
        "unresolved_inheritance"
    } else if target.is_some() {
        if call {
            "direct_lexical_call"
        } else {
            "lexical_binding"
        }
    } else if limited {
        "candidate_name_truncated"
    } else if candidates.is_empty() {
        "observed_syntax"
    } else {
        "candidate_name"
    };
    if call {
        let mut parent = node.parent();
        while let Some(ancestor) = parent {
            if let Some(source_definition) = owners.get(&ancestor.start_byte()) {
                result.relationships.push(Relationship {
                    source: *source_definition,
                    target,
                    kind: "calls".into(),
                    evidence_start: node.start_byte(),
                    evidence_end: node.end_byte(),
                    provenance: provenance.into(),
                });
                break;
            }
            parent = ancestor.parent();
        }
    }
    result.occurrences.push(Occurrence {
        name: name.into(),
        start: node.start_byte(),
        end: node.end_byte(),
        role: role.into(),
        target,
        candidates: if target.is_some() {
            Vec::new()
        } else {
            candidates
        },
        provenance: provenance.into(),
    });
}

fn call_name(node: Node<'_>) -> bool {
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if ancestor.kind() == "invocation_expression" {
            let Some(function) = ancestor.child_by_field_name("function") else {
                return false;
            };
            return terminal_name(function).is_some_and(|name| name.id() == node.id());
        }
        parent = ancestor.parent();
    }
    false
}

fn type_reference(node: Node<'_>) -> bool {
    ancestor(node, "type_pattern")
        || ancestor(node, "type_parameter_constraint")
        || ancestor(node, "type_argument_list")
        || in_field(node, "type")
        || in_field(node, "returns")
}

fn in_field(node: Node<'_>, field_name: &str) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        if parent
            .child_by_field_name(field_name)
            .is_some_and(|field| field.id() == child.id())
        {
            return true;
        }
        child = parent;
    }
    false
}

fn terminal_name<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    match node.kind() {
        "identifier" => Some(node),
        "generic_name" => node.named_child(0),
        "member_access_expression" | "member_binding_expression" | "qualified_name" => {
            terminal_name(node.child_by_field_name("name")?)
        }
        "alias_qualified_name" => terminal_name(node.child_by_field_name("name")?),
        "parenthesized_expression" | "invocation_expression" => terminal_name(
            node.child_by_field_name("function")
                .or_else(|| node.named_child(0))?,
        ),
        _ => None,
    }
}

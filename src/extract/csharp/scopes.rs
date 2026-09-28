use std::ops::Range;

use tree_sitter::Node;

use super::super::bindings::text;

pub(super) fn binding_scope(node: Node<'_>, kind: &str) -> Range<usize> {
    if kind == "local" && node.kind() == "foreach_statement" {
        return node
            .child_by_field_name("body")
            .map_or_else(|| node.byte_range(), |body| body.byte_range());
    }
    let mut parent = node.parent();
    while let Some(candidate) = parent {
        let matches = if matches!(kind, "parameter") {
            matches!(
                candidate.kind(),
                "method_declaration"
                    | "constructor_declaration"
                    | "local_function_statement"
                    | "lambda_expression"
                    | "anonymous_method_expression"
                    | "accessor_declaration"
                    | "indexer_declaration"
                    | "record_declaration"
                    | "delegate_declaration"
            )
        } else if matches!(
            kind,
            "field" | "property" | "event" | "method" | "constructor" | "enum_member"
        ) {
            matches!(
                candidate.kind(),
                "declaration_list" | "enum_member_declaration_list" | "compilation_unit"
            )
        } else if matches!(
            kind,
            "class" | "interface" | "struct" | "record" | "enum" | "delegate" | "namespace"
        ) {
            matches!(candidate.kind(), "declaration_list" | "compilation_unit")
        } else if kind == "type_parameter" {
            matches!(
                candidate.kind(),
                "method_declaration"
                    | "class_declaration"
                    | "struct_declaration"
                    | "record_declaration"
                    | "interface_declaration"
                    | "delegate_declaration"
            )
        } else {
            matches!(
                candidate.kind(),
                "block"
                    | "for_statement"
                    | "foreach_statement"
                    | "catch_clause"
                    | "switch_section"
                    | "compilation_unit"
                    | "global_statement"
            )
        };
        if matches {
            return candidate.byte_range();
        }
        parent = candidate.parent();
    }
    node.byte_range()
}

pub(super) fn identifier_tokens<'tree>(node: Node<'tree>, result: &mut Vec<Node<'tree>>) {
    if node.kind() == "identifier" {
        result.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        identifier_tokens(child, result);
    }
}

pub(super) fn display_name(node: Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "qualified_name" => match (
            node.child_by_field_name("qualifier"),
            node.child_by_field_name("name"),
        ) {
            (Some(left), Some(right)) => format!(
                "{}.{}",
                display_name(left, source),
                display_name(right, source)
            ),
            _ => text(node, source).trim().into(),
        },
        "alias_qualified_name" => match (
            node.child_by_field_name("alias"),
            node.child_by_field_name("name"),
        ) {
            (Some(left), Some(right)) => {
                format!("{}::{}", text(left, source), display_name(right, source))
            }
            _ => text(node, source).trim().into(),
        },
        _ => text(node, source).trim().into(),
    }
}

pub(super) fn join(parent: Option<String>, child: String) -> String {
    match parent.filter(|value| !value.is_empty()) {
        Some(parent) => format!("{parent}.{child}"),
        None => child,
    }
}

pub(super) fn ancestor(node: Node<'_>, kind: &str) -> bool {
    let mut current = Some(node);
    while let Some(candidate) = current {
        if candidate.kind() == kind {
            return true;
        }
        current = candidate.parent();
    }
    false
}

pub(super) fn has_conditional_ancestor(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(candidate) = current {
        if candidate.kind().starts_with("preproc_") {
            return true;
        }
        current = candidate.parent();
    }
    false
}

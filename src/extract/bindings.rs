use std::ops::Range;

use tree_sitter::Node;

use super::syntax_names::{declaration, name_tokens};
use super::{Definition, Extraction};

pub(super) struct Binding {
    pub definition: usize,
    pub token: Range<usize>,
    pub scope: Range<usize>,
    pub visible: usize,
    pub resolvable: bool,
    pub blocks_before: bool,
    pub function_boundary: Option<usize>,
    pub namespaces: u8,
}

pub(super) fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or("")
}

pub(super) fn scope(node: Node<'_>) -> Range<usize> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if matches!(
            parent.kind(),
            "block"
                | "statement_block"
                | "declaration_list"
                | "class_body"
                | "source_file"
                | "program"
                | "chunk"
                | "function_item"
                | "function_declaration"
                | "function_definition"
                | "function_signature_item"
                | "function_signature"
                | "closure_expression"
                | "arrow_function"
                | "function_expression"
                | "method_definition"
        ) {
            return parent.byte_range();
        }
        ancestor = parent.parent();
    }
    node.byte_range()
}

pub(super) fn function_scope(node: Node<'_>) -> Range<usize> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if matches!(
            parent.kind(),
            "function_item"
                | "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "program"
                | "source_file"
        ) {
            return parent.byte_range();
        }
        ancestor = parent.parent();
    }
    node.byte_range()
}

fn add_binding(
    node: Node<'_>,
    name: Node<'_>,
    kind: &str,
    container: &Option<String>,
    source: &[u8],
    result: &mut Extraction,
    bindings: &mut Vec<Binding>,
) {
    let name_text = text(name, source);
    if name_text.is_empty() {
        return;
    }
    let hoisted = matches!(
        kind,
        "function"
            | "struct"
            | "enum"
            | "trait"
            | "type"
            | "constant"
            | "module"
            | "class"
            | "interface"
            | "macro"
    ) && node.kind() != "variable_declarator"
        && result.language != "luau";
    let is_var = matches!(result.language.as_str(), "javascript" | "typescript")
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "variable_declaration");
    let scope = if is_var {
        function_scope(node)
    } else {
        scope(node)
    };
    let resolvable = !matches!(kind, "method" | "field" | "variant")
        && !matches!(
            name.parent().map(|p| p.kind()),
            Some("dot_index_expression" | "method_index_expression")
        )
        && !conditional(node, source);
    bindings.push(Binding {
        definition: result.definitions.len(),
        token: name.byte_range(),
        visible: if hoisted || is_var || kind == "parameter" {
            scope.start
        } else if kind == "function" {
            node.start_byte()
        } else {
            node.end_byte()
        },
        scope,
        resolvable,
        blocks_before: matches!(result.language.as_str(), "javascript" | "typescript"),
        function_boundary: (result.language == "rust" && matches!(kind, "variable" | "parameter"))
            .then(|| function_scope(node).start),
        namespaces: match kind {
            "type" | "trait" | "interface" => 2,
            "struct" | "enum" | "class" | "module" => 3,
            "macro" => 4,
            "import" => 7,
            _ => 1,
        },
    });
    result.definitions.push(Definition {
        name: name_text.into(),
        kind: kind.into(),
        start: node.start_byte(),
        end: node.end_byte(),
        container: container.clone(),
    });
}

pub(super) fn collect(
    node: Node<'_>,
    source: &[u8],
    container: Option<String>,
    result: &mut Extraction,
    bindings: &mut Vec<Binding>,
) {
    if matches!(
        node.kind(),
        "comment" | "line_comment" | "block_comment" | "string" | "string_literal"
    ) {
        return;
    }
    let mut next_container = container.clone();
    if node.kind() == "impl_item" {
        let header_end = node
            .child_by_field_name("body")
            .map_or(node.end_byte(), |n| n.start_byte());
        next_container = Some(
            String::from_utf8_lossy(&source[node.start_byte()..header_end])
                .trim()
                .into(),
        );
    }
    if let Some((kind, name)) = declaration(node) {
        let mut names = Vec::with_capacity(4);
        name_tokens(name, &mut names);
        for token in names {
            add_binding(node, token, kind, &container, source, result, bindings);
        }
        if matches!(
            kind,
            "function" | "method" | "class" | "struct" | "enum" | "trait" | "module" | "interface"
        ) {
            next_container = Some(match &container {
                Some(parent) => format!("{parent}::{}", text(name, source)),
                None => text(name, source).into(),
            });
        }
    }
    if node.kind() == "variable_list" && ancestor_kind(node, "variable_declaration") {
        let mut cursor = node.walk();
        for name in node.children_by_field_name("name", &mut cursor) {
            if name.kind() == "identifier" {
                let declaration = node.parent().unwrap_or(node);
                add_binding(
                    declaration,
                    name,
                    "variable",
                    &container,
                    source,
                    result,
                    bindings,
                );
            }
        }
    }
    if node.kind() == "field"
        && let Some(name) = node
            .child_by_field_name("name")
            .filter(|n| n.kind() == "identifier")
    {
        add_binding(node, name, "field", &container, source, result, bindings);
    }
    if node.kind() == "identifier"
        && node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "import_clause" | "namespace_import"))
    {
        add_binding(node, node, "import", &container, source, result, bindings);
    }
    if matches!(
        node.kind(),
        "identifier" | "object_pattern" | "array_pattern" | "assignment_pattern" | "rest_pattern"
    ) && node.parent().is_some_and(|parent| {
        matches!(parent.kind(), "formal_parameters" | "closure_parameters")
            || parent.kind() == "arrow_function"
                && parent.child_by_field_name("parameter") == Some(node)
    }) {
        let mut names = Vec::with_capacity(4);
        name_tokens(node, &mut names);
        for name in names {
            add_binding(
                node,
                name,
                "parameter",
                &container,
                source,
                result,
                bindings,
            );
        }
    }
    if node.kind() == "use_declaration"
        && let Some(argument) = node.child_by_field_name("argument")
    {
        collect_use(argument, source, &container, result, bindings);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, source, next_container.clone(), result, bindings);
    }
}

fn conditional(node: Node<'_>, source: &[u8]) -> bool {
    let mut current = Some(node);
    while let Some(ancestor) = current {
        let mut previous = ancestor.prev_named_sibling();
        while let Some(attribute) = previous.filter(|n| n.kind() == "attribute_item") {
            if text(attribute, source).contains("cfg") {
                return true;
            }
            previous = attribute.prev_named_sibling();
        }
        current = ancestor.parent();
    }
    text(node, source).contains("#[cfg")
}

fn collect_use(
    node: Node<'_>,
    source: &[u8],
    container: &Option<String>,
    result: &mut Extraction,
    bindings: &mut Vec<Binding>,
) {
    let name = match node.kind() {
        "identifier" => Some(node),
        "scoped_identifier" => node.child_by_field_name("name"),
        "use_as_clause" => node.child_by_field_name("alias"),
        _ => None,
    };
    if let Some(name) = name {
        add_binding(node, name, "import", container, source, result, bindings);
    } else if let Some(list) = node.child_by_field_name("list") {
        collect_use(list, source, container, result, bindings);
    } else if node.kind() == "use_list" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_use(child, source, container, result, bindings);
        }
    }
}

pub(super) fn ancestor_kind(node: Node<'_>, kind: &str) -> bool {
    let mut parent = node.parent();
    while let Some(current) = parent {
        if current.kind() == kind {
            return true;
        }
        if matches!(
            current.kind(),
            "block" | "statement_block" | "source_file" | "program" | "chunk"
        ) {
            break;
        }
        parent = current.parent();
    }
    false
}

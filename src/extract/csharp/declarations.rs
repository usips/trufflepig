use std::collections::HashMap;
use std::ops::Range;

use tree_sitter::Node;

use super::super::bindings::{Binding, text};
use super::super::{Definition, Extraction, Occurrence};
use super::scopes::{
    ancestor, binding_scope, display_name, has_conditional_ancestor, identifier_tokens, join,
};

pub(super) fn collect(
    node: Node<'_>,
    source: &[u8],
    container: Option<String>,
    result: &mut Extraction,
    bindings: &mut Vec<Binding>,
    declarations: &mut Vec<Range<usize>>,
    owners: &mut HashMap<usize, usize>,
) {
    if node.kind() == "comment" {
        return;
    }
    if node.kind() == "compilation_unit" {
        let mut current = container;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect(
                child,
                source,
                current.clone(),
                result,
                bindings,
                declarations,
                owners,
            );
            if child.kind() == "file_scoped_namespace_declaration"
                && let Some(name) = child.child_by_field_name("name")
            {
                current = Some(join(current, display_name(name, source)));
            }
        }
        return;
    }

    let mut child_container = container.clone();
    let (kind, name) = match node.kind() {
        "namespace_declaration" | "file_scoped_namespace_declaration" => {
            let Some(name) = node.child_by_field_name("name") else {
                return;
            };
            let label = display_name(name, source);
            add_symbol(
                node,
                name,
                &label,
                "namespace",
                container.clone(),
                2,
                false,
                true,
                result,
                bindings,
                declarations,
                owners,
            );
            child_container = Some(join(container.clone(), label));
            (None, None)
        }
        "class_declaration" => (Some("class"), node.child_by_field_name("name")),
        "interface_declaration" => (Some("interface"), node.child_by_field_name("name")),
        "struct_declaration" => (Some("struct"), node.child_by_field_name("name")),
        "record_declaration" => (Some("record"), node.child_by_field_name("name")),
        "enum_declaration" => (Some("enum"), node.child_by_field_name("name")),
        "delegate_declaration" => (Some("delegate"), node.child_by_field_name("name")),
        "method_declaration" => (Some("method"), node.child_by_field_name("name")),
        "constructor_declaration" => (Some("constructor"), node.child_by_field_name("name")),
        "local_function_statement" => (Some("function"), node.child_by_field_name("name")),
        "property_declaration" => (Some("property"), node.child_by_field_name("name")),
        "event_declaration" => (Some("event"), node.child_by_field_name("name")),
        "enum_member_declaration" => (Some("enum_member"), node.child_by_field_name("name")),
        "type_parameter" => (Some("type_parameter"), node.child_by_field_name("name")),
        "parameter" => (Some("parameter"), node.child_by_field_name("name")),
        "implicit_parameter" => (Some("parameter"), Some(node)),
        "variable_declarator" => {
            let kind = if ancestor(node, "event_field_declaration") {
                "event"
            } else if ancestor(node, "field_declaration") {
                "field"
            } else {
                "local"
            };
            (Some(kind), node.child_by_field_name("name"))
        }
        "catch_declaration" | "declaration_expression" => {
            (Some("local"), node.child_by_field_name("name"))
        }
        "foreach_statement" => {
            if let Some(left) = node.child_by_field_name("left") {
                let mut names = Vec::with_capacity(2);
                identifier_tokens(left, &mut names);
                for name in names {
                    add_symbol(
                        node,
                        name,
                        text(name, source),
                        "local",
                        container.clone(),
                        1,
                        true,
                        false,
                        result,
                        bindings,
                        declarations,
                        owners,
                    );
                }
            }
            (None, None)
        }
        _ => (None, None),
    };

    if let (Some(kind), Some(name)) = (kind, name) {
        let display = display_name(name, source);
        let type_symbol = matches!(
            kind,
            "class"
                | "interface"
                | "struct"
                | "record"
                | "enum"
                | "delegate"
                | "type_parameter"
                | "namespace"
        );
        let local = matches!(kind, "local" | "parameter");
        let hoisted = !local;
        add_symbol(
            node,
            name,
            &display,
            kind,
            container.clone(),
            if type_symbol { 2 } else { 1 },
            kind == "type_parameter" || local || kind == "function",
            hoisted,
            result,
            bindings,
            declarations,
            owners,
        );
        if matches!(
            kind,
            "class"
                | "interface"
                | "struct"
                | "record"
                | "enum"
                | "delegate"
                | "method"
                | "constructor"
                | "function"
                | "property"
                | "field"
                | "event"
        ) {
            child_container = Some(join(container, display));
        }
    }

    if node.kind() == "type_parameter_list" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect(
                child,
                source,
                child_container.clone(),
                result,
                bindings,
                declarations,
                owners,
            );
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(
            child,
            source,
            child_container.clone(),
            result,
            bindings,
            declarations,
            owners,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn add_symbol(
    node: Node<'_>,
    name: Node<'_>,
    spelling: &str,
    kind: &str,
    container: Option<String>,
    namespaces: u8,
    resolvable: bool,
    hoisted: bool,
    result: &mut Extraction,
    bindings: &mut Vec<Binding>,
    declarations: &mut Vec<Range<usize>>,
    owners: &mut HashMap<usize, usize>,
) {
    if spelling.is_empty() {
        return;
    }
    let definition = result.definitions.len();
    let scope = binding_scope(node, kind);
    let token = name.byte_range();
    let conditional = has_conditional_ancestor(node);
    bindings.push(Binding {
        definition,
        token: token.clone(),
        visible: if hoisted { scope.start } else { token.end },
        scope,
        resolvable: resolvable && !conditional,
        blocks_before: false,
        function_boundary: None,
        namespaces,
    });
    result.definitions.push(Definition {
        name: spelling.into(),
        kind: kind.into(),
        start: node.start_byte(),
        end: node.end_byte(),
        container,
    });
    result.occurrences.push(Occurrence {
        name: spelling.into(),
        start: token.start,
        end: token.end,
        role: "declaration".into(),
        target: Some(definition),
        candidates: Vec::new(),
        provenance: "syntax_declaration".into(),
    });
    declarations.push(token);
    if matches!(
        kind,
        "method" | "constructor" | "function" | "property" | "field" | "event"
    ) {
        owners.insert(node.start_byte(), definition);
    }
}

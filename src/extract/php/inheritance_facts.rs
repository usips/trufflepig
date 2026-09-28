use tree_sitter::Node;

use crate::extract::php_markers::{ClassRole, Marker, ParentKind, Visibility, encode};

use super::Extraction;
use super::declarations::Catalog;
use super::names::{canonical, final_segment, text};
use super::scopes::{class_node, namespace_at};

pub(super) fn collect(root: Node<'_>, source: &[u8], catalog: &Catalog, result: &mut Extraction) {
    let uncertain_file = result.status != "complete";
    collect_at(root, source, catalog, uncertain_file, result);
}

fn collect_at(
    node: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    uncertain_file: bool,
    result: &mut Extraction,
) {
    match node.kind() {
        "class_declaration" => add_class_facts(node, source, catalog, uncertain_file, result),
        "use_declaration" => add_trait_use_facts(node, source, catalog, uncertain_file, result),
        "method_declaration" => add_method_fact(node, source, catalog, uncertain_file, result),
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_at(child, source, catalog, uncertain_file, result);
    }
}

fn add_class_facts(
    class: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    uncertain_file: bool,
    result: &mut Extraction,
) {
    let Some(definition) = catalog.class_definitions.get(&class.start_byte()).copied() else {
        return;
    };
    let role = result
        .definitions
        .get(definition)
        .map_or(ClassRole::Unknown, |class| {
            if has_xfcp_prefix(&class.name) {
                ClassRole::Proxy
            } else {
                ClassRole::Ordinary
            }
        });
    let conditional = uncertain_file || class_is_conditional(class);
    result.relationships.push(encode(
        definition,
        None,
        class.byte_range(),
        Marker::Class { role, conditional },
    ));

    let mut cursor = class.walk();
    let Some(parent_clause) = class
        .named_children(&mut cursor)
        .find(|child| child.kind() == "base_clause")
    else {
        return;
    };
    add_parent_fact(
        class,
        parent_clause,
        definition,
        source,
        catalog,
        uncertain_file,
        result,
    );
}

fn add_parent_fact(
    class: Node<'_>,
    clause: Node<'_>,
    class_definition: usize,
    source: &[u8],
    catalog: &Catalog,
    uncertain_file: bool,
    result: &mut Extraction,
) {
    let mut cursor = clause.walk();
    let names: Vec<_> = clause
        .named_children(&mut cursor)
        .filter(|child| is_name_node(*child))
        .collect();
    let Ok([parent]) = <[_; 1]>::try_from(names) else {
        result.relationships.push(encode(
            class_definition,
            None,
            clause.byte_range(),
            Marker::Parent {
                kind: ParentKind::Unknown,
                conditional: uncertain_file || class_is_conditional(class) || clause.has_error(),
                resolved_name: None,
            },
        ));
        return;
    };

    let raw = text(parent, source);
    let (canonical_name, resolved) = canonical(
        raw,
        namespace_at(&catalog.regions, parent.start_byte()),
        &catalog.imports,
        "class",
        parent.start_byte(),
    );
    let resolved_name = (resolved && !canonical_name.is_empty()).then(|| canonical_name.clone());
    let kind = if parent.has_error() || resolved_name.is_none() {
        ParentKind::Unknown
    } else if has_xfcp_prefix(final_segment(&canonical_name)) {
        ParentKind::Proxy
    } else {
        ParentKind::Ordinary
    };
    result.relationships.push(encode(
        class_definition,
        None,
        if kind == ParentKind::Unknown && clause.has_error() {
            clause.byte_range()
        } else {
            parent.byte_range()
        },
        Marker::Parent {
            kind,
            conditional: uncertain_file || class_is_conditional(class),
            resolved_name,
        },
    ));
}

fn add_trait_use_facts(
    use_node: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    uncertain_file: bool,
    result: &mut Extraction,
) {
    let Some(owner) = class_node(use_node) else {
        return;
    };
    let Some(class_definition) = catalog.class_definitions.get(&owner.start_byte()).copied() else {
        return;
    };
    let conditional =
        uncertain_file || class_is_conditional(owner) || conditional_between(use_node, owner);
    let mut cursor = use_node.walk();
    for name in use_node
        .named_children(&mut cursor)
        .filter(|child| is_name_node(*child))
    {
        let raw = text(name, source);
        if raw.trim().is_empty() {
            continue;
        }
        result.relationships.push(encode(
            class_definition,
            None,
            name.byte_range(),
            Marker::TraitUse { conditional },
        ));
    }
}

fn add_method_fact(
    method: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    uncertain_file: bool,
    result: &mut Extraction,
) {
    let Some(owner) = class_node(method) else {
        return;
    };
    let Some(owner_definition) = catalog.class_definitions.get(&owner.start_byte()).copied() else {
        return;
    };
    let Some(method_definition) = catalog.owners.get(&method.start_byte()).copied() else {
        return;
    };
    let visibility = method_visibility(method, source);
    let abstract_method = method
        .named_children(&mut method.walk())
        .any(|child| child.kind() == "abstract_modifier")
        || method.child_by_field_name("body").is_none()
        || owner.kind() == "interface_declaration";
    let conditional =
        uncertain_file || class_is_conditional(owner) || conditional_between(method, owner);
    result.relationships.push(encode(
        method_definition,
        Some(owner_definition),
        method.byte_range(),
        Marker::Method {
            visibility,
            abstract_method,
            conditional,
        },
    ));
}

fn method_visibility(method: Node<'_>, source: &[u8]) -> Visibility {
    let mut cursor = method.walk();
    let modifiers: Vec<_> = method
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "visibility_modifier")
        .collect();
    match modifiers.as_slice() {
        [] => Visibility::Public,
        [modifier] => match text(*modifier, source).trim() {
            "public" => Visibility::Public,
            "protected" => Visibility::Protected,
            "private" => Visibility::Private,
            _ => Visibility::Unknown,
        },
        _ => Visibility::Unknown,
    }
}

fn class_is_conditional(class: Node<'_>) -> bool {
    has_dynamic_ancestor(class, None)
}

fn conditional_between(node: Node<'_>, owner: Node<'_>) -> bool {
    has_dynamic_ancestor(node, Some(owner))
}

fn has_dynamic_ancestor(node: Node<'_>, stop: Option<Node<'_>>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if stop.is_some_and(|boundary| boundary.id() == parent.id()) {
            return false;
        }
        if matches!(
            parent.kind(),
            "if_statement"
                | "else_clause"
                | "else_if_clause"
                | "switch_statement"
                | "case_statement"
                | "while_statement"
                | "do_statement"
                | "for_statement"
                | "foreach_statement"
                | "try_statement"
                | "catch_clause"
                | "finally_clause"
                | "function_definition"
                | "method_declaration"
                | "anonymous_function"
                | "arrow_function"
        ) {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn is_name_node(node: Node<'_>) -> bool {
    matches!(node.kind(), "name" | "qualified_name" | "relative_name")
}

fn has_xfcp_prefix(name: &str) -> bool {
    name.get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("XFCP_"))
}

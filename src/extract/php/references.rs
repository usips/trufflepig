use tree_sitter::Node;

use crate::extract::php_markers::{Marker, encode};

use super::declarations::Catalog;
use super::names::{canonical, final_segment, terminal_name, text};
use super::scopes::{ancestor, class_node, namespace_at};
use super::{Extraction, Occurrence, Relationship};

pub(super) fn collect(root: Node<'_>, source: &[u8], catalog: &Catalog, result: &mut Extraction) {
    collect_at(root, source, catalog, result);
}

fn collect_at(node: Node<'_>, source: &[u8], catalog: &Catalog, result: &mut Extraction) {
    if matches!(node.kind(), "comment" | "php_tag" | "text") {
        return;
    }
    match node.kind() {
        "function_call_expression" => {
            if let Some(function) = node.child_by_field_name("function") {
                if let Some(name) = static_name(function) {
                    observe_call(name, "function", source, node, catalog, result);
                }
            }
        }
        "member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression" => {
            observe_scoped_call(node, source, catalog, result);
        }
        "object_creation_expression" => observe_object_creation(node, source, catalog, result),
        "class_constant_access_expression" => observe_class_constant(node, source, catalog, result),
        "scoped_property_access_expression" => {
            observe_scoped_property(node, source, catalog, result)
        }
        "member_access_expression" | "nullsafe_member_access_expression" => {
            observe_member_access(node, source, catalog, result)
        }
        "use_declaration" => observe_trait_use(node, source, catalog, result),
        "variable_name" => observe_variable(node, source, catalog, result),
        "name" | "qualified_name" | "relative_name" => {
            observe_named_reference(node, source, catalog, result)
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_at(child, source, catalog, result);
    }
}

fn observe_named_reference(
    node: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    result: &mut Extraction,
) {
    if is_suppressed_name(node, catalog) || is_nested_name_component(node) {
        return;
    }
    if in_import_or_trait_use(node) || in_call_name(node) || in_object_name(node) {
        return;
    }
    if is_type_context(node) {
        observe_type(node, source, catalog, result);
    } else if is_constant_context(node) {
        observe_constant(node, source, catalog, result);
    }
}

fn observe_type(node: Node<'_>, source: &[u8], catalog: &Catalog, result: &mut Extraction) {
    let raw = text(node, source);
    let namespace = namespace_at(&catalog.regions, node.start_byte());
    let (canonical_name, resolved) =
        canonical(raw, namespace, &catalog.imports, "class", node.start_byte());
    let raw_placeholder = is_xfcp_placeholder(raw);
    let placeholder = raw_placeholder || (resolved && is_xfcp_placeholder(&canonical_name));
    let imported_placeholder_alias = raw_placeholder
        && !raw.contains('\\')
        && resolved
        && canonical_name != super::names::join(namespace, raw);
    let name = if raw_placeholder && !imported_placeholder_alias {
        raw.to_owned()
    } else {
        canonical_name
    };
    let candidates = if resolved && !placeholder {
        candidates_for_type(&name, catalog)
    } else {
        Vec::new()
    };
    let provenance = if placeholder {
        "xenforo_generated_placeholder"
    } else if resolved {
        "php_fqcn"
    } else {
        "php_ambiguous_import"
    };
    result.occurrences.push(Occurrence {
        name,
        start: node.start_byte(),
        end: node.end_byte(),
        role: "type".into(),
        target: None,
        candidates,
        provenance: provenance.into(),
    });
    if let Some((source_definition, kind)) = inheritance_owner(node, catalog) {
        result.relationships.push(Relationship {
            source: source_definition,
            target: None,
            kind: kind.into(),
            evidence_start: node.start_byte(),
            evidence_end: node.end_byte(),
            provenance: "unresolved_inheritance".into(),
        });
    } else if let Some(source_definition) = trait_owner(node, catalog) {
        result.relationships.push(Relationship {
            source: source_definition,
            target: None,
            kind: "uses_trait".into(),
            evidence_start: node.start_byte(),
            evidence_end: node.end_byte(),
            provenance: "unresolved_trait_use".into(),
        });
    }
}

fn is_xfcp_placeholder(name: &str) -> bool {
    final_segment(name)
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("XFCP_"))
}

fn observe_call(
    node: Node<'_>,
    call_kind: &str,
    source: &[u8],
    expression: Node<'_>,
    catalog: &Catalog,
    result: &mut Extraction,
) {
    if is_suppressed_name(node, catalog) {
        return;
    }
    let raw = text(node, source);
    let namespace = namespace_at(&catalog.regions, node.start_byte());
    let (name, resolved) = if call_kind == "function" {
        canonical(
            raw,
            namespace,
            &catalog.imports,
            "function",
            node.start_byte(),
        )
    } else {
        (raw.to_owned(), true)
    };
    let candidates = if resolved && call_kind == "function" {
        candidates_for_call(&name, None, catalog)
    } else if call_kind == "method" {
        candidates_for_call(
            raw,
            method_container(expression, source, catalog).as_deref(),
            catalog,
        )
    } else {
        Vec::new()
    };
    result.occurrences.push(Occurrence {
        name,
        start: node.start_byte(),
        end: node.end_byte(),
        role: "call".into(),
        target: None,
        candidates,
        provenance: if resolved {
            "php_call"
        } else {
            "php_ambiguous_import"
        }
        .into(),
    });
    if let Some(source_definition) = owner(expression, catalog) {
        result.relationships.push(Relationship {
            source: source_definition,
            target: None,
            kind: "calls".into(),
            evidence_start: node.start_byte(),
            evidence_end: node.end_byte(),
            provenance: "php_call".into(),
        });
    }
}

fn observe_scoped_call(node: Node<'_>, source: &[u8], catalog: &Catalog, result: &mut Extraction) {
    if node.kind() == "scoped_call_expression"
        && let Some(scope) = node.child_by_field_name("scope")
        && is_parent_scope(scope, source)
    {
        if let Some(name) = node.child_by_field_name("name") {
            observe_parent_call(name, node, source, catalog, result);
        }
        return;
    }

    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    if matches!(name.kind(), "name" | "qualified_name" | "relative_name") {
        observe_call(name, "method", source, node, catalog, result);
    }
    if node.kind() == "scoped_call_expression"
        && let Some(scope) = node.child_by_field_name("scope")
        && is_name_node(scope)
    {
        observe_type(scope, source, catalog, result);
    }
}

fn observe_parent_call(
    name: Node<'_>,
    expression: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    result: &mut Extraction,
) {
    if name.kind() == "name"
        && let Some((caller_method, owner_class)) = parent_call_owner(expression, catalog)
    {
        result.occurrences.push(Occurrence {
            name: text(name, source).to_owned(),
            start: name.start_byte(),
            end: name.end_byte(),
            role: "call".into(),
            target: None,
            candidates: Vec::new(),
            provenance: "php_parent_call".into(),
        });
        result.relationships.push(encode(
            caller_method,
            Some(owner_class),
            name.start_byte()..name.end_byte(),
            Marker::ParentCall,
        ));
        return;
    }

    result.occurrences.push(Occurrence {
        name: text(name, source).to_owned(),
        start: name.start_byte(),
        end: name.end_byte(),
        role: "call".into(),
        target: None,
        candidates: Vec::new(),
        provenance: "php_parent_call_unresolved".into(),
    });
}

fn is_parent_scope(scope: Node<'_>, source: &[u8]) -> bool {
    (scope.kind() == "relative_scope" || is_name_node(scope))
        && text(scope, source).eq_ignore_ascii_case("parent")
}

fn parent_call_owner(expression: Node<'_>, catalog: &Catalog) -> Option<(usize, usize)> {
    let mut current = Some(expression);
    let method = loop {
        let node = current?;
        match node.kind() {
            "method_declaration" => break node,
            "function_definition" | "anonymous_function" | "arrow_function" => return None,
            _ => current = node.parent(),
        }
    };

    let class = class_node(method)?;
    if class.kind() != "class_declaration" {
        return None;
    }

    Some((
        *catalog.owners.get(&method.start_byte())?,
        *catalog.class_definitions.get(&class.start_byte())?,
    ))
}

fn observe_object_creation(
    node: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    result: &mut Extraction,
) {
    let mut cursor = node.walk();
    if let Some(name) = node
        .named_children(&mut cursor)
        .find(|child| is_name_node(*child))
    {
        observe_type(name, source, catalog, result);
    }
}

fn observe_class_constant(
    node: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    result: &mut Extraction,
) {
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    if let Some(scope) = children
        .first()
        .copied()
        .filter(|child| is_name_node(*child))
    {
        observe_type(scope, source, catalog, result);
    }
    if let Some(constant) = children.get(1).copied().and_then(terminal_name) {
        observe_constant(constant, source, catalog, result);
    }
}

fn observe_scoped_property(
    node: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    result: &mut Extraction,
) {
    if let Some(scope) = node.child_by_field_name("scope")
        && is_name_node(scope)
    {
        observe_type(scope, source, catalog, result);
    }
    if let Some(name) = node.child_by_field_name("name") {
        observe_member_name(name, source, catalog, result);
    }
}

fn observe_member_access(
    node: Node<'_>,
    source: &[u8],
    catalog: &Catalog,
    result: &mut Extraction,
) {
    if let Some(name) = node.child_by_field_name("name") {
        observe_member_name(name, source, catalog, result);
    }
}

fn observe_member_name(node: Node<'_>, source: &[u8], catalog: &Catalog, result: &mut Extraction) {
    if is_suppressed_name(node, catalog) {
        return;
    }
    let name = text(node, source).trim_start_matches('$');
    if name.is_empty() {
        return;
    }
    result.occurrences.push(Occurrence {
        name: name.into(),
        start: node.start_byte(),
        end: node.end_byte(),
        role: "read".into(),
        target: None,
        candidates: Vec::new(),
        provenance: "php_member_name".into(),
    });
}

fn observe_trait_use(node: Node<'_>, source: &[u8], catalog: &Catalog, result: &mut Extraction) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if is_name_node(child) {
            observe_type(child, source, catalog, result);
        }
    }
}

fn observe_constant(node: Node<'_>, source: &[u8], catalog: &Catalog, result: &mut Extraction) {
    if is_suppressed_name(node, catalog) {
        return;
    }
    let raw = text(node, source);
    if matches!(raw.to_ascii_lowercase().as_str(), "true" | "false" | "null") {
        return;
    }
    let namespace = namespace_at(&catalog.regions, node.start_byte());
    let (name, resolved) = canonical(
        raw,
        namespace,
        &catalog.imports,
        "constant",
        node.start_byte(),
    );
    let candidates = if resolved {
        candidates_for_constant(&name, catalog)
    } else {
        Vec::new()
    };
    result.occurrences.push(Occurrence {
        name,
        start: node.start_byte(),
        end: node.end_byte(),
        role: "read".into(),
        target: None,
        candidates,
        provenance: if resolved {
            "php_constant"
        } else {
            "php_ambiguous_import"
        }
        .into(),
    });
}

fn observe_variable(node: Node<'_>, source: &[u8], catalog: &Catalog, result: &mut Extraction) {
    if is_suppressed_name(node, catalog) || is_member_name(node) {
        return;
    }
    let name = text(node, source);
    let scope = super::scopes::function_scope(node);
    let matching: Vec<_> = catalog
        .symbols
        .iter()
        .filter(|symbol| {
            matches!(symbol.kind.as_str(), "parameter" | "variable")
                && symbol.qualified_name == name
                && symbol.function_scope == scope
                && symbol.scope.contains(&node.start_byte())
        })
        .collect();
    let candidates: Vec<_> = matching.iter().map(|symbol| symbol.definition).collect();
    let target = (result.status == "complete"
        && matching.len() == 1
        && matching[0].kind == "parameter"
        && matching[0].resolvable)
        .then(|| matching[0].definition);
    result.occurrences.push(Occurrence {
        name: name.into(),
        start: node.start_byte(),
        end: node.end_byte(),
        role: "read".into(),
        target,
        candidates: if target.is_some() {
            Vec::new()
        } else {
            candidates
        },
        provenance: if target.is_some() {
            "php_local_binding"
        } else {
            "candidate_name"
        }
        .into(),
    });
}

fn is_type_context(node: Node<'_>) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        if parent.kind() == "attribute" {
            return parent.named_child(0) == Some(child);
        }
        if parent.kind() == "cast_expression" {
            return parent.child_by_field_name("type").is_some_and(|type_node| {
                type_node.start_byte() <= node.start_byte()
                    && node.end_byte() <= type_node.end_byte()
            });
        }
        if matches!(
            parent.kind(),
            "type"
                | "named_type"
                | "base_clause"
                | "class_interface_clause"
                | "type_list"
                | "instanceof_expression"
        ) {
            return true;
        }
        if matches!(
            parent.kind(),
            "function_call_expression"
                | "member_call_expression"
                | "scoped_call_expression"
                | "object_creation_expression"
                | "namespace_use_declaration"
                | "use_declaration"
                | "class_constant_access_expression"
        ) {
            return false;
        }
        child = parent;
    }
    false
}

fn is_constant_context(node: Node<'_>) -> bool {
    if ancestor(node, &["named_label_statement"]).is_some() {
        return false;
    }
    if node.parent().is_some_and(|parent| {
        parent.kind() == "argument" && parent.child_by_field_name("name") == Some(node)
    }) {
        return false;
    }
    ancestor(
        node,
        &[
            "function_call_expression",
            "member_call_expression",
            "nullsafe_member_call_expression",
            "scoped_call_expression",
            "object_creation_expression",
            "namespace_use_declaration",
            "use_declaration",
            "class_constant_access_expression",
            "member_access_expression",
            "nullsafe_member_access_expression",
            "scoped_property_access_expression",
            "variable_name",
        ],
    )
    .is_none()
}

fn is_suppressed_name(node: Node<'_>, catalog: &Catalog) -> bool {
    catalog
        .declarations
        .iter()
        .any(|range| range.start <= node.start_byte() && node.end_byte() <= range.end)
}

fn is_nested_name_component(node: Node<'_>) -> bool {
    ancestor(node, &["qualified_name", "relative_name", "namespace_name"])
        .is_some_and(|parent| parent.id() != node.id())
}

fn in_import_or_trait_use(node: Node<'_>) -> bool {
    ancestor(node, &["namespace_use_declaration", "use_declaration"]).is_some()
}

fn in_call_name(node: Node<'_>) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        if matches!(
            parent.kind(),
            "function_call_expression"
                | "member_call_expression"
                | "nullsafe_member_call_expression"
                | "scoped_call_expression"
        ) {
            return parent
                .child_by_field_name("function")
                .or_else(|| parent.child_by_field_name("name"))
                .is_some_and(|field| {
                    field.start_byte() <= node.start_byte() && node.end_byte() <= field.end_byte()
                });
        }
        child = parent;
    }
    false
}

fn in_object_name(node: Node<'_>) -> bool {
    ancestor(node, &["object_creation_expression"]).is_some()
}

fn is_member_name(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "member_access_expression"
                | "nullsafe_member_access_expression"
                | "scoped_property_access_expression"
        ) && parent.child_by_field_name("name") == Some(node)
    })
}

fn is_name_node(node: Node<'_>) -> bool {
    matches!(node.kind(), "name" | "qualified_name" | "relative_name")
}

fn static_name(node: Node<'_>) -> Option<Node<'_>> {
    if is_name_node(node) {
        Some(node)
    } else if node.kind() == "parenthesized_expression" {
        node.named_child(0).and_then(static_name)
    } else {
        None
    }
}

fn candidates_for_type(name: &str, catalog: &Catalog) -> Vec<usize> {
    let name = name.trim_start_matches('\\');
    catalog
        .symbols
        .iter()
        .filter(|symbol| {
            matches!(
                symbol.kind.as_str(),
                "class" | "interface" | "trait" | "enum"
            ) && symbol
                .qualified_name
                .trim_start_matches('\\')
                .eq_ignore_ascii_case(name)
        })
        .map(|symbol| symbol.definition)
        .take(64)
        .collect()
}

fn candidates_for_call(name: &str, container: Option<&str>, catalog: &Catalog) -> Vec<usize> {
    let name = name.trim_start_matches('\\');
    catalog
        .symbols
        .iter()
        .filter(|symbol| {
            if symbol.kind == "function" {
                symbol
                    .qualified_name
                    .trim_start_matches('\\')
                    .eq_ignore_ascii_case(name)
            } else if symbol.kind == "method" {
                let same_name = final_segment(&symbol.qualified_name).eq_ignore_ascii_case(name);
                same_name
                    && container.is_none_or(|container| {
                        symbol
                            .qualified_name
                            .rsplit_once('\\')
                            .is_some_and(|(owner, _)| owner.eq_ignore_ascii_case(container))
                    })
            } else {
                false
            }
        })
        .map(|symbol| symbol.definition)
        .take(64)
        .collect()
}

fn candidates_for_constant(name: &str, catalog: &Catalog) -> Vec<usize> {
    let name = name.trim_start_matches('\\');
    catalog
        .symbols
        .iter()
        .filter(|symbol| {
            symbol.kind == "constant" && symbol.qualified_name.trim_start_matches('\\') == name
        })
        .map(|symbol| symbol.definition)
        .take(64)
        .collect()
}

fn inheritance_owner(node: Node<'_>, catalog: &Catalog) -> Option<(usize, &'static str)> {
    let parent = ancestor(node, &["base_clause", "class_interface_clause"])?;
    let class = class_node(parent)?;
    let source = *catalog.class_definitions.get(&class.start_byte())?;
    let kind = if parent.kind() == "base_clause" {
        "extends"
    } else {
        "implements"
    };
    Some((source, kind))
}

fn trait_owner(node: Node<'_>, catalog: &Catalog) -> Option<usize> {
    let use_node = ancestor(node, &["use_declaration"])?;
    if is_trait_adaptation(node, use_node) {
        return None;
    }
    let class = class_node(use_node)?;
    catalog.class_definitions.get(&class.start_byte()).copied()
}

fn is_trait_adaptation(node: Node<'_>, use_node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.id() == use_node.id() {
            return false;
        }
        if matches!(
            parent.kind(),
            "use_list" | "use_as_clause" | "use_instead_of_clause"
        ) {
            return true;
        }
        current = parent.parent();
    }
    true
}

fn method_container(expression: Node<'_>, source: &[u8], catalog: &Catalog) -> Option<String> {
    let scope = expression.child_by_field_name("scope")?;
    if !is_name_node(scope) {
        return None;
    }
    let namespace = namespace_at(&catalog.regions, scope.start_byte());
    let (name, resolved) = canonical(
        text(scope, source),
        namespace,
        &catalog.imports,
        "class",
        scope.start_byte(),
    );
    resolved.then_some(name)
}

fn owner(node: Node<'_>, catalog: &Catalog) -> Option<usize> {
    let function = ancestor(node, &["method_declaration", "function_definition"])?;
    catalog.owners.get(&function.start_byte()).copied()
}

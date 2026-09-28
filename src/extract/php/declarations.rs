use std::collections::HashMap;
use std::ops::Range;

use tree_sitter::Node;

use super::imports;
use super::names::{Import, join, text};
use super::scopes::{NamespaceRegion, class_node, function_scope, namespace_at, namespace_regions};
use super::{Definition, Extraction, Occurrence, Relationship};

pub(super) struct Catalog {
    pub regions: Vec<NamespaceRegion>,
    pub imports: Vec<Import>,
    pub declarations: Vec<Range<usize>>,
    pub symbols: Vec<Symbol>,
    pub class_definitions: HashMap<usize, usize>,
    pub owners: HashMap<usize, usize>,
    uncertain_globals: Vec<(Range<usize>, String)>,
}

pub(super) struct Symbol {
    pub definition: usize,
    pub qualified_name: String,
    pub kind: String,
    pub scope: Range<usize>,
    pub function_scope: Range<usize>,
    pub resolvable: bool,
}

pub(super) fn collect(root: Node<'_>, source: &[u8], result: &mut Extraction) -> Catalog {
    let regions = namespace_regions(root, source);
    let imports = imports::collect(root, source, &regions);
    let mut catalog = Catalog {
        regions,
        imports,
        declarations: Vec::with_capacity(source.len() / 160),
        symbols: Vec::with_capacity(source.len() / 128),
        class_definitions: HashMap::new(),
        owners: HashMap::new(),
        uncertain_globals: Vec::new(),
    };
    collect_at(root, source, &mut catalog, result);
    for symbol in &mut catalog.symbols {
        if symbol.kind == "parameter"
            && catalog.uncertain_globals.iter().any(|(scope, name)| {
                scope == &symbol.function_scope && name == &symbol.qualified_name
            })
        {
            symbol.resolvable = false;
        }
    }
    catalog
        .declarations
        .sort_by_key(|range| (range.start, range.end));
    catalog
}

fn collect_at(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    if matches!(node.kind(), "comment" | "php_tag" | "text") {
        return;
    }
    match node.kind() {
        "namespace_definition" => add_namespace(node, source, catalog, result),
        "class_declaration" => add_named_type(node, "class", source, catalog, result),
        "interface_declaration" => add_named_type(node, "interface", source, catalog, result),
        "trait_declaration" => add_named_type(node, "trait", source, catalog, result),
        "enum_declaration" => add_named_type(node, "enum", source, catalog, result),
        "function_definition" => add_function(node, source, catalog, result),
        "method_declaration" => add_method(node, source, catalog, result),
        "namespace_use_clause" => add_import(node, source, catalog, result),
        "property_element" => add_property(node, source, catalog, result),
        "const_element" => add_constant(node, source, catalog, result),
        "enum_case" => add_enum_case(node, source, catalog, result),
        "simple_parameter" | "variadic_parameter" | "property_promotion_parameter" => {
            add_parameter(node, source, catalog, result)
        }
        "global_declaration" => add_global_names(node, source, catalog),
        "variable_name" if is_variable_binding(node, source) => {
            add_variable(node, source, catalog, result)
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_at(child, source, catalog, result);
    }
}

fn add_namespace(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let spelling = text(name, source).trim();
    if spelling.is_empty() {
        return;
    }
    add_definition(
        node,
        name,
        spelling,
        "namespace",
        None,
        namespace_scope(node, catalog),
        false,
        source,
        catalog,
        result,
    );
}

fn add_named_type(
    node: Node<'_>,
    kind: &str,
    source: &[u8],
    catalog: &mut Catalog,
    result: &mut Extraction,
) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let namespace = namespace_at(&catalog.regions, node.start_byte());
    let definition = add_definition(
        node,
        name,
        text(name, source),
        kind,
        nonempty(namespace),
        namespace_scope(node, catalog),
        false,
        source,
        catalog,
        result,
    );
    catalog
        .class_definitions
        .insert(node.start_byte(), definition);
}

fn add_function(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let container = namespace_at(&catalog.regions, node.start_byte());
    let scope = namespace_scope(node, catalog);
    let definition = add_definition(
        node,
        name,
        text(name, source),
        "function",
        nonempty(container),
        scope,
        false,
        source,
        catalog,
        result,
    );
    catalog.owners.insert(node.start_byte(), definition);
}

fn add_method(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let container = class_fqcn(node, source, catalog);
    let scope = class_node(node).map_or_else(|| node.byte_range(), |class| class.byte_range());
    let definition = add_definition(
        node,
        name,
        text(name, source),
        "method",
        nonempty(&container),
        scope,
        false,
        source,
        catalog,
        result,
    );
    catalog.owners.insert(node.start_byte(), definition);
}

fn add_import(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    let Some(import) = catalog
        .imports
        .iter()
        .find(|import| import.clause_span == node.byte_range())
        .cloned()
    else {
        return;
    };
    let alias_token = source.get(import.alias_span.clone()).unwrap_or_default();
    let spelling = std::str::from_utf8(alias_token).unwrap_or(&import.alias);
    let scope = import.scope.clone();
    let definition = add_definition(
        node,
        node,
        &import.alias,
        "import",
        nonempty(&import.namespace),
        scope,
        false,
        source,
        catalog,
        result,
    );
    if let Some(occurrence) = result.occurrences.last_mut() {
        occurrence.start = import.alias_span.start;
        occurrence.end = import.alias_span.end;
        occurrence.name = spelling.to_owned();
    }
    catalog.declarations.push(import.alias_span.clone());
    result.occurrences.push(Occurrence {
        name: import.target.clone(),
        start: import.target_span.start,
        end: import.target_span.end,
        role: "import".into(),
        target: None,
        candidates: Vec::new(),
        provenance: match import.kind.as_str() {
            "function" => "php_function_import",
            "const" => "php_const_import",
            _ => "php_import",
        }
        .into(),
    });
    result.relationships.push(Relationship {
        source: definition,
        target: None,
        kind: "imports".into(),
        evidence_start: import.target_span.start,
        evidence_end: import.target_span.end,
        provenance: "php_import".into(),
    });
}

fn add_property(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let property = text(name, source).trim_start_matches('$');
    let container = class_fqcn(node, source, catalog);
    add_definition(
        node,
        name,
        property,
        "property",
        nonempty(&container),
        class_node(node).map_or_else(|| node.byte_range(), |class| class.byte_range()),
        false,
        source,
        catalog,
        result,
    );
}

fn add_constant(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    let Some(name) = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "name")
    else {
        return;
    };
    let container = class_fqcn(node, source, catalog);
    let in_class = class_node(node).is_some();
    let container = if in_class {
        nonempty(&container)
    } else {
        nonempty(namespace_at(&catalog.regions, node.start_byte()))
    };
    add_definition(
        node,
        name,
        text(name, source),
        "constant",
        container,
        class_node(node).map_or_else(
            || namespace_scope(node, catalog),
            |class| class.byte_range(),
        ),
        false,
        source,
        catalog,
        result,
    );
}

fn add_enum_case(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let container = class_fqcn(node, source, catalog);
    add_definition(
        node,
        name,
        text(name, source),
        "enum_case",
        nonempty(&container),
        class_node(node).map_or_else(|| node.byte_range(), |class| class.byte_range()),
        false,
        source,
        catalog,
        result,
    );
}

fn add_parameter(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let name_text = text(name, source);
    let scope = function_scope(node);
    add_definition(
        node,
        name,
        name_text,
        "parameter",
        None,
        scope.clone(),
        true,
        source,
        catalog,
        result,
    );
    if node.kind() == "property_promotion_parameter" {
        let container = class_fqcn(node, source, catalog);
        add_definition(
            node,
            name,
            name_text.trim_start_matches('$'),
            "property",
            nonempty(&container),
            class_node(node).map_or_else(|| node.byte_range(), |class| class.byte_range()),
            false,
            source,
            catalog,
            result,
        );
    }
}

fn add_variable(node: Node<'_>, source: &[u8], catalog: &mut Catalog, result: &mut Extraction) {
    let name = text(node, source);
    if name == "$this" || is_superglobal(name) {
        return;
    }
    add_definition(
        node,
        node,
        name,
        "variable",
        None,
        function_scope(node),
        false,
        source,
        catalog,
        result,
    );
}

fn add_global_names(node: Node<'_>, source: &[u8], catalog: &mut Catalog) {
    let scope = function_scope(node);
    let mut cursor = node.walk();
    for name in node.named_children(&mut cursor) {
        if name.kind() == "variable_name" {
            catalog
                .uncertain_globals
                .push((scope.clone(), text(name, source).to_owned()));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add_definition(
    node: Node<'_>,
    token: Node<'_>,
    spelling: &str,
    kind: &str,
    container: Option<String>,
    scope: Range<usize>,
    resolvable: bool,
    source: &[u8],
    catalog: &mut Catalog,
    result: &mut Extraction,
) -> usize {
    if spelling.is_empty() {
        return usize::MAX;
    }
    let definition = result.definitions.len();
    let token_range = token.byte_range();
    let container_name = container.as_deref().unwrap_or("");
    let qualified_name = match kind {
        "class" | "interface" | "trait" | "enum" | "function" | "method" | "property"
        | "constant" | "enum_case" => join(container_name, spelling),
        _ => spelling.to_owned(),
    };
    let function_scope = function_scope(node);
    result.definitions.push(Definition {
        name: spelling.into(),
        kind: kind.into(),
        start: node.start_byte(),
        end: node.end_byte(),
        container,
    });
    result.occurrences.push(Occurrence {
        name: spelling.into(),
        start: token_range.start,
        end: token_range.end,
        role: "declaration".into(),
        target: Some(definition),
        candidates: Vec::new(),
        provenance: "syntax_declaration".into(),
    });
    catalog.declarations.push(token_range.clone());
    catalog.symbols.push(Symbol {
        definition,
        qualified_name,
        kind: kind.into(),
        scope: scope.clone(),
        function_scope,
        resolvable: resolvable && result.status == "complete",
    });
    let _ = source;
    definition
}

fn namespace_scope(node: Node<'_>, catalog: &Catalog) -> Range<usize> {
    super::scopes::namespace_region_at(&catalog.regions, node.start_byte())
        .map_or_else(|| node.byte_range(), |region| region.range.clone())
}

fn class_fqcn(node: Node<'_>, source: &[u8], catalog: &Catalog) -> String {
    let Some(class) = class_node(node) else {
        return String::new();
    };
    let Some(name) = class.child_by_field_name("name") else {
        return String::new();
    };
    join(
        namespace_at(&catalog.regions, class.start_byte()),
        text(name, source),
    )
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn is_variable_binding(node: Node<'_>, source: &[u8]) -> bool {
    if node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "simple_parameter"
                | "variadic_parameter"
                | "property_promotion_parameter"
                | "property_element"
                | "global_declaration"
                | "anonymous_function_use_clause"
        )
    }) {
        return false;
    }
    if node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "member_access_expression"
                | "nullsafe_member_access_expression"
                | "scoped_property_access_expression"
        ) && parent.child_by_field_name("name") == Some(node)
    }) {
        return false;
    }
    if node.parent().is_some_and(|parent| {
        parent.kind() == "assignment_expression" && parent.child_by_field_name("left") == Some(node)
    }) {
        return true;
    }
    if node.parent().is_some_and(|parent| {
        parent.kind() == "static_variable_declaration"
            && parent.child_by_field_name("name") == Some(node)
    }) {
        return true;
    }
    if node.parent().is_some_and(|parent| {
        parent.kind() == "catch_clause" && parent.child_by_field_name("name") == Some(node)
    }) {
        return true;
    }
    if let Some(foreach) = super::scopes::ancestor(node, &["foreach_statement"])
        && let Some(target) = foreach.named_child(1)
    {
        return foreach_target_contains(target, node);
    }
    let _ = source;
    false
}

fn foreach_target_contains(mut target: Node<'_>, variable: Node<'_>) -> bool {
    if target.kind() == "by_ref" {
        let Some(inner) = target.named_child(0) else {
            return false;
        };
        target = inner;
    }
    if target.kind() == "pair" {
        let mut cursor = target.walk();
        let children: Vec<_> = target.named_children(&mut cursor).collect();
        return children
            .into_iter()
            .any(|binding| foreach_target_contains(binding, variable));
    }
    if target.kind() == "by_ref" {
        let Some(inner) = target.named_child(0) else {
            return false;
        };
        return foreach_target_contains(inner, variable);
    }
    if target.kind() == "list_literal" {
        return variable.start_byte() >= target.start_byte()
            && variable.end_byte() <= target.end_byte();
    }
    target.id() == variable.id()
}

fn is_superglobal(name: &str) -> bool {
    matches!(
        name,
        "$GLOBALS"
            | "$_SERVER"
            | "$_GET"
            | "$_POST"
            | "$_FILES"
            | "$_COOKIE"
            | "$_SESSION"
            | "$_REQUEST"
            | "$_ENV"
    )
}

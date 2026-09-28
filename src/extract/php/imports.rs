use tree_sitter::Node;

use super::names::{Import, final_segment, qualified, text};
use super::scopes::{NamespaceRegion, namespace_at, namespace_region_at};

pub(super) fn collect(root: Node<'_>, source: &[u8], regions: &[NamespaceRegion]) -> Vec<Import> {
    let mut imports = Vec::with_capacity(source.len() / 256);
    collect_at(root, source, regions, &mut imports);
    imports
}

fn collect_at(
    node: Node<'_>,
    source: &[u8],
    regions: &[NamespaceRegion],
    imports: &mut Vec<Import>,
) {
    if node.kind() == "comment" {
        return;
    }
    if node.kind() == "namespace_use_declaration" {
        collect_declaration(node, source, regions, imports);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_at(child, source, regions, imports);
    }
}

fn collect_declaration(
    node: Node<'_>,
    source: &[u8],
    regions: &[NamespaceRegion],
    imports: &mut Vec<Import>,
) {
    let namespace = namespace_at(regions, node.start_byte()).to_owned();
    let scope = namespace_region_at(regions, node.start_byte())
        .map_or_else(|| node.byte_range(), |region| region.range.clone());
    let group = node.child_by_field_name("body");
    let prefix = if group.is_some() {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| child.kind() == "namespace_name")
            .map_or_else(String::new, |child| qualified(child, source))
    } else {
        String::new()
    };
    let default_kind =
        node.child_by_field_name("type")
            .map_or("class", |kind| match text(kind, source) {
                "function" => "function",
                "const" => "const",
                _ => "class",
            });
    if let Some(group) = group {
        let mut cursor = group.walk();
        for clause in group.named_children(&mut cursor) {
            if clause.kind() == "namespace_use_clause" {
                push_clause(
                    clause,
                    &prefix,
                    default_kind,
                    &namespace,
                    &scope,
                    source,
                    imports,
                );
            }
        }
    } else {
        let mut cursor = node.walk();
        for clause in node.named_children(&mut cursor) {
            if clause.kind() == "namespace_use_clause" {
                push_clause(
                    clause,
                    "",
                    default_kind,
                    &namespace,
                    &scope,
                    source,
                    imports,
                );
            }
        }
    }
}

fn push_clause(
    clause: Node<'_>,
    prefix: &str,
    default_kind: &str,
    namespace: &str,
    scope: &std::ops::Range<usize>,
    source: &[u8],
    imports: &mut Vec<Import>,
) {
    let alias_node = clause.child_by_field_name("alias");
    let mut cursor = clause.walk();
    let path = clause
        .named_children(&mut cursor)
        .find(|child| alias_node != Some(*child));
    let Some(path) = path else {
        return;
    };
    let kind = clause
        .child_by_field_name("type")
        .map_or(default_kind, |kind| match text(kind, source) {
            "function" => "function",
            "const" => "const",
            _ => "class",
        })
        .to_owned();
    let path_text = qualified(path, source);
    let target = if prefix.is_empty() || path_text.starts_with('\\') {
        path_text
    } else {
        format!("{prefix}\\{path_text}")
    };
    let alias = alias_node.map_or_else(
        || final_segment(&target).to_owned(),
        |alias| text(alias, source).to_owned(),
    );
    let alias_span = alias_node
        .or_else(|| super::names::terminal_name(path))
        .map_or_else(|| path.byte_range(), |node| node.byte_range());
    imports.push(Import {
        namespace: namespace.to_owned(),
        scope: scope.clone(),
        alias,
        target,
        kind,
        alias_span,
        target_span: path.byte_range(),
        clause_span: clause.byte_range(),
    });
}

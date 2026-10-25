use std::ops::Range;

use tree_sitter::Node;

use super::names::qualified;

#[derive(Clone)]
pub(super) struct NamespaceRegion {
    pub range: Range<usize>,
    pub name: String,
}

pub(super) fn namespace_regions(root: Node<'_>, source: &[u8]) -> Vec<NamespaceRegion> {
    let mut regions = Vec::new();
    let mut cursor = root.walk();
    let children: Vec<_> = root.named_children(&mut cursor).collect();
    let mut active_namespace = String::new();
    let mut active_start = root.start_byte();

    for (index, child) in children.iter().copied().enumerate() {
        if child.kind() != "namespace_definition" {
            continue;
        }
        let name = child
            .child_by_field_name("name")
            .map_or_else(String::new, |node| qualified(node, source));
        if let Some(body) = child.child_by_field_name("body") {
            regions.push(NamespaceRegion {
                range: body.byte_range(),
                name,
            });
        } else {
            if active_start < child.start_byte() {
                regions.push(NamespaceRegion {
                    range: active_start..child.start_byte(),
                    name: active_namespace.clone(),
                });
            }
            active_namespace = name;
            active_start = child.end_byte();
            let next_start = children[index + 1..]
                .iter()
                .find(|node| node.kind() == "namespace_definition")
                .map_or(root.end_byte(), |node| node.start_byte());
            regions.push(NamespaceRegion {
                range: child.byte_range(),
                name: active_namespace.clone(),
            });
            if next_start == root.end_byte() {
                regions.push(NamespaceRegion {
                    range: active_start..root.end_byte(),
                    name: active_namespace.clone(),
                });
            }
        }
    }
    if active_start < root.end_byte()
        && !regions
            .iter()
            .any(|region| region.range.start == active_start)
    {
        regions.push(NamespaceRegion {
            range: active_start..root.end_byte(),
            name: active_namespace,
        });
    }
    if regions.is_empty() {
        regions.push(NamespaceRegion {
            range: root.byte_range(),
            name: String::new(),
        });
    }
    regions.sort_by_key(|region| (region.range.start, region.range.len()));
    regions
}

pub(super) fn namespace_at(regions: &[NamespaceRegion], position: usize) -> &str {
    namespace_region_at(regions, position).map_or("", |region| region.name.as_str())
}

pub(super) fn namespace_region_at(
    regions: &[NamespaceRegion],
    position: usize,
) -> Option<&NamespaceRegion> {
    regions
        .iter()
        .filter(|region| region.range.contains(&position))
        .min_by_key(|region| region.range.len())
}

pub(super) fn ancestor<'tree>(node: Node<'tree>, kinds: &[&str]) -> Option<Node<'tree>> {
    let mut current = Some(node);
    while let Some(candidate) = current {
        if kinds.contains(&candidate.kind()) {
            return Some(candidate);
        }
        current = candidate.parent();
    }
    None
}

pub(super) fn function_scope(node: Node<'_>) -> Range<usize> {
    ancestor(
        node,
        &[
            "method_declaration",
            "function_definition",
            "anonymous_function",
            "arrow_function",
        ],
    )
    .map_or_else(|| node.byte_range(), |function| function.byte_range())
}

pub(super) fn class_node(node: Node<'_>) -> Option<Node<'_>> {
    ancestor(
        node,
        &[
            "class_declaration",
            "interface_declaration",
            "trait_declaration",
            "enum_declaration",
            "anonymous_class",
        ],
    )
}

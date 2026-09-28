use std::ops::Range;

use tree_sitter::Node;

#[derive(Clone)]
pub(super) struct Import {
    pub namespace: String,
    pub scope: Range<usize>,
    pub alias: String,
    pub target: String,
    pub kind: String,
    pub alias_span: Range<usize>,
    pub target_span: Range<usize>,
    pub clause_span: Range<usize>,
}

pub(super) fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or("")
}

pub(super) fn qualified(node: Node<'_>, source: &[u8]) -> String {
    text(node, source).trim().into()
}

pub(super) fn canonical(
    raw: &str,
    namespace: &str,
    imports: &[Import],
    kind: &str,
    position: usize,
) -> (String, bool) {
    let raw = raw.trim();
    if raw.is_empty() {
        return (String::new(), false);
    }
    if raw.starts_with('\\') {
        return (raw.into(), true);
    }
    if matches!(
        raw.to_ascii_lowercase().as_str(),
        "self" | "static" | "parent"
    ) {
        return (raw.into(), false);
    }
    if raw
        .get(..10)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("namespace\\"))
    {
        return (join(namespace, &raw[10..]), true);
    }

    let (head, tail) = raw.split_once('\\').unwrap_or((raw, ""));
    let mut aliases = imports.iter().filter(|import| {
        import.namespace.eq_ignore_ascii_case(namespace)
            && if kind == "constant" {
                import.alias == head
            } else {
                import.alias.eq_ignore_ascii_case(head)
            }
            && import.scope.contains(&position)
            && import_matches(import, kind)
    });
    if let Some(import) = aliases.next() {
        if aliases.next().is_some() {
            return (raw.into(), false);
        }
        return (
            if tail.is_empty() {
                import.target.clone()
            } else {
                join(&import.target, tail)
            },
            true,
        );
    }
    (join(namespace, raw), true)
}

fn import_matches(import: &Import, requested: &str) -> bool {
    match requested {
        "function" => import.kind == "function",
        "constant" => import.kind == "const",
        _ => import.kind == "class",
    }
}

pub(super) fn join(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.into()
    } else if name.is_empty() {
        namespace.into()
    } else {
        format!("{namespace}\\{name}")
    }
}

pub(super) fn final_segment(name: &str) -> &str {
    name.trim_end_matches('\\')
        .rsplit('\\')
        .next()
        .unwrap_or(name)
}

pub(super) fn terminal_name<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    match node.kind() {
        "name" => Some(node),
        "qualified_name" | "relative_name" | "namespace_name" => node
            .named_children(&mut node.walk())
            .last()
            .and_then(terminal_name),
        _ => None,
    }
}

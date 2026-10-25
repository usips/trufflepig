use std::time::{Duration, Instant};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, ParseOptions, Parser, Query, QueryCursor};

use super::binding_index::BindingIndex;
use super::bindings::{self, ancestor_kind, text};
use super::{Extraction, Occurrence, Relationship};

pub(super) fn grammar(path: &str) -> (Language, &'static str) {
    match super::language(path) {
        "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            "[(identifier) (type_identifier) (field_identifier)] @reference",
        ),
        "typescript" if path.ends_with(".tsx") => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            "[(identifier) (type_identifier) (property_identifier) (shorthand_property_identifier)] @reference",
        ),
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "[(identifier) (type_identifier) (property_identifier) (shorthand_property_identifier)] @reference",
        ),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            "[(identifier) (property_identifier) (shorthand_property_identifier)] @reference",
        ),
        "luau" => (tree_sitter_luau::LANGUAGE.into(), "(identifier) @reference"),
        _ => unreachable!("language dispatch occurs before parsing"),
    }
}

pub(super) fn extract(path: &str, source: &[u8]) -> Extraction {
    extract_with_deadline(path, source, Duration::from_millis(500))
}

pub(super) fn extract_with_deadline(path: &str, source: &[u8], budget: Duration) -> Extraction {
    let mut result = Extraction {
        language: super::language(path).into(),
        status: "complete".into(),
        ..Extraction::default()
    };
    if std::str::from_utf8(source).is_err() {
        result.status = "invalid_encoding".into();
        return result;
    }
    let (language, query_source) = grammar(path);
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        result.status = "incompatible_grammar".into();
        return result;
    }
    let started = Instant::now();
    let mut cancelled = |_: &tree_sitter::ParseState| started.elapsed() >= budget;
    let options = ParseOptions::new().progress_callback(&mut cancelled);
    let tree = parser.parse_with_options(&mut |offset, _| &source[offset..], None, Some(options));
    let Some(tree) = tree else {
        parser.reset();
        result.status = "cancelled".into();
        return result;
    };
    if tree.root_node().has_error() {
        result.status = "parse_error".into();
    }
    let Ok(query) = Query::new(&language, query_source) else {
        result.status = "invalid_query".into();
        return result;
    };
    if !bounded_tree(tree.root_node()) {
        result.status = "depth_limit".into();
        return result;
    }
    let mut bindings = Vec::with_capacity(source.len() / 128);
    bindings::collect(tree.root_node(), source, None, &mut result, &mut bindings);
    let bindings = BindingIndex::new(bindings, &result);
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source);
    while let Some((matched, capture_index)) = captures.next() {
        let node = matched.captures[*capture_index].node;
        observe(node, source, &bindings, &mut result);
    }
    super::syntax_modules::collect(tree.root_node(), source, &mut result);
    result
        .occurrences
        .sort_by_key(|occurrence| (occurrence.start, occurrence.end));
    result
}

fn bounded_tree(root: Node<'_>) -> bool {
    let mut cursor = root.walk();
    let mut depth = 0;
    loop {
        if cursor.goto_first_child() {
            depth += 1;
            if depth > 256 {
                return false;
            }
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return true;
            }
            depth -= 1;
        }
    }
}

fn observe(node: Node<'_>, source: &[u8], bindings: &BindingIndex, result: &mut Extraction) {
    let name = text(node, source);
    if name.is_empty() {
        return;
    }
    if let Some(definition) = bindings.declaration(node) {
        result.occurrences.push(Occurrence {
            name: name.into(),
            start: node.start_byte(),
            end: node.end_byte(),
            role: "declaration".into(),
            target: Some(definition),
            candidates: Vec::new(),
            provenance: "syntax_declaration".into(),
        });
        return;
    }
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if matches!(
            ancestor.kind(),
            "comment" | "line_comment" | "block_comment" | "string" | "string_literal"
        ) {
            return;
        }
        parent = ancestor.parent();
    }
    let qualified = node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "scoped_identifier" | "scoped_type_identifier"
        ) || ["field", "property", "method", "key"].iter().any(|field| {
            parent
                .child_by_field_name(field)
                .is_some_and(|child| child.id() == node.id())
        })
    });
    let role = if ancestor_kind(node, "use_declaration") || ancestor_kind(node, "import_statement")
    {
        "import"
    } else if node.kind() == "type_identifier" {
        "type"
    } else if node.parent().is_some_and(|parent| {
        parent.kind() == "macro_invocation" && parent.child_by_field_name("macro") == Some(node)
    }) {
        "macro"
    } else if is_call(node) {
        "call"
    } else {
        "read"
    };
    let namespace = match role {
        "type" => 2,
        "macro" => 4,
        _ => 1,
    };
    let (mut target, candidates, limited) = bindings.resolve(name, node, namespace);
    if qualified || role == "import" || result.status != "complete" {
        target = None;
    }
    let provenance = if target.is_some() {
        if role == "call" {
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
    if role == "call"
        && let Some(owner) = bindings.owner(node)
    {
        result.relationships.push(Relationship {
            source: owner,
            target,
            kind: "calls".into(),
            evidence_start: node.start_byte(),
            evidence_end: node.end_byte(),
            provenance: provenance.into(),
        });
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

fn is_call(node: Node<'_>) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(parent.kind(), "call_expression" | "function_call") {
            return parent
                .child_by_field_name("function")
                .or_else(|| parent.child_by_field_name("name"))
                .is_some_and(|name| name.id() == current.id());
        }
        if !matches!(
            parent.kind(),
            "field_expression"
                | "member_expression"
                | "dot_index_expression"
                | "method_index_expression"
                | "scoped_identifier"
                | "generic_function"
        ) {
            break;
        }
        // A receiver is a read, even when its member is being called.
        if parent
            .child_by_field_name("value")
            .or_else(|| parent.child_by_field_name("object"))
            .or_else(|| parent.child_by_field_name("table"))
            .is_some_and(|receiver| receiver.id() == current.id())
        {
            return false;
        }
        current = parent;
    }
    false
}

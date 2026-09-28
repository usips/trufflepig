//! Bounded PHP extraction from Tree-sitter's syntax tree.
//! Namespaces and imports normalize names; dynamic dispatch stays unresolved.

mod declarations;
mod imports;
mod names;
mod references;
mod scopes;
#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use tree_sitter::{Language, Node, ParseOptions, Parser};

use super::{Definition, Extraction, Occurrence, Relationship};

pub(super) fn extract(source: &[u8]) -> Extraction {
    extract_with_deadline(source, Duration::from_millis(500))
}

fn extract_with_deadline(source: &[u8], budget: Duration) -> Extraction {
    let mut result = Extraction {
        language: "php".into(),
        status: "complete".into(),
        ..Extraction::default()
    };
    if std::str::from_utf8(source).is_err() {
        result.status = "invalid_encoding".into();
        return result;
    }
    if budget.is_zero() {
        result.status = "cancelled".into();
        return result;
    }
    let language: Language = tree_sitter_php::LANGUAGE_PHP.into();
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
    if !bounded_tree(tree.root_node()) {
        result.status = "depth_limit".into();
        return result;
    }

    let catalog = declarations::collect(tree.root_node(), source, &mut result);
    references::collect(tree.root_node(), source, &catalog, &mut result);
    result
        .occurrences
        .sort_by_key(|occurrence| (occurrence.start, occurrence.end));
    result.relationships.sort_by_key(|relationship| {
        (
            relationship.evidence_start,
            relationship.evidence_end,
            relationship.source,
        )
    });
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

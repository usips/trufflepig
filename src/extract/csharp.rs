//! Bounded C# extraction from Tree-sitter's syntax tree.
//! Names that need overload, receiver, or inheritance analysis stay candidates.

mod declarations;
mod references;
mod scopes;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, ParseOptions, Parser, Query, QueryCursor};

use super::Extraction;

const REFERENCES: &str = "[(identifier) (implicit_parameter)] @reference";

pub(super) fn extract(source: &[u8]) -> Extraction {
    extract_with_deadline(source, Duration::from_millis(500))
}

fn extract_with_deadline(source: &[u8], budget: Duration) -> Extraction {
    let mut result = Extraction {
        language: "csharp".into(),
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
    let language: Language = tree_sitter_c_sharp::LANGUAGE.into();
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
    let Ok(query) = Query::new(&language, REFERENCES) else {
        result.status = "invalid_query".into();
        return result;
    };
    if !bounded_tree(tree.root_node()) {
        result.status = "depth_limit".into();
        return result;
    }

    let mut bindings = Vec::with_capacity(source.len() / 96);
    let mut declarations = Vec::with_capacity(source.len() / 128);
    let mut owners = HashMap::new();
    declarations::collect(
        tree.root_node(),
        source,
        None,
        &mut result,
        &mut bindings,
        &mut declarations,
        &mut owners,
    );
    declarations.sort_by_key(|range| (range.start, range.end));
    let index = super::binding_index::BindingIndex::new(bindings, &result);
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source);
    while let Some((matched, capture_index)) = captures.next() {
        references::observe(
            matched.captures[*capture_index].node,
            source,
            &index,
            &declarations,
            &owners,
            &mut result,
        );
    }
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

use crate::extract::Definition;
use anyhow::Result;
use rusqlite::{Transaction, params};
use std::cmp::Reverse;

const REGION_BYTES: usize = 4096;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegionSpan {
    start: usize,
    end: usize,
    definition: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct DefinitionSpan {
    definition: usize,
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct DefinitionNode {
    span: DefinitionSpan,
    children: Vec<DefinitionNode>,
}

pub(super) fn insert(
    tx: &Transaction<'_>,
    file_id: i64,
    path: &str,
    source: &[u8],
    definitions: &[Definition],
) -> Result<()> {
    for region in partition_source(source, definitions) {
        let (name, kind) = region
            .definition
            .map(|index| {
                let definition = &definitions[index];
                (definition.name.as_str(), definition.kind.as_str())
            })
            .unwrap_or((path, "file"));
        let body = String::from_utf8_lossy(&source[region.start..region.end]);
        tx.execute(
            "INSERT INTO regions(file_id,start,end,name,kind,body) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                file_id,
                region.start as i64,
                region.end as i64,
                name,
                kind,
                body.as_ref()
            ],
        )?;
        let region_id = tx.last_insert_rowid();
        let expanded = expand_identifiers(&format!("{name} {body}"));
        tx.execute(
            "INSERT INTO documents(rowid,name,expanded,body,path) VALUES(?1,?2,?3,?4,?5)",
            params![region_id, name, expanded, body.as_ref(), path],
        )?;
    }
    Ok(())
}

fn partition_source(source: &[u8], definitions: &[Definition]) -> Vec<RegionSpan> {
    if source.is_empty() {
        return Vec::new();
    }

    let spans = valid_definition_spans(source, definitions);
    let roots = definition_forest(&spans);
    let mut preferred = Vec::with_capacity(spans.len());
    for root in &roots {
        collect_preferred_spans(root, &mut preferred);
    }
    preferred.sort_unstable_by_key(|span| (span.start, span.end));

    let mut regions = Vec::with_capacity(source.len().div_ceil(REGION_BYTES));
    let mut cursor = 0;
    let mut preferred_index = 0;
    while cursor < source.len() {
        while preferred_index < preferred.len() && preferred[preferred_index].end <= cursor {
            preferred_index += 1;
        }
        let limit = cursor.saturating_add(REGION_BYTES).min(source.len());
        let mut end = cursor;
        let mut next_preferred = preferred_index;
        while next_preferred < preferred.len() {
            let span = preferred[next_preferred];
            if span.start < cursor {
                next_preferred += 1;
                continue;
            }
            if span.start >= limit {
                end = split_boundary(source, end, limit);
                break;
            }
            if span.end > limit {
                // The preferred span is indivisible. The gap before it is the
                // only safe place to close this region.
                end = span.start;
                break;
            }
            end = span.end;
            next_preferred += 1;
            if end == limit {
                break;
            }
        }
        if end == cursor {
            end = split_boundary(source, cursor, limit);
        } else if end < limit && next_preferred == preferred.len() {
            end = split_boundary(source, end, limit);
        }
        if end > source.len() || end <= cursor {
            // `split_boundary` guarantees progress for a non-empty source,
            // but retaining this guard keeps malformed input from looping.
            end = limit.max(cursor + 1).min(source.len());
        }
        regions.push(RegionSpan {
            start: cursor,
            end,
            definition: containing_definition(cursor, end, &spans),
        });
        cursor = end;
    }
    regions
}

fn valid_definition_spans(source: &[u8], definitions: &[Definition]) -> Vec<DefinitionSpan> {
    let mut spans: Vec<_> = definitions
        .iter()
        .enumerate()
        .filter_map(|(definition, value)| {
            (value.start < value.end
                && value.end <= source.len()
                && is_utf8_boundary(source, value.start)
                && is_utf8_boundary(source, value.end))
            .then_some(DefinitionSpan {
                definition,
                start: value.start,
                end: value.end,
            })
        })
        .collect();
    spans.sort_unstable_by_key(|span| (span.start, Reverse(span.end), span.definition));
    spans.dedup_by_key(|span| (span.start, span.end));
    spans
}

fn definition_forest(spans: &[DefinitionSpan]) -> Vec<DefinitionNode> {
    let mut roots = Vec::new();
    let mut stack: Vec<DefinitionNode> = Vec::new();
    for &span in spans {
        while stack.last().is_some_and(|node| span.start >= node.span.end) {
            let node = stack.pop().expect("checked stack");
            attach_definition_node(&mut roots, &mut stack, node);
        }
        if stack.last().is_some_and(|node| span.end > node.span.end) {
            // Crossing spans cannot form a declaration tree. Drop this span
            // as a structural boundary while retaining it for ownership.
            continue;
        }
        stack.push(DefinitionNode {
            span,
            children: Vec::new(),
        });
    }
    while let Some(node) = stack.pop() {
        attach_definition_node(&mut roots, &mut stack, node);
    }
    roots
}

fn attach_definition_node(
    roots: &mut Vec<DefinitionNode>,
    stack: &mut Vec<DefinitionNode>,
    node: DefinitionNode,
) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        roots.push(node);
    }
}

fn collect_preferred_spans(node: &DefinitionNode, preferred: &mut Vec<RegionSpan>) {
    if node.span.end - node.span.start <= REGION_BYTES {
        preferred.push(RegionSpan {
            start: node.span.start,
            end: node.span.end,
            definition: Some(node.span.definition),
        });
        return;
    }
    for child in &node.children {
        collect_preferred_spans(child, preferred);
    }
}

fn containing_definition(start: usize, end: usize, spans: &[DefinitionSpan]) -> Option<usize> {
    spans
        .iter()
        .filter(|span| span.start <= start && end <= span.end)
        .min_by_key(|span| (span.end - span.start, span.definition))
        .map(|span| span.definition)
}

fn split_boundary(source: &[u8], start: usize, limit: usize) -> usize {
    debug_assert!(start < source.len());
    let newline = source[start..limit]
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map(|offset| start + offset + 1);
    let boundary = newline.unwrap_or(limit);
    if boundary == source.len() || is_utf8_boundary(source, boundary) {
        return boundary;
    }
    let mut candidate = boundary;
    while candidate > start && !is_utf8_boundary(source, candidate) {
        candidate -= 1;
    }
    // A packed declaration may already end at `start`; leave a multibyte
    // character for the next region when the remaining gap cannot hold it.
    candidate
}

fn is_utf8_boundary(source: &[u8], offset: usize) -> bool {
    offset == 0 || offset == source.len() || source[offset] & 0xc0 != 0x80
}

fn expand_identifiers(source: &str) -> String {
    let mut expanded = String::with_capacity(source.len() + source.len() / 4);
    let mut previous = ' ';
    let mut chars = source.chars().peekable();
    while let Some(current) = chars.next() {
        if current == '_' || !current.is_alphanumeric() {
            expanded.push(' ');
        } else {
            if current.is_uppercase()
                && (previous.is_lowercase()
                    || previous.is_ascii_digit()
                    || (previous.is_uppercase()
                        && chars.peek().is_some_and(|next| next.is_lowercase())))
            {
                expanded.push(' ');
            }
            expanded.extend(current.to_lowercase());
        }
        previous = current;
    }
    expanded
}

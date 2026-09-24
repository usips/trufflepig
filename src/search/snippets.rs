//! One-line hit previews: the first line inside a hit's span that mentions a query
//! term (or the hit's name), else its first non-blank line. Previews locate
//! evidence; `show` remains the verified read.
use crate::{
    results::{Hit, Snippet},
    store::Store,
};
use anyhow::Result;
use rusqlite::OptionalExtension;
use std::collections::HashMap;

/// Hits beyond this rank keep coordinates only; pages rarely reach them.
const SNIPPET_HITS: usize = 200;
/// Characters kept from a previewed line before an ellipsis.
const SNIPPET_CHARS: usize = 120;
/// Lines scanned inside one span before falling back to its first line.
const SCAN_LINES: usize = 2_000;

/// Lowercased query words worth locating in a preview line.
pub(super) fn preview_terms(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|term| term.len() >= 2)
        .map(str::to_lowercase)
        .collect()
}

/// Attaches previews to the leading hits that lack one, reading each revision once.
pub(super) fn attach(store: &Store, terms: &[String], hits: &mut [Hit]) -> Result<()> {
    let mut statement = store
        .conn
        .prepare_cached("SELECT bytes FROM contents WHERE revision=?1")?;
    let mut sources: HashMap<String, Option<Vec<u8>>> = HashMap::with_capacity(64);
    for hit in hits.iter_mut().take(SNIPPET_HITS) {
        if hit.snippet.is_some() {
            continue;
        }
        let Some(revision) = hit.revision.clone() else {
            continue;
        };
        if !sources.contains_key(&revision) {
            let bytes = statement
                .query_row([&revision], |row| row.get::<_, Vec<u8>>(0))
                .optional()?;
            sources.insert(revision.clone(), bytes);
        }
        if let Some(Some(bytes)) = sources.get(&revision) {
            hit.snippet = preview(bytes, hit.start, hit.end, hit.start_line, terms, &hit.name);
        }
    }
    Ok(())
}

/// Preview of the span `start..end` (whose first line is `first_line`) in `bytes`.
pub(crate) fn preview(
    bytes: &[u8],
    start: usize,
    end: usize,
    first_line: usize,
    terms: &[String],
    name: &str,
) -> Option<Snippet> {
    let start = start.min(bytes.len());
    let begin = bytes[..start]
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    let stop = end.clamp(start, bytes.len());
    let name = name.to_lowercase();
    // Lower rank wins; ties keep the earliest line. Comment and attribute lines
    // rank below code that mentions the same term.
    let mut best: Option<(u8, usize, &[u8])> = None;
    let mut position = begin;
    for (offset, line) in bytes[begin..]
        .split(|&byte| byte == b'\n')
        .enumerate()
        .take(SCAN_LINES)
    {
        if offset > 0 && position >= stop {
            break;
        }
        position += line.len() + 1;
        let text = String::from_utf8_lossy(line);
        let trimmed = text.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let lowered = text.to_lowercase();
        let term = terms.iter().any(|term| lowered.contains(term.as_str()));
        let named = !name.is_empty() && lowered.contains(&name);
        let comment = ["//", "/*", "*", "#", "--", ";"]
            .iter()
            .any(|marker| trimmed.starts_with(marker));
        let rank = match (term, named, comment) {
            (true, _, false) => 0,
            (false, true, false) => 1,
            (true, _, true) => 2,
            (false, true, true) => 3,
            (false, false, false) => 4,
            (false, false, true) => 5,
        };
        if best.is_none_or(|(old, _, _)| rank < old) {
            best = Some((rank, first_line + offset, line));
            if rank == 0 {
                break;
            }
        }
    }
    let (_, number, line) = best?;
    Some(snippet(number, &String::from_utf8_lossy(line)))
}

fn snippet(line: usize, text: &str) -> Snippet {
    let clean: String = text
        .trim()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let mut text: String = clean.chars().take(SNIPPET_CHARS).collect();
    if clean.chars().count() > SNIPPET_CHARS {
        text.push('…');
    }
    Snippet { line, text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_prefers_a_term_line_then_the_name_then_the_first_line() {
        let source = b"/// Refills tokens.\n#[inline]\npub fn refill(&mut self) {\n    self.tokens = self.capacity;\n}\n";
        let end = source.len();
        let terms = preview_terms("capacity");
        let hit = preview(source, 0, end, 10, &terms, "refill").unwrap();
        assert_eq!((hit.line, hit.text.as_str()), (13, "self.tokens = self.capacity;"));
        // A named definition previews its signature, not its doc comment.
        let hit = preview(source, 0, end, 10, &[], "refill").unwrap();
        assert_eq!((hit.line, hit.text.as_str()), (12, "pub fn refill(&mut self) {"));
        let hit = preview(source, 0, end, 10, &preview_terms("refill"), "").unwrap();
        assert_eq!(hit.line, 12);
        let hit = preview(source, 0, end, 10, &preview_terms("zzz"), "").unwrap();
        assert_eq!(hit.text, "pub fn refill(&mut self) {");
    }

    #[test]
    fn preview_stays_inside_the_span_and_bounds_its_text() {
        let long = format!("a\tb {}\nneedle\n", "x".repeat(200));
        let span_end = long.find('\n').unwrap();
        let hit = preview(long.as_bytes(), 0, span_end, 1, &preview_terms("needle"), "").unwrap();
        assert_eq!(hit.line, 1);
        assert!(hit.text.starts_with("a b "));
        assert_eq!(hit.text.chars().count(), SNIPPET_CHARS + 1);
    }
}

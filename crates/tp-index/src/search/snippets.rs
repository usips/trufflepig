//! One-line hit previews choose the best line among at most 2,000 scanned
//! lines in a hit's span (distinct query terms first; code beats a comment
//! with one more term; earliest on ties), then fall back to a naming line or
//! the first non-blank scanned line. `show` remains the verified read.
use crate::{
    results::{Hit, Snippet},
    store::Store,
};
use anyhow::Result;
use std::cmp::Reverse;

/// Hits beyond this rank keep coordinates only; pages rarely reach them.
const SNIPPET_HITS: usize = 200;
/// Characters kept from a previewed line before an ellipsis.
const SNIPPET_CHARS: usize = 120;
/// Maximum lines scanned inside one span when choosing its best preview line.
const SCAN_LINES: usize = 2_000;

/// Distinct lowercased query words worth locating in a preview line.
pub(super) fn preview_terms(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::with_capacity(8);
    for term in text
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|term| term.len() >= 2)
        .map(str::to_lowercase)
    {
        if !terms.contains(&term) {
            terms.push(term);
        }
    }
    terms
}

/// Attaches previews to leading hits that lack one, reading each revision once.
pub(super) fn attach(store: &Store, terms: &[String], hits: &mut [Hit]) -> Result<()> {
    let mut pending = Vec::with_capacity(hits.len().min(SNIPPET_HITS));
    pending.extend(
        hits.iter_mut()
            .take(SNIPPET_HITS)
            .filter(|hit| hit.snippet.is_none() && hit.revision.is_some()),
    );
    pending.sort_unstable_by(|a, b| a.revision.cmp(&b.revision));

    let mut statement = store
        .conn
        .prepare_cached("SELECT bytes FROM contents WHERE revision=?1")?;
    for group in pending.chunk_by_mut(|a, b| a.revision == b.revision) {
        let mut rows = statement.query([&group[0].revision])?;
        let Some(row) = rows.next()? else {
            continue;
        };
        let bytes = row.get_ref(0)?.as_blob()?;
        for hit in group {
            hit.snippet = preview(bytes, hit.start, hit.end, hit.start_line, terms, &hit.name);
        }
    }
    Ok(())
}

/// Term evidence (more first), then the line class (lower first).
type PreviewRank = (Reverse<usize>, u8);

/// Best preview line in the scanned portion of `start..end`, falling back to
/// its first non-blank scanned line; `first_line` is the line number of `start`.
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
    // Evidence is two points per distinct term plus three for a code line
    // with a term, so code beats a comment with one more term. Then the line
    // class decides; ties keep the earliest line.
    let mut best: Option<(PreviewRank, usize, &[u8])> = None;
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
        let matched = terms
            .iter()
            .filter(|term| lowered.contains(term.as_str()))
            .count();
        let named = !name.is_empty() && lowered.contains(&name);
        let comment = ["//", "/*", "*", "#", "--", ";"]
            .iter()
            .any(|marker| trimmed.starts_with(marker));
        let class = match (matched > 0, named, comment) {
            (true, _, false) => 0,
            (false, true, false) => 1,
            (true, _, true) => 2,
            (false, true, true) => 3,
            (false, false, false) => 4,
            (false, false, true) => 5,
        };
        let evidence = 2 * matched + if matched > 0 && !comment { 3 } else { 0 };
        let rank = (Reverse(evidence), class);
        if best.is_none_or(|(old, _, _)| rank < old) {
            best = Some((rank, first_line + offset, line));
            if matched == terms.len() && class == 0 {
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

    fn hit(revision: Option<String>, start: usize, end: usize, start_line: usize) -> Hit {
        Hit {
            handle: String::new(),
            path: String::new(),
            revision,
            start,
            end,
            start_line,
            end_line: start_line,
            name: String::new(),
            kind: String::new(),
            container: None,
            provenance: None,
            resolution: None,
            candidates: Vec::new(),
            target: None,
            snippet: None,
            repeats: None,
            differs: false,
        }
    }

    fn revision(store: &Store, path: &str) -> String {
        store
            .conn
            .query_row("SELECT revision FROM files WHERE path=?1", [path], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn preview_prefers_a_term_line_then_the_name_then_the_first_line() {
        let source = b"/// Refills tokens.\n#[inline]\npub fn refill(&mut self) {\n    self.tokens = self.capacity;\n}\n";
        let end = source.len();
        let terms = preview_terms("capacity");
        let hit = preview(source, 0, end, 10, &terms, "refill").unwrap();
        assert_eq!(
            (hit.line, hit.text.as_str()),
            (13, "self.tokens = self.capacity;")
        );
        // A named definition previews its signature, not its doc comment.
        let hit = preview(source, 0, end, 10, &[], "refill").unwrap();
        assert_eq!(
            (hit.line, hit.text.as_str()),
            (12, "pub fn refill(&mut self) {")
        );
        let hit = preview(source, 0, end, 10, &preview_terms("refill"), "").unwrap();
        assert_eq!(hit.line, 12);
        let hit = preview(source, 0, end, 10, &preview_terms("zzz"), "").unwrap();
        assert_eq!(hit.text, "pub fn refill(&mut self) {");
    }

    #[test]
    fn preview_stays_inside_the_span_and_bounds_its_text() {
        let long = format!("a\tb {}\nneedle\n", "x".repeat(200));
        let span_end = long.find('\n').unwrap();
        let hit = preview(
            long.as_bytes(),
            0,
            span_end,
            1,
            &preview_terms("needle"),
            "",
        )
        .unwrap();
        assert_eq!(hit.line, 1);
        assert!(hit.text.starts_with("a b "));
        assert_eq!(hit.text.chars().count(), SNIPPET_CHARS + 1);
    }

    #[test]
    fn preview_picks_the_line_with_the_most_distinct_terms() {
        let source = b"fn slot(item: Item) -> Result<()> {\n    // unknown item\n    let slot = find(item)?;\n    Err(fault(\"unknown item slot\"))\n}\n";
        let terms = preview_terms("unknown item slot item");
        assert_eq!(terms, ["unknown", "item", "slot"]);
        let hit = preview(source, 0, source.len(), 1, &terms, "slot").unwrap();
        assert_eq!(
            (hit.line, hit.text.as_str()),
            (4, "Err(fault(\"unknown item slot\"))")
        );
        // Equal counts keep the earliest code line.
        let hit = preview(source, 0, source.len(), 1, &preview_terms("item"), "").unwrap();
        assert_eq!(hit.line, 1);
    }

    #[test]
    fn preview_prefers_code_unless_a_comment_has_two_more_terms() {
        let source = b"// unknown item slot faults\nlet x = item;\n";
        let terms = preview_terms("unknown item slot");
        let hit = preview(source, 0, source.len(), 1, &terms, "").unwrap();
        assert_eq!(hit.line, 1);
        let source = b"// unknown item slot\nlet slot = item.slot();\n";
        let hit = preview(source, 0, source.len(), 1, &terms, "").unwrap();
        assert_eq!(hit.line, 2);
    }

    #[test]
    fn attach_groups_interleaved_revisions_and_preserves_missing_or_prefilled_hits() {
        let first = b"fn alpha() {}\nfn trailing() {}\n";
        let second = b"fn beta() {}\n";
        let (_root, _cache, store) =
            crate::search::tests::fixture(&[("first.rs", first), ("second.rs", second)]);
        let first_revision = revision(&store, "first.rs");
        let second_revision = revision(&store, "second.rs");
        let second_line = b"fn alpha() {}\n".len();
        let mut prefilled = hit(Some(second_revision.clone()), 0, second.len(), 1);
        prefilled.snippet = Some(Snippet {
            line: 99,
            text: "already attached".into(),
        });
        let mut hits = vec![
            hit(Some(first_revision.clone()), 0, first.len(), 1),
            hit(Some(second_revision.clone()), 0, second.len(), 1),
            hit(Some("missing-revision".into()), 0, first.len(), 1),
            hit(Some(first_revision), second_line, first.len(), 2),
            prefilled,
            hit(None, 0, first.len(), 1),
        ];

        attach(&store, &["alpha".into()], &mut hits).unwrap();

        assert_eq!(hits[0].snippet.as_ref().unwrap().text, "fn alpha() {}");
        assert_eq!(hits[1].snippet.as_ref().unwrap().text, "fn beta() {}");
        assert!(hits[2].snippet.is_none());
        assert_eq!(hits[3].snippet.as_ref().unwrap().text, "fn trailing() {}");
        assert_eq!(hits[4].snippet.as_ref().unwrap().line, 99);
        assert!(hits[5].snippet.is_none());
    }

    #[test]
    fn attach_leaves_hits_beyond_the_limit_without_snippets() {
        let source = b"fn capped() {}\n";
        let (_root, _cache, store) = crate::search::tests::fixture(&[("capped.rs", source)]);
        let revision = revision(&store, "capped.rs");
        let mut hits = (0..=SNIPPET_HITS)
            .map(|_| hit(Some(revision.clone()), 0, source.len(), 1))
            .collect::<Vec<_>>();

        attach(&store, &[], &mut hits).unwrap();

        assert!(hits[..SNIPPET_HITS].iter().all(|hit| hit.snippet.is_some()));
        assert!(hits[SNIPPET_HITS].snippet.is_none());
    }
}

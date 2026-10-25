//! Literal byte prefilter for cache-free scans. A file whose path and bytes lack
//! the queried name, or lack every term of each free-text group, cannot match,
//! so it counts as searched without tree-sitter extraction.
use crate::search::{Query, qualified_name::QualifiedName};
use memchr::memmem::Finder;

pub(super) struct LiteralPrefilter {
    shape: PrefilterShape,
    /// Reused ASCII-lowercased copy of the current file for term matching.
    folded: Vec<u8>,
}

enum PrefilterShape {
    /// `sym:` and `refs`: definition and occurrence names compare case-sensitively.
    Name(Box<Finder<'static>>),
    /// Free text matches a file when the lowercase tokens of one group occur in
    /// order, separated only by non-alphanumeric bytes (an FTS phrase, or a
    /// camel-case identifier its expansion splits). Lexical retrieval folds case
    /// and diacritics, so files with foldable characters always pass.
    Terms(Vec<Vec<Finder<'static>>>),
}

impl LiteralPrefilter {
    /// The prefilter a search query admits; `None` reads every eligible file.
    pub(super) fn for_search(query: &Query) -> Option<Self> {
        if query.regex || query.text.is_empty() {
            return None;
        }
        if query.exact {
            return Self::for_name(&query.text);
        }
        let text = query.text.trim();
        let literal = text.len() >= 2
            && text.starts_with('"')
            && text.ends_with('"')
            && !text[1..text.len() - 1].contains('"');
        let body = if literal {
            &text[1..text.len() - 1]
        } else {
            text
        };
        if !body.is_ascii() {
            return None;
        }
        // Terms split like `fts_terms`; each term is an FTS phrase of its tokens.
        let terms = body
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .filter(|term| !term.is_empty());
        let groups: Vec<Vec<Finder<'static>>> = if literal {
            vec![terms.flat_map(term_tokens).collect()]
        } else {
            terms.map(|term| term_tokens(term).collect()).collect()
        };
        let groups: Vec<_> = groups
            .into_iter()
            .filter(|group: &Vec<_>| !group.is_empty())
            .collect();
        (!groups.is_empty()).then(|| Self {
            shape: PrefilterShape::Terms(groups),
            folded: Vec::new(),
        })
    }

    /// Case-sensitive prefilter for the final segment of `NAME` or `A::NAME`.
    pub(super) fn for_name(text: &str) -> Option<Self> {
        let name = QualifiedName::parse(text).name;
        (!name.is_empty()).then(|| Self {
            shape: PrefilterShape::Name(Box::new(Finder::new(name.as_bytes()).into_owned())),
            folded: Vec::new(),
        })
    }

    /// Whether `bytes` (or the path, which names module rows and the path
    /// column) may hold a match and must be extracted.
    pub(super) fn admits(&mut self, path: &str, bytes: &[u8]) -> bool {
        let Self { shape, folded } = self;
        match shape {
            PrefilterShape::Name(finder) => {
                finder.find(path.as_bytes()).is_some() || finder.find(bytes).is_some()
            }
            PrefilterShape::Terms(groups) => {
                // Binary files are never indexed as text, so only their raw bytes count.
                if !bytes.is_ascii() && !bytes.contains(&0) && may_fold_to_ascii(bytes) {
                    return true;
                }
                folded.clear();
                folded.extend(bytes.iter().map(u8::to_ascii_lowercase));
                let path = path.to_ascii_lowercase();
                groups.iter().any(|group| {
                    phrase_occurs(folded, group) || phrase_occurs(path.as_bytes(), group)
                })
            }
        }
    }
}

/// Whether the bytes hold a character FTS `unicode61` may fold into an ASCII
/// letter (Latin letters with diacritics, combining marks, letterlike symbols).
/// Other non-ASCII bytes only ever separate or extend ASCII tokens.
fn may_fold_to_ascii(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).chars().any(|ch| match ch {
        '\u{0300}'..='\u{036f}' => true,
        '\u{00c0}'..='\u{024f}' | '\u{1e00}'..='\u{1eff}' | '\u{2100}'..='\u{214f}' => {
            ch.is_alphabetic()
        }
        _ => false,
    })
}

/// Whether `tokens` occur in order in `haystack` with only non-alphanumeric
/// bytes (possibly none) between consecutive tokens.
fn phrase_occurs(haystack: &[u8], tokens: &[Finder<'static>]) -> bool {
    let Some((first, rest)) = tokens.split_first() else {
        return false;
    };
    first.find_iter(haystack).any(|start| {
        let mut position = start + first.needle().len();
        rest.iter().all(|token| {
            while haystack
                .get(position)
                .is_some_and(|byte| !byte.is_ascii_alphanumeric())
            {
                position += 1;
            }
            let found = haystack[position..].starts_with(token.needle());
            position += token.needle().len();
            found
        })
    })
}

/// The lowercase tokens FTS `unicode61` produces for one ASCII term.
fn term_tokens(term: &str) -> impl Iterator<Item = Finder<'static>> + '_ {
    term.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| Finder::new(token.to_ascii_lowercase().as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admits(query: &str, path: &str, bytes: &str) -> bool {
        let query = Query::parse(query).unwrap();
        LiteralPrefilter::for_search(&query)
            .expect("prefilter")
            .admits(path, bytes.as_bytes())
    }

    #[test]
    fn symbol_names_match_case_sensitively_in_bytes_or_path() {
        assert!(admits("sym:Engine::tick_hz", "a.rs", "fn tick_hz() {}"));
        assert!(!admits("sym:tick_hz", "a.rs", "fn TICK_HZ() {}"));
        assert!(admits("sym:tick_hz", "src/tick_hz.rs", ""));
    }

    #[test]
    fn free_text_needs_every_token_of_one_group_in_any_case() {
        assert!(admits("copy_fixture", "a.rs", "fn CopyFixture() {}"));
        assert!(admits("copy_fixture", "a.rs", "copy::fixture"));
        assert!(!admits("copy_fixture", "a.rs", "fn copy() {}"));
        assert!(!admits("copy_fixture", "a.rs", "copy the fixture"));
        assert!(admits("copy_fixture other", "a.rs", "let OTHER = 1;"));
        assert!(admits("\"copy fixture\"", "src/copy/fixture.rs", ""));
        assert!(!admits("\"copy fixture\"", "src/fixture/copy.rs", ""));
        assert!(!admits("\"copy fixture\"", "a.rs", "copy only"));
        assert!(admits("cafe", "a.rs", "let café = 1;"));
        assert!(admits("copy_fixture", "a.rs", "// copy — fixture"));
        assert!(!admits("copy_fixture", "a.rs", "// 2 × 3"));
        assert!(!admits("copy_fixture", "a.rs", "// copy — the fixture"));
    }

    #[test]
    fn binary_files_skip_the_diacritic_shortcut() {
        let query = Query::parse("copy_fixture").unwrap();
        let mut prefilter = LiteralPrefilter::for_search(&query).expect("prefilter");
        assert!(!prefilter.admits("a.bin", "\0caf\u{e9}".as_bytes()));
        assert!(prefilter.admits("a.bin", "\0copy_fixture".as_bytes()));
    }

    #[test]
    fn regex_filter_only_and_non_ascii_queries_read_every_file() {
        for query in ["re:tick", "kind:function file:src/", "café"] {
            assert!(LiteralPrefilter::for_search(&Query::parse(query).unwrap()).is_none());
        }
    }
}

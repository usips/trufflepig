//! Shared grammar of the `lines` format: tab-separated hit lines, then a
//! `key: value` footer. Errors never use it; they stay JSON objects.

use crate::identity::ResultHandle;

/// Footer keys that end the hit section of a lines page.
pub const COVERAGE_KEY: &str = "coverage: ";
pub const NEXT_KEY: &str = "next: ";
pub const TRUNCATED_LINE: &str = "truncated: true";

/// Page-widening hints; a first page that continues carries the one that applies.
pub const PAGE_SIZE_HINT: &str = "(-n N raises the page size)";
pub const PAGE_BUDGET_HINT: &str = "(-b N raises the token budget)";

/// The command that continues a page, printed as `next: VERB CURSOR[ HINT]`.
#[derive(Clone, Copy, Debug)]
pub struct NextCommand<'a> {
    pub verb: &'a str,
    pub cursor: &'a str,
    pub hint: Option<&'a str>,
}

/// Appends the paging footer shared by hit pages and `show`.
pub fn footer(next: Option<NextCommand<'_>>, truncated: bool, out: &mut String) {
    if let Some(next) = next {
        out.push_str(NEXT_KEY);
        out.push_str(next.verb);
        out.push(' ');
        out.push_str(next.cursor);
        if let Some(hint) = next.hint {
            out.push(' ');
            out.push_str(hint);
        }
        out.push('\n');
    }
    if truncated {
        out.push_str(TRUNCATED_LINE);
        out.push('\n');
    }
}

/// Handles emitted by a lines page, in page order, stopping at the footer.
pub fn emitted_handles(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .take_while(|line| !line.starts_with(COVERAGE_KEY))
        .filter_map(|line| {
            let handle = line.split('\t').next()?;
            handle.parse::<ResultHandle>().ok().map(|_| handle)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_handles_stop_at_coverage_line() {
        let id = uuid::Uuid::nil().simple();
        let text = format!(
            "{id}:1\tsrc/a.rs:1-3\tname\n{id}:2\tsrc/b.rs:4-9\ncoverage: indexed 2/2\n{id}:3\tignored\n"
        );
        let handles: Vec<_> = emitted_handles(&text).collect();
        assert_eq!(handles, vec![format!("{id}:1"), format!("{id}:2")]);
    }

    #[test]
    fn footer_prints_only_present_fields() {
        let mut out = String::new();
        footer(None, false, &mut out);
        assert!(out.is_empty());
        let next = NextCommand {
            verb: "more",
            cursor: "abc@4",
            hint: Some(PAGE_SIZE_HINT),
        };
        footer(Some(next), true, &mut out);
        assert_eq!(
            out,
            "next: more abc@4 (-n N raises the page size)\ntruncated: true\n"
        );
    }
}

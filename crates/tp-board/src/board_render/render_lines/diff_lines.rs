//! Unified diff lines preserve context coordinates and missing-newline markers.

use super::cell;
use crate::board::review_packet::SsotDiff;
use std::fmt::Write;

pub(in crate::board::board_render) fn diff_lines(text: &mut String, diff: &SsotDiff) {
    for hunk in &diff.hunks {
        let context = hunk.context_before.len() + hunk.context_after.len();
        let before_count = hunk.removed.len() + context;
        let after_count = hunk.added.len() + context;
        let before_start =
            hunk.before_start - hunk.context_before.len() - usize::from(before_count == 0);
        let after_start =
            hunk.after_start - hunk.context_before.len() - usize::from(after_count == 0);
        writeln!(
            text,
            "@@ -{},{} +{},{} @@",
            before_start, before_count, after_start, after_count
        )
        .unwrap();
        for (prefix, lines) in [
            (" ", &hunk.context_before),
            ("-", &hunk.removed),
            ("+", &hunk.added),
            (" ", &hunk.context_after),
        ] {
            for line in lines {
                writeln!(
                    text,
                    "{prefix}{}",
                    cell(line.strip_suffix('\n').unwrap_or(line))
                )
                .unwrap();
                if !line.ends_with('\n') {
                    text.push_str("\\ No newline at end of file\n");
                }
            }
        }
    }
    if let Some(next) = &diff.next {
        writeln!(text, "next: {next}").unwrap();
    }
}

//! `lines` rendering of a `show` response: a `PATH [(MEMBER)] lines FIRST-LAST`
//! header, then `LINE<TAB>text` rows, then the footer. Row text is the JSON `text` field,
//! so byte-escaped lines keep their `\xNN` escapes and are flagged once.

use crate::output::lines::{NextCommand, footer};
use serde_json::Value;
use std::fmt::Write;

pub(crate) fn show_text(value: &Value) -> String {
    let rows = value["lines"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut out = String::with_capacity(rows.len() * 80 + 160);
    out.push_str(value["path"].as_str().unwrap_or_default());
    if let Some(member) = value["member"].as_str() {
        let _ = write!(out, " ({member})");
    }
    let line = |row: Option<&Value>| row.and_then(|row| row["line"].as_u64());
    match (line(rows.first()), line(rows.last())) {
        (Some(first), Some(last)) => {
            let _ = writeln!(out, " lines {first}-{last}");
        }
        _ => out.push_str(" (empty)\n"),
    }
    let mut byte_escaped = false;
    for row in rows {
        let escaped = row["encoding"].as_str() == Some("byte-escaped");
        byte_escaped |= escaped;
        let text = row["text"].as_str().unwrap_or_default();
        let text = if escaped {
            text.strip_suffix("\\x0a").unwrap_or(text)
        } else {
            text.strip_suffix('\n').unwrap_or(text)
        };
        let _ = writeln!(out, "{}\t{text}", row["line"].as_u64().unwrap_or(0));
    }
    let next = value["next"].as_str().map(|cursor| NextCommand {
        verb: "show",
        cursor,
        hint: None,
    });
    footer(
        next,
        value["truncated"].as_bool().unwrap_or(false),
        &mut out,
    );
    match (value["verified"].as_bool(), value["source"].as_str()) {
        (Some(false), Some("current_file")) => out.push_str("verified: current file\n"),
        (Some(verified), _) => {
            let _ = writeln!(out, "verified: {verified}");
        }
        _ => {}
    }
    if byte_escaped {
        out.push_str("encoding: byte-escaped\n");
    }
    if let Some(served_from) = value["served_from"].as_str() {
        let _ = writeln!(out, "served_from: {served_from}");
    }
    if let Some(commit) = value["historical"]["commit"].as_str() {
        let _ = writeln!(out, "commit: {commit}");
    }
    let import = &value["import"];
    if let Some(hops) = import["via"].as_array().filter(|hops| !hops.is_empty()) {
        let trail = hops
            .iter()
            .map(|hop| {
                let what = if hop["reexport"] == true {
                    "re-export"
                } else {
                    "import"
                };
                format!(
                    "{what} of {} at {}",
                    hop["path"].as_str().unwrap_or_default(),
                    hop["site"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join(" -> ");
        if import["followed"] == true {
            match import["candidates"].as_u64() {
                Some(count) if count > 1 => {
                    let _ = writeln!(out, "via: {trail} (1 of {count})");
                }
                _ => {
                    let _ = writeln!(out, "via: {trail}");
                }
            }
        } else {
            let _ = writeln!(out, "{trail} (declaration not indexed)");
        }
    }
    if let Some(total) = value["definitions"].as_u64() {
        match value["imports"].as_u64() {
            Some(imports) => {
                let _ = writeln!(out, "definitions: {total} (+{imports} imports)");
            }
            None => {
                let _ = writeln!(out, "definitions: {total}");
            }
        }
    }
    for locator in value["also"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let _ = writeln!(out, "also: {}", locator.as_str().unwrap_or_default());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn show_text_prints_line_range_and_flags_byte_escaped_once() {
        let value = json!({
            "path": "src/a.rs", "member": "lunatic", "revision": "r1",
            "start": 0, "end": 14, "verified": true, "truncated": true,
            "next": "read:abc:1@14",
            "lines": [
                {"line": 1, "start": 0, "end": 6, "text": "fn a()\n", "encoding": "utf8"},
                {"line": 2, "start": 6, "end": 14, "text": "\\xff ok\\x0a", "encoding": "byte-escaped"}
            ]
        });
        assert_eq!(
            show_text(&value),
            "src/a.rs (lunatic) lines 1-2\n1\tfn a()\n2\t\\xff ok\nnext: show read:abc:1@14\ntruncated: true\nverified: true\nencoding: byte-escaped\n"
        );
    }

    #[test]
    fn show_text_counts_imports_apart_and_names_the_import_trail() {
        let value = json!({
            "path": "src/z.rs", "verified": true, "definitions": 1, "imports": 3,
            "also": ["src/y.rs:4-9 function"],
            "import": {"via": [{"site": "src/lib.rs:2", "path": "z::Coord", "reexport": true},
                               {"site": "src/z.rs:1", "path": "inner::Coord", "reexport": false}],
                       "followed": true, "candidates": 2},
            "lines": [{"line": 1, "text": "pub struct Coord;\n", "encoding": "utf8"}]
        });
        assert_eq!(
            show_text(&value),
            "src/z.rs lines 1-1\n1\tpub struct Coord;\nverified: true\nvia: re-export of z::Coord at src/lib.rs:2 -> import of inner::Coord at src/z.rs:1 (1 of 2)\ndefinitions: 1 (+3 imports)\nalso: src/y.rs:4-9 function\n"
        );
        let unfollowed = json!({"path": "a.rs", "definitions": 0, "imports": 1,
            "import": {"via": [{"site": "a.rs:1", "path": "serde::Serialize", "reexport": false}],
                       "followed": false, "candidates": 1},
            "lines": []});
        assert!(
            show_text(&unfollowed)
                .contains("import of serde::Serialize at a.rs:1 (declaration not indexed)\ndefinitions: 0 (+1 imports)\n")
        );
    }

    #[test]
    fn show_text_omits_absent_footer_fields() {
        let value = json!({
            "path": "b.txt", "revision": "r2", "start": 3, "end": 3, "verified": false,
            "truncated": false, "next": null, "lines": []
        });
        assert_eq!(show_text(&value), "b.txt (empty)\nverified: false\n");
        let current = json!({"path": "c.rs", "verified": false, "source": "current_file",
                             "lines": [{"line": 4, "text": "x\n", "encoding": "utf8"}]});
        assert_eq!(
            show_text(&current),
            "c.rs lines 4-4\n4\tx\nverified: current file\n"
        );
    }
}

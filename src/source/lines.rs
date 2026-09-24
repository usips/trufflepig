//! `lines` rendering of a `show` response: a `PATH [(MEMBER)] lines FIRST-LAST`
//! header, then `LINE<TAB>text` rows, then the footer. Row text is the JSON `text` field,
//! so byte-escaped lines keep their `\xNN` escapes and are flagged once.

use crate::output::lines::footer;
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
    footer(
        value["next"].as_str(),
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
    if let Some(commit) = value["historical"]["commit"].as_str() {
        let _ = writeln!(out, "commit: {commit}");
    }
    if let Some(total) = value["definitions"].as_u64() {
        let _ = writeln!(out, "definitions: {total}");
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
            "src/a.rs (lunatic) lines 1-2\n1\tfn a()\n2\t\\xff ok\nnext: read:abc:1@14\ntruncated: true\nverified: true\nencoding: byte-escaped\n"
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

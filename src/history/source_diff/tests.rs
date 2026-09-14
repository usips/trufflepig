use super::*;
use crate::extract::{self, Definition, Extraction};

#[test]
fn histogram_preserves_original_bytes_and_newlines() {
    let before = b"\xff\r\nkeep\nlast";
    let after = b"\xfe\r\nkeep\nlast\n";
    let diff = diff_sources(before, after);
    assert_eq!(diff.changes.len(), 2);
    assert_eq!(diff.changes[0].before, ByteSpan { start: 0, end: 3 });
    assert_eq!(diff.changes[0].after, ByteSpan { start: 0, end: 3 });
    assert_eq!(diff.changes[1].before, ByteSpan { start: 8, end: 12 });
    assert_eq!(diff.changes[1].after, ByteSpan { start: 8, end: 13 });
    assert_eq!(diff.map_before_line(1), Some(1));
    assert_eq!(diff.map_before_line(2), None);
}

#[test]
fn histogram_handles_empty_insertions_deletions_and_crlf() {
    for (before, after) in [
        (&b""[..], &b"a\n"[..]),
        (&b"a\n"[..], &b""[..]),
        (&b"a\n"[..], &b"a\r\n"[..]),
        (&b"a b\n"[..], &b"a  b\n"[..]),
    ] {
        let diff = diff_sources(before, after);
        assert_eq!(diff.changes.len(), 1);
        assert_eq!(
            diff.changes[0].before,
            ByteSpan {
                start: 0,
                end: before.len()
            }
        );
        assert_eq!(
            diff.changes[0].after,
            ByteSpan {
                start: 0,
                end: after.len()
            }
        );
    }
    assert!(diff_sources(b"", b"").changes.is_empty());
}

fn declarations(before: &[u8], after: &[u8]) -> Vec<DeclarationCorrespondence> {
    correspond_declarations(
        before,
        after,
        &extract::extract("a.rs", before),
        &extract::extract("a.rs", after),
        &diff_sources(before, after),
    )
}

#[test]
fn declaration_correspondence_preserves_duplicate_occurrences() {
    let before = b"fn duplicate() {}\nfn duplicate() {}\n";
    let after = b"// inserted\nfn duplicate() {}\nfn duplicate() {}\n";
    let rows = declarations(before, after);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].before, Some(0));
    assert_eq!(rows[0].after, Some(0));
    assert_eq!(rows[1].before, Some(1));
    assert_eq!(rows[1].after, Some(1));
    assert!(
        rows.iter()
            .all(|row| row.relation == DeclarationRelation::Unchanged)
    );
}

#[test]
fn declaration_correspondence_distinguishes_edit_move_and_deletion() {
    let rows = declarations(b"fn edited() { old(); }\n", b"fn edited() { new(); }\n");
    assert_eq!(rows[0].relation, DeclarationRelation::Modified);
    let rows = declarations(
        b"fn alpha() {}\nfn beta() {}\n",
        b"fn beta() {}\nfn alpha() {}\n",
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.relation == DeclarationRelation::Moved)
            .count(),
        1
    );
    let rows = declarations(b"fn removed() {}\n", b"");
    assert_eq!(rows[0].relation, DeclarationRelation::Deleted);
}

#[test]
fn incomplete_extraction_cannot_prove_deletion_or_addition() {
    let before = b"fn present() {}\n";
    let extraction = extract::extract("a.rs", before);
    for status in [
        "parse_error",
        "cancelled",
        "recovered",
        "recovered_incomplete",
        "invalid_encoding",
    ] {
        let incomplete = Extraction {
            status: status.into(),
            ..Extraction::default()
        };
        let rows = correspond_declarations(
            before,
            b"",
            &extraction,
            &incomplete,
            &diff_sources(before, b""),
        );
        assert_eq!(rows[0].relation, DeclarationRelation::Uncertain);
        let rows = correspond_declarations(
            b"",
            before,
            &incomplete,
            &extraction,
            &diff_sources(b"", before),
        );
        assert_eq!(rows[0].relation, DeclarationRelation::Uncertain);
    }
}

#[test]
fn duplicate_edited_declarations_remain_selectable_candidates() {
    let rows = declarations(
        b"fn dup() { old(); }\nfn dup() { old(); }\n",
        b"fn dup() { new(); }\nfn dup() { new(); }\n",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].candidates, vec![0, 1]);
    assert_eq!(rows[1].candidates, vec![0, 1]);
    assert!(
        rows.iter()
            .all(|row| row.relation == DeclarationRelation::Uncertain)
    );
}

#[test]
fn matching_names_without_positional_or_content_evidence_stay_uncertain() {
    let before = b"old\nanchor\n";
    let after = b"anchor\nnew\n";
    let extraction = |start, end| Extraction {
        status: "complete".into(),
        definitions: vec![Definition {
            name: "same".into(),
            kind: "function".into(),
            start,
            end,
            container: None,
        }],
        ..Extraction::default()
    };
    let rows = correspond_declarations(
        before,
        after,
        &extraction(0, 3),
        &extraction(7, 10),
        &diff_sources(before, after),
    );
    assert_eq!(rows[0].relation, DeclarationRelation::Uncertain);
    assert_eq!(rows[0].candidates, vec![0]);
    assert_eq!(rows[1].relation, DeclarationRelation::Uncertain);
}

#[test]
fn duplicate_candidate_explosion_is_bounded_and_explicit() {
    let before = "fn dup() { old(); }\n".repeat(200);
    let after = "fn dup() { new(); }\n".repeat(200);
    let rows = declarations(before.as_bytes(), after.as_bytes());
    assert_eq!(rows.len(), 400);
    assert!(rows.iter().map(|row| row.candidates.len()).sum::<usize>() <= 4096);
    assert!(rows[..200].iter().all(|row| row.candidates_truncated));
    assert!(
        rows.iter()
            .all(|row| row.relation == DeclarationRelation::Uncertain)
    );
}

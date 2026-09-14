use super::*;
use crate::{
    extract,
    history::source_diff::{DeclarationRelation, correspond_declarations, diff_sources},
};

fn facts(bytes: &[u8], extraction: &extract::Extraction) -> Vec<FingerprintFact> {
    extraction
        .definitions
        .iter()
        .map(|definition| FingerprintFact {
            key: ContentRevision::of(
                &serde_json::to_vec(&(&definition.name, &definition.kind, &definition.container))
                    .unwrap(),
            ),
            span: ByteSpan {
                start: definition.start,
                end: definition.end,
            },
            fingerprint: ContentRevision::of(&bytes[definition.start..definition.end]),
        })
        .collect()
}

#[test]
fn fingerprint_and_original_byte_histograms_and_occurrences_agree() {
    for (before, after) in [
        (&b"\xff\r\nkeep\nlast"[..], &b"\xfe\r\nkeep\nlast\n"[..]),
        (b"", b"fn added() {}\n"),
        (b"fn removed() {}\n", b""),
        (
            b"fn dup() {}\nfn dup() {}\n",
            b"// shift\nfn dup() {}\nfn dup() {}\n",
        ),
        (b"fn edited() { old(); }\n", b"fn edited() { new(); }\n"),
        (
            b"fn alpha() {}\nfn beta() {}\n",
            b"fn beta() {}\nfn alpha() {}\n",
        ),
        (
            b"fn dup() { old(); }\nfn dup() { old(); }\n",
            b"fn dup() { new(); }\nfn dup() { new(); }\n",
        ),
    ] {
        let before_lines = fingerprint_lines(before);
        let after_lines = fingerprint_lines(after);
        let original_diff = diff_sources(before, after);
        let hashed_diff = diff_fingerprints(&before_lines, &after_lines).unwrap();
        assert_eq!(original_diff.changes, hashed_diff.changes);
        assert_eq!(original_diff.unchanged_lines, hashed_diff.unchanged_lines);
        let old = extract::extract("a.rs", before);
        let new = extract::extract("a.rs", after);
        let original_matches = correspond_declarations(before, after, &old, &new, &original_diff);
        let hashed_matches = correspond_fingerprints(
            &facts(before, &old),
            &facts(after, &new),
            old.status == "complete",
            new.status == "complete",
            Some(&before_lines),
            Some(&after_lines),
        )
        .unwrap();
        assert_eq!(original_matches, hashed_matches);
    }
}

#[test]
fn fingerprint_only_matches_require_unambiguous_content() {
    let fact = FingerprintFact {
        key: ContentRevision::of(b"name"),
        fingerprint: ContentRevision::of(b"body"),
        span: ByteSpan { start: 0, end: 4 },
    };
    let rows = correspond_fingerprints(&[fact], &[fact], true, true, None, None).unwrap();
    assert_eq!(rows[0].after, Some(0));
    let rows =
        correspond_fingerprints(&[fact, fact], &[fact, fact], true, true, None, None).unwrap();
    assert!(
        rows.iter()
            .all(|row| row.relation == DeclarationRelation::Uncertain)
    );
    let rows = correspond_fingerprints(&[fact], &[], true, false, None, None).unwrap();
    assert_eq!(rows[0].relation, DeclarationRelation::Uncertain);
}

#[test]
fn fingerprint_snapshots_reject_invalid_coordinates() {
    let mut lines = fingerprint_lines(b"line\n");
    lines.offsets[1] = 0;
    assert!(diff_fingerprints(&lines, &lines).is_err());
    let lines = fingerprint_lines(b"line\n");
    let fact = FingerprintFact {
        key: ContentRevision::of(b"name"),
        fingerprint: ContentRevision::of(b"body"),
        span: ByteSpan { start: 0, end: 10 },
    };
    assert!(correspond_fingerprints(&[fact], &[], true, true, Some(&lines), Some(&lines)).is_err());
}

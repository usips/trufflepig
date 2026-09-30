//! Current-bytes definition lookup for files whose published revision is behind
//! disk: extracts one file in memory without touching any index.
use super::{line_span, read_contained};
use crate::{
    extract::Extraction,
    identity::{ByteSpan, ContentRevision},
    results::Hit,
};
use anyhow::{Result, bail, ensure};
use std::path::Path;

/// One definition located in a file's current bytes; unpublished, so unverified
/// against any index generation.
pub struct ReextractedDefinition {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub container: Option<String>,
    pub span: ByteSpan,
    pub start_line: usize,
    pub end_line: usize,
    /// Revision of `bytes`, the file as read for this extraction.
    pub revision: ContentRevision,
    pub bytes: Vec<u8>,
}

impl ReextractedDefinition {
    /// A live hit for these current bytes, with provenance `reextracted`.
    pub fn hit(&self) -> Hit {
        Hit {
            handle: String::new(),
            path: self.path.clone(),
            revision: Some(self.revision.to_string()),
            start: self.span.start,
            end: self.span.end,
            start_line: self.start_line,
            end_line: self.end_line,
            name: self.name.clone(),
            kind: self.kind.clone(),
            container: self.container.clone(),
            provenance: Some("reextracted".into()),
            resolution: None,
            candidates: Vec::new(),
            target: None,
            repeats: None,
            snippet: None,
        }
    }
}

/// Re-reads `hit`'s root-relative path and finds the same named definition in
/// the same container nearest its recorded line. Earlier definitions win ties.
/// Files the index excludes and factless failed extractions are errors, not
/// `None`.
pub fn reextract_definition(root: &Path, hit: &Hit) -> Result<Option<ReextractedDefinition>> {
    let bytes = read_contained(
        root,
        &crate::store::decode_path(&hit.path)?,
        crate::store::MAX_SOURCE_BYTES as usize,
    )?;
    ensure!(
        !bytes.contains(&0),
        "source_excluded: {} is binary; the index does not extract it",
        hit.path
    );
    let extraction = usable_extraction(&hit.path, crate::extract::extract(&hit.path, &bytes))?;
    let Some(definition) = extraction
        .definitions
        .into_iter()
        .filter(|definition| {
            definition.name == hit.name
                && (hit.kind.is_empty() || definition.kind == hit.kind)
                && definition.container.as_deref() == hit.container.as_deref()
        })
        .map(|definition| {
            let lines = line_span(&bytes, definition.start, definition.end);
            (
                lines.0.abs_diff(hit.start_line),
                definition.start,
                lines,
                definition,
            )
        })
        .min_by_key(|(distance, start, _, _)| (*distance, *start))
        .map(|(_, _, lines, definition)| (lines, definition))
    else {
        return Ok(None);
    };
    let ((start_line, end_line), definition) = definition;
    Ok(Some(ReextractedDefinition {
        path: hit.path.clone(),
        span: ByteSpan::new(definition.start, definition.end)?.validate(bytes.len())?,
        name: definition.name,
        kind: definition.kind,
        container: definition.container,
        start_line,
        end_line,
        revision: ContentRevision::of(&bytes),
        bytes,
    }))
}

/// A cancelled extraction, or one that failed without any facts, leaves the
/// file's definitions unknown rather than absent.
pub fn usable_extraction(path: &str, extraction: Extraction) -> Result<Extraction> {
    let factless_failure = extraction.definitions.is_empty()
        && !matches!(extraction.status.as_str(), "complete" | "lexical_only");
    if extraction.status == "cancelled" || factless_failure {
        bail!(
            "extraction_unavailable: {path} extraction {}; its definitions are unknown",
            extraction.status
        );
    }
    Ok(extraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, kind: &str, container: Option<&str>, start_line: usize) -> Hit {
        Hit {
            handle: String::new(),
            path: "lib.rs".into(),
            revision: None,
            start: 0,
            end: 0,
            start_line,
            end_line: start_line,
            name: name.into(),
            kind: kind.into(),
            container: container.map(str::to_owned),
            provenance: None,
            resolution: None,
            candidates: Vec::new(),
            target: None,
            repeats: None,
            snippet: None,
        }
    }

    #[test]
    fn reextract_picks_nearest_definition_in_the_original_container() {
        let root = tempfile::tempdir().unwrap();
        let source = "fn helper() {}\n\nstruct helper;\n\nmod inner {\n    fn helper() {}\n}\n\nfn other() {}\n";
        std::fs::write(root.path().join("lib.rs"), source).unwrap();
        let selected = hit("helper", "function", Some("inner"), 5);
        let found = reextract_definition(root.path(), &selected)
            .unwrap()
            .unwrap();
        assert_eq!((found.start_line, found.end_line), (6, 6));
        assert_eq!(found.kind, "function");
        assert_eq!(found.container.as_deref(), Some("inner"));
        assert_eq!(
            &found.bytes[found.span.start..found.span.end],
            b"fn helper() {}"
        );
        assert_eq!(found.revision, ContentRevision::of(source.as_bytes()));
        let first = reextract_definition(root.path(), &hit("helper", "function", None, 1))
            .unwrap()
            .unwrap();
        assert_eq!(first.start_line, 1);
        let any_kind = reextract_definition(root.path(), &hit("helper", "", None, 3))
            .unwrap()
            .unwrap();
        assert_eq!(any_kind.start_line, 3);
        assert!(
            reextract_definition(root.path(), &hit("missing", "", None, 1))
                .unwrap()
                .is_none()
        );
        std::fs::write(root.path().join("blob.rs"), b"fn helper() {}\0").unwrap();
        let mut binary_hit = hit("helper", "", None, 1);
        binary_hit.path = "blob.rs".into();
        let binary = reextract_definition(root.path(), &binary_hit)
            .err()
            .unwrap();
        assert!(
            binary.to_string().starts_with("source_excluded"),
            "{binary:#}"
        );
        let cancelled = Extraction {
            status: "cancelled".into(),
            ..Extraction::default()
        };
        let error = usable_extraction("lib.rs", cancelled).err().unwrap();
        assert!(
            error.to_string().contains("extraction cancelled"),
            "{error:#}"
        );
        let hit = found.hit();
        assert_eq!(hit.provenance.as_deref(), Some("reextracted"));
        assert_eq!(hit.revision, Some(found.revision.to_string()));
    }
}

use crate::{results::Hit, source, store::Store};
use rusqlite::{Row, Statement};

/// Builds a hit from metadata columns and fetches source bytes only when its
/// revision differs from the one cached by this reader.
pub(super) struct HitRowReader<'conn> {
    contents: Statement<'conn>,
    cached_revision: Option<String>,
    cached_bytes: Vec<u8>,
}

impl<'conn> HitRowReader<'conn> {
    pub(super) fn new(store: &'conn Store) -> rusqlite::Result<Self> {
        Ok(Self {
            contents: store
                .conn
                .prepare("SELECT bytes FROM contents WHERE revision=?1")?,
            cached_revision: None,
            cached_bytes: Vec::new(),
        })
    }

    pub(super) fn read(&mut self, row: &Row<'_>) -> rusqlite::Result<Hit> {
        let revision: String = row.get(1)?;
        let start = row.get::<_, i64>(2)? as usize;
        let end = row.get::<_, i64>(3)? as usize;
        if self.cached_revision.as_deref() != Some(revision.as_str()) {
            self.contents.query_row([&revision], |content| {
                let bytes = content.get_ref(0)?.as_blob()?;
                self.cached_bytes.clear();
                self.cached_bytes.extend_from_slice(bytes);
                Ok(())
            })?;
            self.cached_revision = Some(revision.clone());
        }
        let (start_line, end_line) = source::line_span(&self.cached_bytes, start, end);
        Ok(Hit {
            handle: String::new(),
            path: row.get(0)?,
            revision: Some(revision),
            start,
            end,
            start_line,
            end_line,
            name: row.get(4)?,
            kind: row.get(5)?,
            container: row.get(6)?,
            provenance: Some(row.get(7)?),
            resolution: None,
            candidates: Vec::new(),
            target: None,
            repeats: None,
            snippet: None,
            differs: false,
        })
    }
}

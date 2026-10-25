//! Plans a diff publication: pairs staged and live files by path, maps every
//! staged definition id to a live id, and finds the files whose definitions,
//! occurrences, or relationships differ from the live index.
use super::definition_matching::{DefinitionRewrite, map_definitions};
use super::fact_digest::{RowEncoder, RowSetDigest, StagedIdMap, translate_candidates};
use anyhow::Result;
use rusqlite::{Connection, Row};
use std::collections::HashMap;

/// One `files` row of the live index.
pub(super) struct LiveFile {
    pub(super) id: i64,
    pub(super) revision: Option<String>,
    language: String,
    status: String,
    bytes: i64,
    stat: [Option<i64>; 5],
}

/// One staged `files` row paired with its live counterpart by path.
pub(super) struct StagedFile {
    pub(super) id: i64,
    pub(super) path: String,
    pub(super) revision: Option<String>,
    pub(super) language: String,
    pub(super) status: String,
    pub(super) bytes: i64,
    pub(super) stat: [Option<i64>; 5],
    /// Bytes, extraction cache rows, and regions were staged for this file.
    pub(super) restaged: bool,
    /// The existing live id, or a fresh one for an added path.
    pub(super) live_id: i64,
    pub(super) previous: Option<LiveFile>,
}

impl StagedFile {
    /// Whether the live row already holds this file's columns.
    pub(super) fn row_unchanged(&self) -> bool {
        self.previous.as_ref().is_some_and(|live| {
            live.revision == self.revision
                && live.language == self.language
                && live.status == self.status
                && live.bytes == self.bytes
                && live.stat == self.stat
        })
    }
}

pub(super) struct PublicationDiff {
    pub(super) files: Vec<StagedFile>,
    pub(super) removed: Vec<LiveFile>,
    pub(super) definitions: StagedIdMap,
    pub(super) definition_rewrites: Vec<(usize, DefinitionRewrite)>,
    pub(super) dirty_occurrences: Vec<usize>,
    pub(super) dirty_relationships: Vec<usize>,
}

pub(super) fn plan(conn: &Connection) -> Result<PublicationDiff> {
    let (files, removed) = pair_files(conn)?;
    let (definitions, definition_rewrites) = map_definitions(conn, &files)?;
    let mut encoder = RowEncoder::default();
    let mut candidates = String::new();
    let live = row_sets(
        conn,
        "SELECT file_id,name,start,end,role,target,candidates,provenance FROM main.occurrences",
        |row, encoder| encode_occurrence(row, encoder, row.get(5)?, &row.get::<_, String>(6)?),
        &mut encoder,
    )?;
    let staged = row_sets(
        conn,
        "SELECT file_id,name,start,end,role,target,candidates,provenance FROM staged.occurrences",
        |row, encoder| {
            translate_candidates(&row.get::<_, String>(6)?, &definitions, &mut candidates)?;
            encode_occurrence(
                row,
                encoder,
                definitions.get_optional(row.get(5)?)?,
                &candidates,
            )
        },
        &mut encoder,
    )?;
    let dirty_occurrences = dirty_files(&files, &staged, &live);
    let live = row_sets(
        conn,
        "SELECT file_id,source,target,kind,start,end,provenance FROM main.relationships",
        |row, encoder| encode_relationship(row, encoder, row.get(1)?, row.get(2)?),
        &mut encoder,
    )?;
    let staged = row_sets(
        conn,
        "SELECT file_id,source,target,kind,start,end,provenance FROM staged.relationships",
        |row, encoder| {
            let source = definitions.get(row.get(1)?)?;
            encode_relationship(row, encoder, source, definitions.get_optional(row.get(2)?)?)
        },
        &mut encoder,
    )?;
    let dirty_relationships = dirty_files(&files, &staged, &live);
    Ok(PublicationDiff {
        files,
        removed,
        definitions,
        definition_rewrites,
        dirty_occurrences,
        dirty_relationships,
    })
}

fn pair_files(conn: &Connection) -> Result<(Vec<StagedFile>, Vec<LiveFile>)> {
    let mut live = HashMap::new();
    let mut statement = conn.prepare(
        "SELECT id,path,revision,language,status,bytes,stat_size,stat_mtime_ns,stat_ctime_ns,stat_inode,stat_device FROM main.files",
    )?;
    let mut rows = statement.query([])?;
    let mut next_id = 1;
    while let Some(row) = rows.next()? {
        let file = LiveFile {
            id: row.get(0)?,
            revision: row.get(2)?,
            language: row.get(3)?,
            status: row.get(4)?,
            bytes: row.get(5)?,
            stat: stat_columns(row, 6)?,
        };
        next_id = next_id.max(file.id + 1);
        live.insert(row.get::<_, String>(1)?, file);
    }
    let mut statement = conn.prepare(
        "SELECT f.id,f.path,f.revision,f.language,f.status,f.bytes,f.stat_size,f.stat_mtime_ns,f.stat_ctime_ns,
                f.stat_inode,f.stat_device,r.file_id IS NOT NULL
         FROM staged.files f LEFT JOIN staged.restaged r ON r.file_id=f.id ORDER BY f.id",
    )?;
    let mut rows = statement.query([])?;
    let mut files = Vec::with_capacity(live.len() + 16);
    while let Some(row) = rows.next()? {
        let path: String = row.get(1)?;
        let previous = live.remove(&path);
        let live_id = match &previous {
            Some(previous) => previous.id,
            None => {
                next_id += 1;
                next_id - 1
            }
        };
        files.push(StagedFile {
            id: row.get(0)?,
            path,
            revision: row.get(2)?,
            language: row.get(3)?,
            status: row.get(4)?,
            bytes: row.get(5)?,
            stat: stat_columns(row, 6)?,
            restaged: row.get(11)?,
            live_id,
            previous,
        });
    }
    Ok((files, live.into_values().collect()))
}

fn stat_columns(row: &Row<'_>, first: usize) -> rusqlite::Result<[Option<i64>; 5]> {
    Ok([
        row.get(first)?,
        row.get(first + 1)?,
        row.get(first + 2)?,
        row.get(first + 3)?,
        row.get(first + 4)?,
    ])
}

fn encode_occurrence(
    row: &Row<'_>,
    encoder: &mut RowEncoder,
    target: Option<i64>,
    candidates: &str,
) -> Result<()> {
    encoder
        .begin()
        .text(Some(&row.get::<_, String>(1)?))
        .integer(Some(row.get(2)?))
        .integer(Some(row.get(3)?))
        .text(Some(&row.get::<_, String>(4)?))
        .integer(target)
        .text(Some(candidates))
        .text(Some(&row.get::<_, String>(7)?));
    Ok(())
}

fn encode_relationship(
    row: &Row<'_>,
    encoder: &mut RowEncoder,
    source: i64,
    target: Option<i64>,
) -> Result<()> {
    encoder
        .begin()
        .integer(Some(source))
        .integer(target)
        .text(Some(&row.get::<_, String>(3)?))
        .integer(Some(row.get(4)?))
        .integer(Some(row.get(5)?))
        .text(Some(&row.get::<_, String>(6)?));
    Ok(())
}

/// Per-file row-set digests of one table; column 0 is `file_id`.
fn row_sets(
    conn: &Connection,
    query: &str,
    mut encode: impl FnMut(&Row<'_>, &mut RowEncoder) -> Result<()>,
    encoder: &mut RowEncoder,
) -> Result<HashMap<i64, RowSetDigest>> {
    let mut digests: HashMap<i64, RowSetDigest> = HashMap::new();
    let mut statement = conn.prepare(query)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        encode(row, encoder)?;
        digests.entry(row.get(0)?).or_default().add(encoder.hash());
    }
    Ok(digests)
}

/// Indexes of staged files whose rows differ from their live counterpart's.
fn dirty_files(
    files: &[StagedFile],
    staged: &HashMap<i64, RowSetDigest>,
    live: &HashMap<i64, RowSetDigest>,
) -> Vec<usize> {
    let empty = RowSetDigest::default();
    files
        .iter()
        .enumerate()
        .filter(|(_, file)| {
            let live = file
                .previous
                .as_ref()
                .and_then(|previous| live.get(&previous.id))
                .unwrap_or(&empty);
            staged.get(&file.id).unwrap_or(&empty) != live
        })
        .map(|(index, _)| index)
        .collect()
}

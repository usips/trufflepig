//! Maps staged definition ids to live ids for a diff publication. A file whose
//! definition sequence is unchanged keeps its ids in order; other files match
//! definitions by name, kind, and container, so references into an edited
//! file keep pointing at the same live rows.
use super::fact_digest::{RowEncoder, RowSequenceDigest, StagedIdMap};
use super::publication_diff::StagedFile;
use anyhow::Result;
use rusqlite::{Connection, Row};
use std::collections::{HashMap, VecDeque};

/// Definition edits for one file whose definitions differ: matched rows keep
/// their live ids and move to new spans; the rest are inserted or deleted.
#[derive(Default)]
pub(super) struct DefinitionRewrite {
    pub(super) moved: Vec<(i64, i64, i64)>,
    pub(super) inserted: Vec<(i64, i64)>,
    pub(super) deleted: Vec<i64>,
}

/// One file's definitions in id order.
#[derive(Default)]
struct DefinitionSequence {
    digest: RowSequenceDigest,
    ids: Vec<i64>,
}

fn definition_sequences(
    conn: &Connection,
    schema: &str,
) -> Result<HashMap<i64, DefinitionSequence>> {
    let mut sequences: HashMap<i64, DefinitionSequence> = HashMap::new();
    let mut encoder = RowEncoder::default();
    let mut statement = conn.prepare(&format!(
        "SELECT id,file_id,name,kind,start,end,container FROM {schema}.definitions ORDER BY id"
    ))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let sequence = sequences.entry(row.get(1)?).or_default();
        encode_definition(row, encoder.begin())?;
        sequence.digest.push(encoder.hash());
        sequence.ids.push(row.get(0)?);
    }
    Ok(sequences)
}

fn encode_definition(row: &Row<'_>, encoder: &mut RowEncoder) -> Result<()> {
    encoder
        .text(Some(&row.get::<_, String>(2)?))
        .text(Some(&row.get::<_, String>(3)?))
        .integer(Some(row.get(4)?))
        .integer(Some(row.get(5)?))
        .text(row.get::<_, Option<String>>(6)?.as_deref());
    Ok(())
}

/// Files with an identical definition sequence keep their live ids in order;
/// others match definitions by name, kind, and container.
pub(super) fn map_definitions(
    conn: &Connection,
    files: &[StagedFile],
) -> Result<(StagedIdMap, Vec<(usize, DefinitionRewrite)>)> {
    let staged_sequences = definition_sequences(conn, "staged")?;
    let live_sequences = definition_sequences(conn, "main")?;
    let max_staged: i64 = conn.query_row(
        "SELECT coalesce(max(id),0) FROM staged.definitions",
        [],
        |row| row.get(0),
    )?;
    let mut next_id: i64 = conn.query_row(
        "SELECT coalesce(max(id),0)+1 FROM main.definitions",
        [],
        |row| row.get(0),
    )?;
    let mut map = StagedIdMap::with_max_staged_id(max_staged)?;
    let mut rewrites = Vec::new();
    let empty = DefinitionSequence::default();
    for (index, file) in files.iter().enumerate() {
        let staged = staged_sequences.get(&file.id).unwrap_or(&empty);
        let live = file
            .previous
            .as_ref()
            .and_then(|previous| live_sequences.get(&previous.id))
            .unwrap_or(&empty);
        if staged.digest == live.digest {
            for (staged_id, live_id) in staged.ids.iter().zip(&live.ids) {
                map.insert(*staged_id, *live_id)?;
            }
            continue;
        }
        let rewrite = match_definitions(conn, file, &mut map, &mut next_id)?;
        rewrites.push((index, rewrite));
    }
    Ok((map, rewrites))
}

type DefinitionKey = (String, String, Option<String>);

fn match_definitions(
    conn: &Connection,
    file: &StagedFile,
    map: &mut StagedIdMap,
    next_id: &mut i64,
) -> Result<DefinitionRewrite> {
    let mut live: HashMap<DefinitionKey, VecDeque<(i64, i64, i64)>> = HashMap::new();
    if let Some(previous) = &file.previous {
        let mut statement = conn.prepare_cached(
            "SELECT id,name,kind,start,end,container FROM main.definitions WHERE file_id=?1 ORDER BY id",
        )?;
        let mut rows = statement.query([previous.id])?;
        while let Some(row) = rows.next()? {
            live.entry((row.get(1)?, row.get(2)?, row.get(5)?))
                .or_default()
                .push_back((row.get(0)?, row.get(3)?, row.get(4)?));
        }
    }
    let mut rewrite = DefinitionRewrite::default();
    let mut statement = conn.prepare_cached(
        "SELECT id,name,kind,start,end,container FROM staged.definitions WHERE file_id=?1 ORDER BY id",
    )?;
    let mut rows = statement.query([file.id])?;
    while let Some(row) = rows.next()? {
        let staged_id: i64 = row.get(0)?;
        let (start, end): (i64, i64) = (row.get(3)?, row.get(4)?);
        let key = (row.get(1)?, row.get(2)?, row.get(5)?);
        match live.get_mut(&key).and_then(VecDeque::pop_front) {
            Some((live_id, live_start, live_end)) => {
                map.insert(staged_id, live_id)?;
                if (live_start, live_end) != (start, end) {
                    rewrite.moved.push((live_id, start, end));
                }
            }
            None => {
                map.insert(staged_id, *next_id)?;
                rewrite.inserted.push((staged_id, *next_id));
                *next_id += 1;
            }
        }
    }
    rewrite.deleted = live.into_values().flatten().map(|(id, _, _)| id).collect();
    Ok(rewrite)
}

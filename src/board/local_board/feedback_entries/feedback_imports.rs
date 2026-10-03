//! Permanent feedback import aliases and replay receipts.

use super::*;

/// Aliases include content-deduped requests, so their UUIDs never expire.
pub(in crate::board::local_board) fn remember_import(
    tx: &Transaction<'_>,
    key: &FeedbackImportKey,
    entry: EntryId,
) -> Result<(), BoardError> {
    tx.execute(
        "INSERT OR IGNORE INTO feedback_imports(import_key,entry_id) VALUES(?1,?2)",
        params![key.to_string(), sqlite_id(entry.get())?],
    )
    .map_err(sql_error)?;
    Ok(())
}

pub(in crate::board::local_board) fn imported_reply(
    conn: &Connection,
    key: &FeedbackImportKey,
) -> Result<Option<BoardReply>, BoardError> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT entry_id FROM feedback_imports WHERE import_key=?1",
            [key.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    id.map(|id| {
        let entry = read_entry(
            conn,
            EntryId::new(sqlite_u64(id)?).map_err(BoardError::from)?,
        )?;
        Ok(BoardReply::new(
            "local",
            BoardResult::Change(BoardChange {
                entry: entry.id,
                seq: entry.seq,
                plan: entry.plan,
                revision: None,
                task: None,
                deduplicated: true,
            }),
        ))
    })
    .transpose()
}

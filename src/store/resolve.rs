use anyhow::Result;
use rusqlite::Connection;

/// Global name matches remain candidates even when only one definition matches.
pub(super) fn references(conn: &mut Connection) -> Result<()> {
    let transaction = conn.transaction()?;
    transaction.execute_batch(
        "UPDATE occurrences AS occurrence
         SET candidates=(SELECT json_group_array(id) FROM (
             SELECT definition.id FROM definitions AS definition
             JOIN files AS definition_file ON definition_file.id=definition.file_id
             JOIN files AS occurrence_file ON occurrence_file.id=occurrence.file_id
             WHERE definition.name=occurrence.name
               AND (definition_file.language=occurrence_file.language OR
                 (definition_file.language IN ('typescript','javascript') AND occurrence_file.language IN ('typescript','javascript')))
             ORDER BY definition.id LIMIT 64)),
             provenance=provenance || ';global_name_candidates'
         WHERE target IS NULL AND candidates='[]' AND role IN ('call','read','type');"
    )?;
    transaction.commit()?;
    Ok(())
}

use anyhow::Result;
use rusqlite::Connection;

/// Global name matches remain candidates even when only one definition matches.
/// Candidates depend only on an occurrence's name and language, so each
/// distinct pair is resolved once and then assigned to its occurrences.
pub(super) fn references(conn: &mut Connection) -> Result<()> {
    let transaction = conn.transaction()?;
    transaction.execute_batch(
        "CREATE TEMP TABLE global_name_candidates(name TEXT NOT NULL,language TEXT NOT NULL,candidates TEXT NOT NULL,PRIMARY KEY(name,language)) WITHOUT ROWID;
         INSERT INTO temp.global_name_candidates(name,language,candidates)
         SELECT wanted.name,wanted.language,(SELECT json_group_array(id) FROM (
             SELECT definition.id FROM definitions AS definition
             JOIN files AS definition_file ON definition_file.id=definition.file_id
             WHERE definition.name=wanted.name
               AND (definition_file.language=wanted.language OR
                 (definition_file.language IN ('typescript','javascript') AND wanted.language IN ('typescript','javascript')))
             ORDER BY definition.id LIMIT 64))
         FROM (SELECT DISTINCT occurrence.name,occurrence_file.language FROM occurrences AS occurrence
               JOIN files AS occurrence_file ON occurrence_file.id=occurrence.file_id
               WHERE occurrence.target IS NULL AND occurrence.candidates='[]'
                 AND occurrence.role IN ('call','read','type') AND occurrence_file.language!='php') AS wanted;
         UPDATE occurrences AS occurrence
         SET candidates=(SELECT resolved.candidates FROM temp.global_name_candidates AS resolved
             JOIN files AS occurrence_file ON occurrence_file.id=occurrence.file_id
             WHERE resolved.name=occurrence.name AND resolved.language=occurrence_file.language),
             provenance=provenance || ';global_name_candidates'
         WHERE target IS NULL AND candidates='[]' AND role IN ('call','read','type')
           AND occurrence.file_id IN (SELECT id FROM files WHERE language!='php');
         DROP TABLE temp.global_name_candidates;",
    )?;
    transaction.commit()?;
    Ok(())
}

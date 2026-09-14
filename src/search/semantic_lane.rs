use super::{Query, hit_row};
use crate::{
    results::{Hit, MAX_HITS},
    semantic::{self, Embedding, SemanticEngine},
    store::Store,
};
use anyhow::Result;

pub(super) fn append(
    store: &Store,
    query: &Query,
    engine: &mut SemanticEngine,
    vector: &Embedding,
    hits: &mut Vec<Hit>,
    coverage: &mut serde_json::Value,
) -> Result<bool> {
    let mut stmt=store.conn.prepare("SELECT r.id,r.body,r.file_id FROM regions r JOIN files f ON f.id=r.file_id WHERE f.revision IS NOT NULL AND substr(f.path,1,length(?1))=?1 AND (?2='' OR f.language=?2) AND (?3='' OR r.kind=?3) ORDER BY r.file_id,r.id")?;
    let rows = stmt.query_map(
        rusqlite::params![query.path, query.language, query.kind],
        |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        },
    )?;
    let mut embedded = 0usize;
    let mut failures = 0usize;
    let mut last_file = None;
    let mut file_failed = false;
    let mut complete_files = 0usize;
    let candidates = rows.filter_map(|row| match row {
        Ok((id, body, file)) => {
            if last_file != Some(file) {
                if last_file.is_some() && !file_failed {
                    complete_files += 1;
                }
                last_file = Some(file);
                file_failed = false;
            }
            match engine.embed(&body) {
                Ok(embedding) => {
                    embedded += 1;
                    Some((id, embedding))
                }
                Err(_) => {
                    failures += 1;
                    file_failed = true;
                    None
                }
            }
        }
        Err(_) => {
            failures += 1;
            file_failed = true;
            None
        }
    });
    let ranked = semantic::search_top_k(vector, candidates, MAX_HITS);
    let mut stmt=store.conn.prepare("SELECT f.path,f.revision,r.start,r.end,r.name,r.kind,NULL,'semantic',c.bytes FROM regions r JOIN files f ON f.id=r.file_id JOIN contents c ON c.revision=f.revision WHERE r.id=?1")?;
    // Round-robin fusion preserves each deterministic lane's internal ordering.
    let mut semantic_hits = Vec::with_capacity(ranked.len());
    for hit in ranked {
        semantic_hits.push(stmt.query_row([hit.id as i64], hit_row)?);
    }
    let lexical = std::mem::take(hits);
    hits.reserve(lexical.len() + semantic_hits.len());
    let mut a = lexical.into_iter();
    let mut b = semantic_hits.into_iter();
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (a, b) => {
                hits.extend(a);
                hits.extend(b);
            }
        }
    }
    if last_file.is_some() && !file_failed {
        complete_files += 1;
    }
    coverage["semantic_files"] = complete_files.into();
    coverage["semantic_total_regions"] = (embedded + failures).into();
    coverage["semantic_scope"] = "query_filters".into();
    coverage["semantic_regions"] = embedded.into();
    coverage["semantic_failures"] = failures.into();
    Ok(embedded > MAX_HITS)
}

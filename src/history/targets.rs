use crate::{
    identity::{ByteSpan, ResultHandle},
    results::{self, Hit, ResultEntry},
    store::Store,
};
use anyhow::{Context, Result};

pub(super) struct Target {
    pub path: String,
    pub symbol: Option<String>,
    pub span: Option<ByteSpan>,
    pub revision: Option<String>,
    pub commit: Option<crate::identity::GitOid>,
}
pub(super) enum Selection {
    Target(Target),
    Candidates(Vec<Hit>),
}

pub(super) fn resolve(store: &Store, value: &str) -> Result<Selection> {
    if value.parse::<ResultHandle>().is_ok() {
        let (_, entry) = results::entry(store, value)?;
        return Ok(Selection::Target(match entry {
            ResultEntry::LiveSource(hit) => Target {
                path: hit.path,
                revision: hit.revision,
                commit: None,
                symbol: (hit.kind != "file").then_some(hit.name),
                span: Some(ByteSpan::new(hit.start, hit.end)?),
            },
            ResultEntry::Change(change) => {
                let side = change
                    .after
                    .or(change.before)
                    .context("history_unavailable: change has no readable side")?;
                let symbol = (change.name != side.path).then_some(change.name);
                Target {
                    path: side.path,
                    revision: Some(side.revision.to_string()),
                    commit: Some(side.commit),
                    symbol,
                    span: Some(side.span),
                }
            }
            ResultEntry::Commit(_) => {
                anyhow::bail!("invalid_target: commit handle is not a source selector")
            }
        }));
    }
    if let Some(name) = value.strip_prefix("sym:") {
        let mut query = store.conn.prepare("SELECT f.path,f.revision,d.start,d.end,d.name,d.kind,d.container FROM definitions d JOIN files f ON f.id=d.file_id WHERE d.name=?1 ORDER BY f.path,d.start")?;
        let hits = query
            .query_map([name], |r| {
                Ok(Hit {
                    handle: String::new(),
                    path: r.get(0)?,
                    revision: r.get(1)?,
                    start: r.get::<_, i64>(2)? as usize,
                    end: r.get::<_, i64>(3)? as usize,
                    start_line: 0,
                    end_line: 0,
                    name: r.get(4)?,
                    kind: r.get(5)?,
                    container: r.get(6)?,
                    provenance: None,
                    resolution: None,
                    candidates: Vec::new(),
                    target: None,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if hits.len() != 1 {
            return Ok(Selection::Candidates(hits));
        }
        let hit = &hits[0];
        return Ok(Selection::Target(Target {
            path: hit.path.clone(),
            revision: hit.revision.clone(),
            commit: None,
            symbol: Some(name.into()),
            span: Some(ByteSpan::new(hit.start, hit.end)?),
        }));
    }
    let path = value.strip_prefix("path:").unwrap_or(value).to_owned();
    let decoded = crate::store::decode_path(&path)?;
    anyhow::ensure!(
        decoded
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
            && !decoded.as_os_str().is_empty(),
        "invalid_path: expected confined root-relative path"
    );
    Ok(Selection::Target(Target {
        path,
        revision: None,
        commit: None,
        symbol: None,
        span: None,
    }))
}

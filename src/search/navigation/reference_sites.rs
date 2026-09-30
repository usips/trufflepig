//! `refs NAME [file:P] [lang:L] [kind:K]`: occurrence sites ordered declaration
//! first, then other non-test sites, imports, and test sites, each tier in path
//! order. Sites sharing a `path:line` collapse into one row marked `×N`.
use super::definition_target;
use crate::{
    results::{self, MAX_HITS, ResultSet},
    search::{Query, declarations::is_test_path, hit_row, qualified_name::QualifiedName, snippets},
    store::Store,
};
use anyhow::{Result, ensure};
use rusqlite::params;
use std::collections::{HashMap, HashSet};

/// Parses `refs` words (or a `refs:NAME ...` search) as one query, so unquoted
/// `file:`, `lang:` and `kind:` words after the name filter the sites.
pub fn reference_query(text: &str) -> Result<Query> {
    let mut query = Query::parse(text)?;
    if let Some(name) = query.text.strip_prefix("refs:") {
        query.text = name.trim().to_owned();
    }
    Ok(query)
}

/// Occurrences named by `query.text`; `kind:` matches the occurrence role
/// (`call`, `import`, ...) or the kind of its resolved or candidate target.
pub fn references(store: &Store, query: &Query) -> Result<ResultSet> {
    let name = QualifiedName::parse(&query.text);
    ensure!(!name.name.is_empty(), "usage: refs NAME");
    let snapshot = store.conn.unchecked_transaction()?;
    let generation = store.generation()?;
    let mut coverage = serde_json::to_value(store.coverage()?)?;
    let mut stmt = store.conn.prepare(
        "SELECT f.path,f.revision,o.start,o.end,o.name,o.role,NULL,o.provenance,c.bytes,
                o.target,o.candidates,
                EXISTS(SELECT 1 FROM definitions t
                       WHERE t.file_id=o.file_id AND t.kind='module' AND t.name IN ('tests','test')
                         AND t.start<=o.start AND t.end>=o.end),
                (SELECT t.kind FROM definitions t WHERE t.id=o.target)
         FROM occurrences o
         JOIN files f ON f.id=o.file_id
         JOIN contents c ON c.revision=f.revision
         WHERE o.name=?1
           AND substr(f.path,1,length(?2))=?2
           AND (?3='' OR f.language=?3)
           AND (?4='' OR o.role=?4 OR EXISTS(
                SELECT 1 FROM definitions t
                WHERE t.kind=?4
                  AND (t.id=o.target OR t.id IN (SELECT value FROM json_each(o.candidates)))))
         ORDER BY f.path,o.start,o.id
         LIMIT ?5",
    )?;
    let rows = stmt.query_map(
        params![
            name.name,
            query.path,
            query.language,
            query.kind,
            (MAX_HITS + 1) as i64
        ],
        |r| {
            Ok(Site {
                hit: hit_row(r)?,
                target: r.get(9)?,
                candidates: r.get(10)?,
                in_test_module: r.get(11)?,
                target_kind: r.get(12)?,
                candidate_ids: Vec::new(),
            })
        },
    )?;
    let mut sites = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut truncated = sites.len() > MAX_HITS;
    sites.truncate(MAX_HITS);
    for site in &mut sites {
        site.candidate_ids = serde_json::from_str(&site.candidates)?;
    }
    if name.is_qualified() {
        let mut kept = Vec::with_capacity(sites.len());
        for site in sites {
            if site_matches_qualifier(store, &name, &site)? {
                kept.push(site);
            }
        }
        sites = kept;
    }
    let files = sites
        .iter()
        .map(|site| site.hit.path.as_str())
        .collect::<HashSet<_>>()
        .len();
    coverage["reference_sites"] = sites.len().into();
    coverage["reference_files"] = files.into();
    if truncated {
        coverage["reference_sites_truncated"] = true.into();
    }
    sites.sort_by_key(Site::tier);
    let mut hits = Vec::with_capacity(sites.len().min(128));
    let mut lines: HashMap<(String, usize), usize> = HashMap::with_capacity(sites.len());
    let mut repeats = Vec::with_capacity(sites.len().min(128));
    let mut bytes = 0usize;
    for site in sites {
        let key = (site.hit.path.clone(), site.hit.start_line);
        if let Some(&index) = lines.get(&key) {
            repeats[index] += 1;
            continue;
        }
        let mut hit = site.hit;
        hit.resolution = Some(
            if site.target.is_some() {
                "resolved"
            } else if !site.candidate_ids.is_empty() {
                "candidate"
            } else {
                "unresolved"
            }
            .into(),
        );
        hit.target = site
            .target
            .map(|id| definition_target(store, id))
            .transpose()?;
        for id in site
            .candidate_ids
            .iter()
            .filter(|id| Some(**id) != site.target)
        {
            hit.candidates.push(definition_target(store, *id)?);
        }
        bytes += serde_json::to_vec(&hit)?.len();
        if bytes > results::MAX_BYTES {
            truncated = true;
            break;
        }
        lines.insert(key, hits.len());
        repeats.push(1usize);
        hits.push(hit);
    }
    for (hit, count) in hits.iter_mut().zip(repeats) {
        if count > 1
            && let Some(resolution) = &mut hit.resolution
        {
            resolution.push_str(&format!(" ×{count}"));
        }
    }
    snippets::attach(store, &snippets::preview_terms(&name.name), &mut hits)?;
    snapshot.commit()?;
    Ok(ResultSet {
        generation,
        coverage,
        truncated,
        hits,
    })
}

/// Coverage keys a `refs` result carries; workspace member coverage copies them.
pub const REFERENCE_COVERAGE_KEYS: [&str; 3] = [
    "reference_sites",
    "reference_files",
    "reference_sites_truncated",
];

/// `refs T sites in F files` from a `refs` result's coverage, so a page states
/// how exhaustive it is; `T+` when the site cap was reached.
pub fn reference_summary(coverage: &serde_json::Value) -> Option<String> {
    let sites = coverage["reference_sites"].as_u64()?;
    let files = coverage["reference_files"].as_u64().unwrap_or(0);
    let more = if coverage["reference_sites_truncated"] == true {
        "+"
    } else {
        ""
    };
    let plural = |count: u64| if count == 1 { "" } else { "s" };
    Some(format!(
        "refs {sites}{more} site{} in {files} file{}",
        plural(sites),
        plural(files)
    ))
}

struct Site {
    hit: crate::results::Hit,
    target: Option<i64>,
    candidates: String,
    in_test_module: bool,
    target_kind: Option<String>,
    candidate_ids: Vec<i64>,
}

impl Site {
    /// 0 declaration, 1 other non-test site, 2 import, 3 test site. A `use`
    /// binding's declaration site counts as an import.
    fn tier(&self) -> u8 {
        let declaration = self.hit.kind == "declaration";
        let imports = self.hit.kind == "import"
            || declaration && self.target_kind.as_deref() == Some("import");
        if declaration && !imports {
            0
        } else if self.in_test_module || is_test_path(&self.hit.path) {
            3
        } else if imports {
            2
        } else {
            1
        }
    }
}

/// A qualified `refs A::b` keeps sites whose target or a candidate is `b` owned by `A`.
fn site_matches_qualifier(store: &Store, name: &QualifiedName, site: &Site) -> Result<bool> {
    for id in site.target.iter().chain(&site.candidate_ids) {
        let (path, container): (String, Option<String>) = store.conn.query_row(
            "SELECT f.path,d.container FROM definitions d JOIN files f ON f.id=d.file_id WHERE d.id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if name.matches(&path, container.as_deref()).is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;

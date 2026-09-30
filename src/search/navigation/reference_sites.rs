//! `refs NAME [file:P] [lang:L] [kind:K]`: occurrence sites ordered declaration
//! first, then other non-test sites, imports, and test sites, each tier in path
//! order. Sites sharing a `path:line` collapse into one row marked `×N`.
use super::definition_target;
use crate::{
    results::{self, MAX_HITS, ResultSet},
    search::{Query, hit_row, path_class::is_test_path, qualified_name::QualifiedName, snippets},
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
    references_with_limit(store, query, MAX_HITS)
}

/// Returns sites ordered by declaration/code/import/test priority, applying
/// ownership and priority before the result cap.
fn references_with_limit(store: &Store, query: &Query, max_sites: usize) -> Result<ResultSet> {
    let name = QualifiedName::parse(&query.text);
    ensure!(!name.name.is_empty(), "usage: refs NAME");
    let snapshot = store.conn.unchecked_transaction()?;
    let generation = store.generation()?;
    let mut coverage = serde_json::to_value(store.coverage()?)?;
    let paths = query.path.bind(&store.conn)?;
    let mut stmt = store.conn.prepare(&format!(
        "SELECT f.path,f.revision,o.start,o.end,o.name,o.role,NULL,o.provenance,c.bytes,
                o.target,o.candidates,
                EXISTS(SELECT 1 FROM definitions t
                       WHERE t.file_id=o.file_id AND t.kind='module' AND t.name IN ('tests','test')
                         AND t.start<=o.start AND t.end>=o.end),
                (SELECT t.kind FROM definitions t WHERE t.id=o.target)
         FROM occurrences o
         JOIN files f ON f.id=o.file_id
         JOIN contents c ON c.revision=f.revision
         WHERE o.name=?1{}
           AND (?2='' OR f.language=?2)
           AND (?3='' OR o.role=?3 OR EXISTS(
                SELECT 1 FROM definitions t
                WHERE t.kind=?3
                  AND (t.id=o.target OR t.id IN (SELECT value FROM json_each(o.candidates)))))
           AND (?5='' OR EXISTS(
                SELECT 1 FROM definitions t JOIN files tf ON tf.id=t.file_id
                WHERE (t.id=o.target OR t.id IN (SELECT value FROM json_each(o.candidates)))
                  AND instr(coalesce(t.container,'')||'/'||replace(tf.path,'-','_'),?5)>0))
         ORDER BY f.path,o.start,o.id
         LIMIT ?4",
        paths.sql_clause("f.path")
    ))?;
    let rows = stmt.query_map(
        params![
            name.name,
            query.language,
            query.kind,
            i64::MAX,
            name.owner_hint()
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
    let result_cap = max_sites.saturating_add(1);
    let mut priority_sites: [Vec<Site>; 4] = std::array::from_fn(|_| Vec::new());
    let mut retained = 0usize;
    let mut truncated = false;
    for row in rows {
        let mut site = row?;
        site.candidate_ids = serde_json::from_str(&site.candidates)?;
        if name.is_qualified() && !site_matches_qualifier(store, &name, &site)? {
            continue;
        }
        let priority = site.tier() as usize;
        if retained < result_cap {
            priority_sites[priority].push(site);
            retained += 1;
        } else {
            truncated = true;
            let worst = (0..priority_sites.len())
                .rev()
                .find(|tier| !priority_sites[*tier].is_empty())
                .expect("full reference site buffer has a worst tier");
            if priority < worst {
                priority_sites[worst].pop();
                priority_sites[priority].push(site);
            } else if priority == 0 && worst == 0 {
                // The buffer already contains cap+1 declarations, the best
                // possible tier, in path order; later rows cannot improve it.
                break;
            }
        }
        if retained == result_cap && priority_sites[0].len() == result_cap {
            truncated = true;
            break;
        }
    }
    drop(stmt);
    let mut sites: Vec<Site> = priority_sites.into_iter().flatten().collect();
    truncated |= sites.len() > max_sites;
    sites.truncate(max_sites);
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
    let mut hits: Vec<crate::results::Hit> = Vec::with_capacity(sites.len().min(128));
    let mut rows: HashMap<SiteKey, usize> = HashMap::with_capacity(sites.len());
    let mut bytes = 0usize;
    for mut site in sites {
        site.candidate_ids.sort_unstable();
        let key = SiteKey {
            path: site.hit.path.clone(),
            line: site.hit.start_line,
            role: site.hit.kind.clone(),
            target: site.target,
            candidates: site.candidate_ids.clone(),
        };
        if let Some(&index) = rows.get(&key) {
            let row = &mut hits[index];
            row.repeats = Some(row.repeats.unwrap_or(1) + 1);
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
        rows.insert(key, hits.len());
        hits.push(hit);
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

/// Occurrences on one line collapse only when role and resolution agree.
#[derive(Eq, Hash, PartialEq)]
struct SiteKey {
    path: String,
    line: usize,
    role: String,
    target: Option<i64>,
    candidates: Vec<i64>,
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
    /// 0 declaration, 1 other code, 2 import, 3 any test site (tests go last
    /// whatever their role). A `use` binding's declaration counts as an import.
    fn tier(&self) -> u8 {
        let declaration = self.hit.kind == "declaration";
        let imports = self.hit.kind == "import"
            || declaration && self.target_kind.as_deref() == Some("import");
        if self.in_test_module || is_test_path(&self.hit.path) {
            3
        } else if declaration && !imports {
            0
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

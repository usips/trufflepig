use crate::{
    results::{DefinitionTarget, Hit},
    store::Store,
};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};

const MAX_VISITED: usize = 64;
const MAX_EMITTED: usize = 100;
const MAX_PENDING: usize = MAX_VISITED + MAX_EMITTED;
const MAX_SEED_WORK: usize = MAX_PENDING;

#[derive(Default)]
pub(super) struct InheritanceContext {
    pub(super) relationships: Vec<Value>,
    pub(super) truncated: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RelationshipRow {
    kind: String,
    provenance: String,
    path: String,
    start: usize,
    end: usize,
    source: i64,
    target: Option<i64>,
}

pub(super) fn collect(store: &Store, hit: &Hit) -> Result<InheritanceContext> {
    let mut context = InheritanceContext::default();
    let seed_collection = class_seeds(store, hit)?;
    context.truncated = seed_collection.truncated;
    if seed_collection.ids.is_empty() {
        return Ok(context);
    }

    let mut queue = VecDeque::with_capacity(MAX_PENDING);
    let mut pending = HashSet::with_capacity(MAX_PENDING);
    for seed in seed_collection.ids {
        pending.insert(seed);
        queue.push_back(seed);
    }

    let mut visited = HashSet::with_capacity(MAX_VISITED);
    let mut emitted = HashSet::with_capacity(MAX_EMITTED);
    while let Some(class_id) = queue.pop_front() {
        pending.remove(&class_id);
        if visited.contains(&class_id) {
            continue;
        }
        if visited.len() == MAX_VISITED {
            context.truncated = true;
            break;
        }
        visited.insert(class_id);

        let (class_name, namespace): (String, Option<String>) = store.conn.query_row(
            "SELECT name,container FROM definitions WHERE id=?1",
            [class_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let qualified_class = namespace
            .as_deref()
            .filter(|value| !value.is_empty())
            .map_or_else(
                || class_name.clone(),
                |value| format!("{value}\\{class_name}"),
            );

        for edge in relationships_for_class(store, class_id, &qualified_class)? {
            if emitted.insert(edge.clone()) {
                if context.relationships.len() == MAX_EMITTED {
                    context.truncated = true;
                    return Ok(context);
                }
                context
                    .relationships
                    .push(relationship_value(store, &edge)?);
            }

            let next = match edge.kind.as_str() {
                "framework_parent_candidate" => {
                    if edge.source == class_id {
                        edge.target
                    } else if edge.target == Some(class_id) {
                        Some(edge.source)
                    } else {
                        None
                    }
                }
                "php_extends_candidate" if edge.source == class_id => edge.target,
                _ => None,
            };
            if let Some(next) = next
                && !visited.contains(&next)
                && !pending.contains(&next)
            {
                if pending.len() + visited.len() == MAX_PENDING {
                    context.truncated = true;
                    break;
                }
                pending.insert(next);
                queue.push_back(next);
            }
        }

        if visited.len() == MAX_VISITED && !queue.is_empty() {
            context.truncated = true;
            break;
        }
    }
    Ok(context)
}

struct SeedCollection {
    ids: Vec<i64>,
    known: HashSet<i64>,
    work: usize,
    truncated: bool,
}

impl SeedCollection {
    fn new() -> Self {
        Self {
            ids: Vec::with_capacity(MAX_VISITED),
            known: HashSet::with_capacity(MAX_VISITED),
            work: 0,
            truncated: false,
        }
    }

    fn add_class(&mut self, id: i64) {
        if self.known.contains(&id) {
            return;
        }
        if self.ids.len() == MAX_VISITED {
            self.truncated = true;
            return;
        }
        self.known.insert(id);
        self.ids.push(id);
    }

    fn add_definition(&mut self, store: &Store, id: i64) -> Result<bool> {
        if self.work == MAX_SEED_WORK {
            self.truncated = true;
            return Ok(false);
        }
        self.work += 1;
        let kind: Option<String> = store
            .conn
            .query_row("SELECT kind FROM definitions WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        match kind.as_deref() {
            Some("class" | "interface" | "trait" | "enum") => self.add_class(id),
            Some("method") => {
                let mut stmt = store.conn.prepare(
                    "SELECT owner.id FROM definitions member
                     JOIN definitions owner ON owner.file_id=member.file_id
                       AND owner.kind IN ('class','interface','trait','enum')
                       AND owner.start<=member.start AND owner.end>=member.end
                     WHERE member.id=?1 ORDER BY owner.end-owner.start,owner.id LIMIT 1",
                )?;
                if let Some(owner) = stmt
                    .query_row([id], |row| row.get::<_, i64>(0))
                    .optional()?
                {
                    self.add_class(owner);
                }
            }
            _ => {}
        }
        Ok(true)
    }
}

fn class_seeds(store: &Store, hit: &Hit) -> Result<SeedCollection> {
    let mut seeds = SeedCollection::new();
    let mut stmt = store.conn.prepare(
        "SELECT d.id FROM definitions d JOIN files f ON f.id=d.file_id
         WHERE f.path=?1 AND f.revision=?2 AND d.start<=?3 AND d.end>=?4
           AND d.kind IN ('class','interface','trait','enum')
         ORDER BY d.end-d.start,d.start,d.id LIMIT ?5",
    )?;
    let ids = stmt.query_map(
        params![
            hit.path,
            hit.revision,
            hit.start as i64,
            hit.end as i64,
            MAX_VISITED as i64 + 1
        ],
        |row| row.get::<_, i64>(0),
    )?;
    let mut hit_classes = 0;
    for id in ids {
        let id = id?;
        if hit_classes == MAX_VISITED {
            seeds.truncated = true;
            break;
        }
        hit_classes += 1;
        seeds.add_class(id);
    }
    drop(stmt);

    let mut target_count = 0;
    for target in hit.target.iter().chain(hit.candidates.iter()) {
        if target_count == MAX_SEED_WORK || seeds.work == MAX_SEED_WORK {
            seeds.truncated = true;
            break;
        }
        target_count += 1;
        let mut target_ids = target_definition_ids(store, target)?;
        if target_ids.len() > MAX_VISITED {
            seeds.truncated = true;
            target_ids.truncate(MAX_VISITED);
        }
        for id in target_ids {
            if !seeds.add_definition(store, id)? {
                break;
            }
        }
    }

    let mut stmt = store.conn.prepare(
        "SELECT o.target,o.candidates FROM occurrences o JOIN files f ON f.id=o.file_id
         WHERE f.path=?1 AND f.revision=?2 AND o.role='xenforo_extension_implementation'
           AND o.start<?4 AND ?3<o.end ORDER BY o.start,o.end,o.id LIMIT ?5",
    )?;
    let rows = stmt.query_map(
        params![
            hit.path,
            hit.revision,
            hit.start as i64,
            hit.end as i64,
            MAX_SEED_WORK as i64 + 1
        ],
        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut occurrence_count = 0;
    for row in rows {
        if occurrence_count == MAX_SEED_WORK || seeds.work == MAX_SEED_WORK {
            seeds.truncated = true;
            break;
        }
        occurrence_count += 1;
        let (target, candidates) = row?;
        if let Some(target) = target {
            if !seeds.add_definition(store, target)? {
                break;
            }
        }
        for id in serde_json::from_str::<Vec<i64>>(&candidates)? {
            if !seeds.add_definition(store, id)? {
                break;
            }
        }
    }
    Ok(seeds)
}

fn target_definition_ids(store: &Store, target: &DefinitionTarget) -> Result<Vec<i64>> {
    let mut stmt = store.conn.prepare(
        "SELECT d.id FROM definitions d JOIN files f ON f.id=d.file_id
         WHERE f.path=?1 AND f.revision=?2 AND d.start=?3 AND d.end=?4 AND d.name=?5
         ORDER BY d.id LIMIT ?6",
    )?;
    let ids = stmt.query_map(
        params![
            target.path,
            target.revision,
            target.start as i64,
            target.end as i64,
            target.name,
            MAX_VISITED as i64 + 1
        ],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(ids.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn relationships_for_class(
    store: &Store,
    class_id: i64,
    qualified_class: &str,
) -> Result<Vec<RelationshipRow>> {
    let mut stmt = store.conn.prepare(
        "SELECT r.kind,r.provenance,f.path,r.start,r.end,r.source,r.target
         FROM relationships r JOIN files f ON f.id=r.file_id
         WHERE (r.kind='framework_parent_candidate' AND (r.source=?1 OR r.target=?1))
            OR (r.kind='php_extends_candidate' AND r.source=?1)
            OR (r.kind='inheritance_issue' AND (r.source=?1 OR r.source IN (
                 SELECT d.id FROM definitions d JOIN definitions owner ON owner.id=?1
                 WHERE d.kind='method' AND d.file_id=owner.file_id
                   AND d.start>=owner.start AND d.start<=owner.end AND d.end<=owner.end
                   AND lower(d.container)=lower(?2))))
            OR (r.kind='php_parent_call_candidate' AND r.source IN (
                 SELECT d.id FROM definitions d JOIN definitions owner ON owner.id=?1
                 WHERE d.kind='method' AND d.file_id=owner.file_id
                   AND d.start>=owner.start AND d.start<=owner.end AND d.end<=owner.end
                   AND lower(d.container)=lower(?2)))
         ORDER BY f.path,r.start,r.end,r.kind,r.source,r.target LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![class_id, qualified_class, MAX_EMITTED as i64 + 1],
        |row| {
            Ok(RelationshipRow {
                kind: row.get(0)?,
                provenance: row.get(1)?,
                path: row.get(2)?,
                start: row.get::<_, i64>(3)? as usize,
                end: row.get::<_, i64>(4)? as usize,
                source: row.get(5)?,
                target: row.get(6)?,
            })
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn relationship_value(store: &Store, edge: &RelationshipRow) -> Result<Value> {
    let resolution = if edge.kind.ends_with("_candidate") {
        "candidate"
    } else {
        "unresolved"
    };
    Ok(serde_json::json!({
        "kind": edge.kind,
        "resolution": resolution,
        "provenance": edge.provenance,
        "path": edge.path,
        "start": edge.start,
        "end": edge.end,
        "source": super::navigation::definition_target(store, edge.source)?,
        "target": edge.target.map(|id| super::navigation::definition_target(store, id)).transpose()?
    }))
}

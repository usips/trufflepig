mod alias_names;
mod alias_source;
mod chain_adapter;
mod chain_projection;
mod class_extensions;
mod manifest;

use super::hierarchy_index::{PhpHierarchyIndex, PhpParentCandidate};
use super::php_names::{PhpClassCandidate, PhpClassIndex};
use crate::store::inheritance::DerivedBudget;
pub(super) use alias_names::canonicalize_class_name;
pub(super) use alias_source::AliasSource;
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

const MAX_METADATA_FILES: usize = 10_000;
const MAX_METADATA_FACTS: usize = 200_000;
const MAX_CANDIDATES: usize = 64;
const MAX_METADATA_SOURCE_BYTES: usize = 2 * 1024 * 1024;

struct AddonManifest {
    file_id: i64,
    directory: String,
    id: String,
    byte_len: usize,
    requirements: Vec<ManifestRequirement>,
    definition_id: i64,
}

struct ManifestRequirement {
    name: String,
    span: Range<usize>,
    duplicate: bool,
}

struct ExtensionFile {
    file_id: i64,
    addon_id: String,
    directory: String,
    extensions: Vec<ClassExtension>,
}

struct ExtensionFileIssue {
    file_id: i64,
    directory: String,
    byte_len: usize,
    code: &'static str,
}

struct ClassExtension {
    from_class: String,
    from_span: Range<usize>,
    to_class: String,
    to_span: Range<usize>,
    tag_span: Range<usize>,
    active: Option<bool>,
    execute_order: Option<i64>,
    duplicate: bool,
}

/// Adds XenForo add-on and class-extension evidence from stored source bytes.
pub(super) fn resolve(
    conn: &mut Connection,
    classes: &PhpClassIndex,
    hierarchy: &mut PhpHierarchyIndex,
    aliases: &AliasSource,
    budget: &mut DerivedBudget,
) -> Result<()> {
    let transaction = conn.transaction()?;
    let baseline = transaction.total_changes();
    ensure!(
        metadata_file_count(&transaction)? <= MAX_METADATA_FILES,
        "XenForo metadata file limit exceeded"
    );
    let mut manifests = load_manifests(&transaction)?;
    let (extensions, extension_issues) = load_extension_files(&transaction)?;
    let parsed_facts = manifests
        .iter()
        .map(|manifest| manifest.requirements.len())
        .sum::<usize>()
        .saturating_add(
            extensions
                .iter()
                .map(|file| file.extensions.len())
                .sum::<usize>(),
        );
    ensure!(
        parsed_facts <= MAX_METADATA_FACTS,
        "XenForo metadata fact limit exceeded"
    );

    let mut addon_ids = HashMap::<String, Vec<i64>>::with_capacity(manifests.len());
    let mut addon_directories = HashMap::<String, AddonOwner>::with_capacity(manifests.len());
    for manifest in &mut manifests {
        let definition_id = insert_definition(
            &transaction,
            manifest.file_id,
            &manifest.id,
            "addon",
            None,
            0,
            manifest.byte_len,
        )?;
        manifest.definition_id = definition_id;
        addon_ids
            .entry(manifest.id.clone())
            .or_default()
            .push(definition_id);
        addon_directories.insert(
            manifest.directory.clone(),
            AddonOwner {
                id: manifest.id.clone(),
                definition_id,
            },
        );
        ensure_fact_budget(&transaction, baseline)?;
    }

    publish_requirements(&transaction, &manifests, &addon_ids, baseline)?;
    publish_extensions(
        &transaction,
        &extensions,
        classes,
        &addon_ids,
        &addon_directories,
        baseline,
    )?;
    publish_extension_file_issues(
        &transaction,
        &extension_issues,
        &addon_directories,
        baseline,
    )?;

    let prepared = chain_adapter::prepare(&transaction, &extensions, hierarchy, aliases, budget)?;
    let mut parents_by_evidence = BTreeMap::<(i64, i64, i64), Vec<PhpParentCandidate>>::new();
    for parent in prepared.hierarchy_edges.iter().cloned() {
        parents_by_evidence
            .entry((
                parent.child_class_id,
                parent.evidence_start,
                parent.evidence_end,
            ))
            .or_default()
            .push(parent);
    }
    for ((child, start, end), replacements) in parents_by_evidence {
        hierarchy.replace_proxy_parent_candidates(child, start, end, replacements);
    }
    chain_projection::project(&transaction, &prepared)?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn load_alias_source(conn: &Connection) -> Result<AliasSource> {
    let source: Option<Vec<u8>> = conn
        .query_row(
            "SELECT c.bytes FROM files f JOIN contents c ON c.revision=f.revision WHERE f.path='src/XF.php' AND f.language='php'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(AliasSource::parse(source.as_deref().unwrap_or_default()))
}

struct AddonOwner {
    id: String,
    definition_id: i64,
}

fn metadata_file_count(transaction: &Transaction<'_>) -> Result<usize> {
    let count: i64 = transaction.query_row(
        "SELECT count(*) FROM files WHERE path LIKE 'src/addons/%/addon.json' OR path LIKE 'src/addons/%/!_data/class_extensions.xml' ESCAPE '!'",
        [],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count)?)
}

fn load_manifests(transaction: &Transaction<'_>) -> Result<Vec<AddonManifest>> {
    let mut statement = transaction.prepare(
        "SELECT f.id,f.path,length(c.bytes),c.bytes FROM files f JOIN contents c ON c.revision=f.revision WHERE f.path LIKE 'src/addons/%/addon.json' ORDER BY f.path",
    )?;
    let mut rows = statement.query([])?;
    let mut manifests = Vec::new();
    let mut total_requirements = 0_usize;
    let mut scanned_files = 0_usize;
    while let Some(row) = rows.next()? {
        scanned_files += 1;
        if scanned_files > MAX_METADATA_FILES {
            bail!("XenForo metadata file limit exceeded");
        }
        let file_id = row.get(0)?;
        let path: String = row.get(1)?;
        let source_len: i64 = row.get(2)?;
        if source_len > MAX_METADATA_SOURCE_BYTES as i64 {
            continue;
        }
        let source: Vec<u8> = row.get(3)?;
        let Some((id, directory)) = addon_location(&path, "/addon.json") else {
            continue;
        };
        let Ok(requirements) = manifest::parse_manifest(&source) else {
            continue;
        };
        total_requirements = total_requirements.saturating_add(requirements.len());
        ensure!(
            total_requirements <= MAX_METADATA_FACTS,
            "XenForo manifest fact limit exceeded"
        );
        manifests.push(AddonManifest {
            file_id,
            directory,
            id,
            byte_len: source.len(),
            requirements,
            definition_id: 0,
        });
    }
    Ok(manifests)
}

fn load_extension_files(
    transaction: &Transaction<'_>,
) -> Result<(Vec<ExtensionFile>, Vec<ExtensionFileIssue>)> {
    let mut statement = transaction.prepare(
        "SELECT f.id,f.path,length(c.bytes),c.bytes FROM files f JOIN contents c ON c.revision=f.revision WHERE f.path LIKE 'src/addons/%/!_data/class_extensions.xml' ESCAPE '!' ORDER BY f.path",
    )?;
    let mut rows = statement.query([])?;
    let mut files = Vec::new();
    let mut issues = Vec::new();
    let mut total_extensions = 0_usize;
    let mut scanned_files = 0_usize;
    while let Some(row) = rows.next()? {
        scanned_files += 1;
        if scanned_files > MAX_METADATA_FILES {
            bail!("XenForo metadata file limit exceeded");
        }
        let file_id = row.get(0)?;
        let path: String = row.get(1)?;
        let source_len: i64 = row.get(2)?;
        let source: Vec<u8> = row.get(3)?;
        let Some((addon_id, directory)) = addon_location(&path, "/_data/class_extensions.xml")
        else {
            continue;
        };
        if source_len > MAX_METADATA_SOURCE_BYTES as i64 {
            issues.push(ExtensionFileIssue {
                file_id,
                directory,
                byte_len: usize::try_from(source_len)?,
                code: "extension_metadata_too_large",
            });
            continue;
        }
        let Ok(extensions) = class_extensions::parse(&source) else {
            // A malformed tail invalidates the file's earlier declarations too.
            issues.push(ExtensionFileIssue {
                file_id,
                directory,
                byte_len: source.len(),
                code: "malformed_extension_metadata",
            });
            continue;
        };
        total_extensions = total_extensions.saturating_add(extensions.len());
        ensure!(
            total_extensions <= MAX_METADATA_FACTS,
            "XenForo extension fact limit exceeded"
        );
        files.push(ExtensionFile {
            file_id,
            addon_id,
            directory,
            extensions,
        });
    }
    Ok((files, issues))
}

fn publish_extension_file_issues(
    transaction: &Transaction<'_>,
    issues: &[ExtensionFileIssue],
    addon_directories: &HashMap<String, AddonOwner>,
    baseline: u64,
) -> Result<()> {
    for issue in issues {
        let Some(owner) = addon_directories.get(&issue.directory) else {
            continue;
        };
        insert_relationship(
            transaction,
            owner.definition_id,
            None,
            "inheritance_issue",
            issue.file_id,
            0..issue.byte_len,
            &format!("xenforo_class_extensions_xml;{}", issue.code),
        )?;
        ensure_fact_budget(transaction, baseline)?;
    }
    Ok(())
}

fn addon_location(path: &str, suffix: &str) -> Option<(String, String)> {
    let tail = path.strip_prefix("src/addons/")?.strip_suffix(suffix)?;
    if tail.is_empty()
        || tail.starts_with('/')
        || tail.ends_with('/')
        || tail
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return None;
    }
    let decoded = crate::store::decode_path(tail).ok()?;
    let id = decoded.to_str()?.to_owned();
    Some((id, format!("src/addons/{tail}")))
}

fn publish_requirements(
    transaction: &Transaction<'_>,
    manifests: &[AddonManifest],
    addon_ids: &HashMap<String, Vec<i64>>,
    baseline: u64,
) -> Result<()> {
    for manifest in manifests {
        for requirement in &manifest.requirements {
            let is_platform = matches!(requirement.name.as_str(), "XF" | "php");
            let candidates = if is_platform || requirement.duplicate {
                &[][..]
            } else {
                addon_ids
                    .get(&requirement.name)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
            };
            let provenance = if requirement.duplicate {
                "xenforo_addon_json_require;duplicate_requirement_key"
            } else if is_platform {
                "xenforo_addon_json_require;platform_requirement"
            } else if candidates.is_empty() {
                "xenforo_addon_json_require;target_missing"
            } else {
                "xenforo_addon_json_require;declared_requirement"
            };
            let candidates = candidates
                .iter()
                .copied()
                .take(MAX_CANDIDATES)
                .collect::<Vec<_>>();
            let provenance = if addon_ids
                .get(&requirement.name)
                .is_some_and(|matches| matches.len() > MAX_CANDIDATES)
                && !is_platform
                && !requirement.duplicate
            {
                format!("{provenance};xenforo_candidates_truncated")
            } else {
                provenance.to_owned()
            };
            insert_occurrence(
                transaction,
                manifest.file_id,
                &requirement.name,
                requirement.span.clone(),
                "xenforo_addon_requirement",
                None,
                &candidates,
                &provenance,
            )?;
            let kind = if candidates.is_empty() {
                "xenforo_addon_requirement"
            } else {
                "xenforo_addon_requirement_candidate"
            };
            if candidates.is_empty() {
                insert_relationship(
                    transaction,
                    manifest.definition_id,
                    None,
                    kind,
                    manifest.file_id,
                    requirement.span.clone(),
                    &provenance,
                )?;
            } else {
                for &candidate in candidates.iter().take(MAX_CANDIDATES) {
                    insert_relationship(
                        transaction,
                        manifest.definition_id,
                        Some(candidate),
                        kind,
                        manifest.file_id,
                        requirement.span.clone(),
                        &provenance,
                    )?;
                }
            }
            ensure_fact_budget(transaction, baseline)?;
        }
    }
    Ok(())
}

fn publish_extensions(
    transaction: &Transaction<'_>,
    files: &[ExtensionFile],
    classes: &PhpClassIndex,
    addon_ids: &HashMap<String, Vec<i64>>,
    addon_directories: &HashMap<String, AddonOwner>,
    baseline: u64,
) -> Result<()> {
    for file in files {
        for extension in &file.extensions {
            let active = extension.active == Some(true) && !extension.duplicate;
            let active_value = extension
                .active
                .map_or("unknown", |active| if active { "1" } else { "0" });
            let order = extension
                .execute_order
                .map_or_else(|| "unknown".to_owned(), |order| order.to_string());
            let state = if extension.duplicate {
                "duplicate_declaration"
            } else if extension.active == Some(false) {
                "declared_disabled"
            } else {
                "runtime_xfcp_chain_unresolved"
            };
            let status = format!("active={active_value};execute_order={order};{state}");
            let provenance = format!("xenforo_class_extensions_xml;{status}");
            let extension_definition = insert_definition(
                transaction,
                file.file_id,
                &extension.to_class,
                "xenforo_class_extension",
                Some(&file.addon_id),
                extension.tag_span.start,
                extension.tag_span.end,
            )?;
            let base_classes = if active {
                classes.candidates(&extension.from_class)
            } else {
                &[][..]
            };
            let implementation_classes = if active {
                classes.candidates(&extension.to_class)
            } else {
                &[][..]
            };
            let base_candidates = class_candidate_ids(base_classes);
            let implementation_candidates = class_candidate_ids(implementation_classes);
            let base_provenance = if base_classes.len() > MAX_CANDIDATES {
                format!("{provenance};xenforo_candidates_truncated")
            } else {
                provenance.clone()
            };
            let implementation_provenance = if implementation_classes.len() > MAX_CANDIDATES {
                format!("{provenance};xenforo_candidates_truncated")
            } else {
                provenance.clone()
            };
            insert_occurrence(
                transaction,
                file.file_id,
                &extension.from_class,
                extension.from_span.clone(),
                "xenforo_extension_base",
                None,
                &base_candidates,
                &base_provenance,
            )?;
            insert_occurrence(
                transaction,
                file.file_id,
                &extension.to_class,
                extension.to_span.clone(),
                "xenforo_extension_implementation",
                None,
                &implementation_candidates,
                &implementation_provenance,
            )?;
            insert_class_candidates(
                transaction,
                extension_definition,
                file.file_id,
                extension.tag_span.clone(),
                "xenforo_class_extension_candidate",
                &base_provenance,
                &base_candidates,
            )?;
            insert_class_candidates(
                transaction,
                extension_definition,
                file.file_id,
                extension.tag_span.clone(),
                "xenforo_class_extension_implementation_candidate",
                &implementation_provenance,
                &implementation_candidates,
            )?;

            if active {
                publish_addon_extension_candidates(
                    transaction,
                    file,
                    extension,
                    implementation_classes,
                    base_classes,
                    addon_ids,
                    addon_directories,
                    &base_provenance,
                )?;
            }
            ensure_fact_budget(transaction, baseline)?;
        }
    }
    Ok(())
}

fn publish_addon_extension_candidates(
    transaction: &Transaction<'_>,
    file: &ExtensionFile,
    extension: &ClassExtension,
    implementations: &[PhpClassCandidate],
    bases: &[PhpClassCandidate],
    addon_ids: &HashMap<String, Vec<i64>>,
    addon_directories: &HashMap<String, AddonOwner>,
    provenance: &str,
) -> Result<()> {
    let Some(source_addon) = addon_directories.get(&file.directory) else {
        return Ok(());
    };
    let source_classes: Vec<_> = implementations
        .iter()
        .take(MAX_CANDIDATES + 1)
        .filter(|candidate| {
            package_directory_for_file(&candidate.file_path, addon_directories)
                == Some(file.directory.as_str())
        })
        .take(2)
        .collect();
    if source_classes.len() != 1 {
        return Ok(());
    }
    let implementation = source_classes[0];
    let mut targets = HashMap::<String, Vec<i64>>::new();
    for candidate in bases.iter().take(MAX_CANDIDATES) {
        insert_relationship(
            transaction,
            implementation.definition_id,
            Some(candidate.definition_id),
            "xenforo_class_extension_candidate",
            file.file_id,
            extension.tag_span.clone(),
            &format!("{provenance};implementation_to_declared_base"),
        )?;
        let Some(target_directory) =
            package_directory_for_file(&candidate.file_path, addon_directories)
        else {
            continue;
        };
        let Some(target_addon) = addon_directories.get(target_directory) else {
            continue;
        };
        if target_addon.id != source_addon.id {
            targets.entry(target_addon.id.clone()).or_default().extend(
                addon_ids
                    .get(&target_addon.id)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
        }
    }
    for (target_id, definitions) in targets {
        let mut unique = definitions;
        unique.sort_unstable();
        unique.dedup();
        let relation_provenance = if unique.len() > MAX_CANDIDATES {
            format!(
                "{provenance};cross_addon_target={target_id};declared_extension_only;xenforo_candidates_truncated"
            )
        } else {
            format!("{provenance};cross_addon_target={target_id};declared_extension_only")
        };
        if unique.is_empty() {
            insert_relationship(
                transaction,
                source_addon.definition_id,
                None,
                "xenforo_addon_extension_dependency",
                file.file_id,
                extension.tag_span.clone(),
                &relation_provenance,
            )?;
        } else {
            for target in unique.into_iter().take(MAX_CANDIDATES) {
                insert_relationship(
                    transaction,
                    source_addon.definition_id,
                    Some(target),
                    "xenforo_addon_extension_dependency_candidate",
                    file.file_id,
                    extension.tag_span.clone(),
                    &relation_provenance,
                )?;
            }
        }
    }
    Ok(())
}

fn class_candidate_ids(candidates: &[PhpClassCandidate]) -> Vec<i64> {
    candidates
        .iter()
        .take(MAX_CANDIDATES)
        .map(|candidate| candidate.definition_id)
        .collect()
}

fn package_directory_for_file<'a>(
    file_path: &'a str,
    addon_directories: &HashMap<String, AddonOwner>,
) -> Option<&'a str> {
    let mut end = file_path.rfind('/')?;
    loop {
        let directory = &file_path[..end];
        if addon_directories.contains_key(directory) {
            return Some(directory);
        }
        end = directory.rfind('/')?;
    }
}

fn insert_class_candidates(
    transaction: &Transaction<'_>,
    source: i64,
    file_id: i64,
    span: Range<usize>,
    kind: &str,
    provenance: &str,
    candidates: &[i64],
) -> Result<()> {
    if candidates.is_empty() {
        insert_relationship(
            transaction,
            source,
            None,
            &kind.trim_end_matches("_candidate"),
            file_id,
            span,
            provenance,
        )?;
    } else {
        for &candidate in candidates.iter().take(MAX_CANDIDATES) {
            insert_relationship(
                transaction,
                source,
                Some(candidate),
                kind,
                file_id,
                span.clone(),
                provenance,
            )?;
        }
    }
    Ok(())
}

fn insert_definition(
    transaction: &Transaction<'_>,
    file_id: i64,
    name: &str,
    kind: &str,
    container: Option<&str>,
    start: usize,
    end: usize,
) -> Result<i64> {
    transaction.execute(
        "INSERT INTO definitions(file_id,name,kind,start,end,container) VALUES(?1,?2,?3,?4,?5,?6)",
        params![file_id, name, kind, start as i64, end as i64, container],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn insert_occurrence(
    transaction: &Transaction<'_>,
    file_id: i64,
    name: &str,
    span: Range<usize>,
    role: &str,
    target: Option<i64>,
    candidates: &[i64],
    provenance: &str,
) -> Result<i64> {
    transaction.execute(
        "INSERT INTO occurrences(file_id,name,start,end,role,target,candidates,provenance) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![file_id, name, span.start as i64, span.end as i64, role, target, serde_json::to_string(candidates)?, provenance],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn insert_relationship(
    transaction: &Transaction<'_>,
    source: i64,
    target: Option<i64>,
    kind: &str,
    file_id: i64,
    span: Range<usize>,
    provenance: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO relationships(source,target,kind,file_id,start,end,provenance) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![source, target, kind, file_id, span.start as i64, span.end as i64, provenance],
    )?;
    Ok(())
}

fn ensure_fact_budget(transaction: &Transaction<'_>, baseline: u64) -> Result<()> {
    ensure!(
        transaction.total_changes().saturating_sub(baseline) <= MAX_METADATA_FACTS as u64,
        "XenForo metadata fact limit exceeded"
    );
    Ok(())
}

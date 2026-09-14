use super::{decode_path, encode_path, module_config as config};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

pub(super) fn resolve(conn: &mut Connection) -> Result<()> {
    let transaction = conn.transaction()?;
    {
        let mut statement = transaction.prepare("SELECT occurrences.id,occurrences.name,files.path,files.language,occurrences.provenance,occurrences.role FROM occurrences JOIN files ON files.id=occurrences.file_id WHERE occurrences.role IN ('require','import_path','include')")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let source: String = row.get(2)?;
            let language: String = row.get(3)?;
            let original_provenance: String = row.get(4)?;
            let role: String = row.get(5)?;
            if (name.contains('\\') && role != "include") || name.contains('\0') {
                continue;
            }
            let source = decode_path(&source)?;
            let directory = source.parent().unwrap_or(Path::new(""));
            let mut paths = Vec::with_capacity(8);
            let mut provenance = "relative_module_path";
            if role == "include" {
                let include = name.replace('\\', "/");
                paths.extend(config::normalized(&directory.join(&include)));
                paths.extend(config::normalized(Path::new(&include)));
                provenance = "observed_include_path";
            } else if name.starts_with("./") || name.starts_with("../") {
                paths.extend(config::normalized(&directory.join(&name)));
            } else if language == "luau" {
                if name.starts_with('@') {
                    paths = config::luau(&transaction, directory, &name)?;
                    provenance = "luaurc_alias";
                } else {
                    paths = config::rojo(&transaction, &name)?;
                    provenance = "configured_rojo_mapping";
                }
            } else if matches!(language.as_str(), "typescript" | "javascript") {
                paths = config::typescript(&transaction, directory, &name)?;
                provenance = "typescript_paths";
            }
            let mut candidates = Vec::with_capacity(8);
            for path in paths.into_iter().take(64) {
                for path in variants(&path, &language) {
                    let candidate: Option<i64> = transaction.query_row(
                        "SELECT definitions.id FROM definitions JOIN files ON files.id=definitions.file_id WHERE files.path=?1 AND definitions.kind='module'",
                        [encode_path(&path)], |row| row.get(0),
                    ).optional()?;
                    if let Some(candidate) = candidate {
                        candidates.push(candidate);
                    }
                }
            }
            candidates.sort_unstable();
            candidates.dedup();
            let exact = config::normalized(&directory.join(&name)).map(|path| encode_path(&path));
            let exact_target: Option<i64> = if let Some(exact) = exact {
                transaction.query_row("SELECT definitions.id FROM definitions JOIN files ON files.id=definitions.file_id WHERE files.path=?1 AND definitions.kind='module'", [exact], |row| row.get(0)).optional()?
            } else {
                None
            };
            let target = if candidates.len() == 1
                && role == "import_path"
                && provenance == "relative_module_path"
                && exact_target == Some(candidates[0])
            {
                Some(candidates[0])
            } else {
                None
            };
            if !candidates.is_empty() {
                transaction.execute(
                    "UPDATE occurrences SET target=?1,candidates=?2,provenance=?3 WHERE id=?4",
                    params![
                        target,
                        serde_json::to_string(&candidates)?,
                        format!("{original_provenance};{provenance}"),
                        id
                    ],
                )?;
                for candidate in &candidates {
                    transaction.execute(
                        "INSERT INTO relationships(source,target,kind,file_id,start,end,provenance) SELECT definitions.id,?1,?2,occurrences.file_id,occurrences.start,occurrences.end,occurrences.provenance FROM occurrences JOIN definitions ON definitions.file_id=occurrences.file_id AND definitions.kind='module' WHERE occurrences.id=?3",
                        params![candidate, if role == "include" { "include_candidate" } else if target.is_some() { "import" } else { "import_candidate" }, id],
                    )?;
                }
            }
        }
    }
    transaction.commit()?;
    Ok(())
}

fn variants(path: &Path, language: &str) -> Vec<PathBuf> {
    if language == "dreammaker" {
        return vec![path.to_path_buf()];
    }
    let mut variants = Vec::with_capacity(16);
    variants.push(path.to_path_buf());
    let extensions: &[&str] = if language == "luau" {
        &["luau", "lua"]
    } else {
        &["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"]
    };
    if path.extension().is_none() {
        for extension in extensions {
            variants.push(path.with_extension(extension));
            variants.push(
                path.join(if language == "luau" { "init" } else { "index" })
                    .with_extension(extension),
            );
        }
    } else if language != "luau"
        && path
            .extension()
            .is_some_and(|extension| extension == "js" || extension == "jsx")
    {
        variants.push(path.with_extension("ts"));
        variants.push(path.with_extension("tsx"));
    }
    variants
}

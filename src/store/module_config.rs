use super::encode_path;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

pub(super) fn normalized(path: &Path) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => (),
            Component::ParentDir => {
                if !result.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(result)
}

pub(super) fn read(conn: &Connection, path: &Path) -> Result<Option<Value>> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT contents.bytes FROM files JOIN contents USING(revision) WHERE path=?1",
            [encode_path(path)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(bytes.and_then(|bytes| json_with_comments(&bytes)))
}

pub(super) fn nearest(
    conn: &Connection,
    directory: &Path,
    filename: &str,
) -> Result<Option<(PathBuf, Value)>> {
    let mut parent = Some(directory);
    while let Some(directory) = parent {
        let path = directory.join(filename);
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1)",
            [encode_path(&path)],
            |row| row.get(0),
        )?;
        if exists {
            return Ok(read(conn, &path)?.map(|value| (directory.to_path_buf(), value)));
        }
        parent = directory.parent();
    }
    Ok(None)
}

pub(super) fn typescript(conn: &Connection, directory: &Path, name: &str) -> Result<Vec<PathBuf>> {
    let config = nearest(conn, directory, "tsconfig.json")?;
    let config = match config {
        Some(config) => Some(config),
        None => nearest(conn, directory, "jsconfig.json")?,
    };
    let Some((directory, config)) = config else {
        return Ok(Vec::new());
    };
    if config.get("extends").is_some() {
        return Ok(Vec::new());
    }
    let options = &config["compilerOptions"];
    let base = directory.join(options["baseUrl"].as_str().unwrap_or("."));
    let mut paths = Vec::with_capacity(8);
    if let Some(mapping) = options["paths"].as_object() {
        for (pattern, replacements) in mapping {
            let wildcard = if let Some((prefix, suffix)) = pattern.split_once('*') {
                if suffix.contains('*') {
                    continue;
                }
                name.strip_prefix(prefix)
                    .and_then(|tail| tail.strip_suffix(suffix))
            } else if pattern == name {
                Some("")
            } else {
                None
            };
            let Some(wildcard) = wildcard else {
                continue;
            };
            for replacement in replacements
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(32)
            {
                if replacement.matches('*').count() > 1 {
                    continue;
                }
                if let Some(path) = normalized(&base.join(replacement.replace('*', wildcard))) {
                    paths.push(path);
                }
            }
        }
    }
    if options["baseUrl"].is_string()
        && let Some(path) = normalized(&base.join(name))
    {
        paths.push(path);
    }
    Ok(paths)
}

pub(super) fn luau(conn: &Connection, directory: &Path, name: &str) -> Result<Vec<PathBuf>> {
    let Some((directory, config)) = nearest(conn, directory, ".luaurc")? else {
        return Ok(Vec::new());
    };
    let Some(name) = name.strip_prefix('@') else {
        return Ok(Vec::new());
    };
    let (alias, tail) = name.split_once('/').unwrap_or((name, ""));
    let Some(mapping) = config["aliases"][alias].as_str() else {
        return Ok(Vec::new());
    };
    Ok(normalized(&directory.join(mapping).join(tail))
        .into_iter()
        .collect())
}

pub(super) fn rojo(conn: &Connection, name: &str) -> Result<Vec<PathBuf>> {
    let Some(config) = read(conn, Path::new("trufflepig.json"))? else {
        return Ok(Vec::new());
    };
    let Some(project) = config["rojo_project"]
        .as_str()
        .and_then(|path| normalized(Path::new(path)))
    else {
        return Ok(Vec::new());
    };
    let Some(project_config) = read(conn, &project)? else {
        return Ok(Vec::new());
    };
    let directory = project.parent().unwrap_or(Path::new(""));
    let name = name.strip_prefix("game.").unwrap_or(name).replace('.', "/");
    let mut node = &project_config["tree"];
    let mut matched = None;
    let mut segments = name.split('/').peekable();
    while let Some(segment) = segments.next() {
        node = &node[segment];
        if node.is_null() {
            break;
        }
        if let Some(path) = node["$path"].as_str() {
            let mut path = directory.join(path);
            for remaining in segments.clone() {
                path.push(remaining);
            }
            matched = normalized(&path);
        }
    }
    Ok(matched.into_iter().collect())
}

fn json_with_comments(bytes: &[u8]) -> Option<Value> {
    let mut clean = bytes.to_vec();
    let (mut index, mut quoted, mut escaped) = (0, false, false);
    while index < clean.len() {
        let byte = clean[index];
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        } else if byte == b'/' && clean.get(index + 1) == Some(&b'/') {
            while index < clean.len() && clean[index] != b'\n' {
                clean[index] = b' ';
                index += 1;
            }
            continue;
        } else if byte == b'/' && clean.get(index + 1) == Some(&b'*') {
            clean[index] = b' ';
            clean[index + 1] = b' ';
            index += 2;
            while index + 1 < clean.len() && !(clean[index] == b'*' && clean[index + 1] == b'/') {
                clean[index] = b' ';
                index += 1;
            }
            if index + 1 >= clean.len() {
                return None;
            }
            clean[index] = b' ';
            clean[index + 1] = b' ';
            index += 2;
            continue;
        }
        index += 1;
    }
    let (mut index, mut quoted, mut escaped) = (0, false, false);
    while index < clean.len() {
        let byte = clean[index];
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        } else if byte == b',' {
            let mut next = index + 1;
            while clean.get(next).is_some_and(u8::is_ascii_whitespace) {
                next += 1;
            }
            if clean
                .get(next)
                .is_some_and(|byte| matches!(byte, b']' | b'}'))
            {
                clean[index] = b' ';
            }
        }
        index += 1;
    }
    serde_json::from_slice(&clean).ok()
}

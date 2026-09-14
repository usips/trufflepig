use crate::extract::Definition;
use anyhow::Result;
use rusqlite::{Transaction, params};

const REGION_BYTES: usize = 4096;

pub(super) fn insert(
    tx: &Transaction<'_>,
    file_id: i64,
    path: &str,
    source: &[u8],
    definitions: &[Definition],
) -> Result<()> {
    let mut boundaries = Vec::with_capacity(definitions.len() * 2);
    boundaries.extend(
        definitions
            .iter()
            .flat_map(|definition| [definition.start, definition.end]),
    );
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut ordered: Vec<usize> = (0..definitions.len()).collect();
    ordered.sort_unstable_by_key(|&index| definitions[index].start);
    let mut upcoming = ordered.into_iter().peekable();
    let mut active = std::collections::BinaryHeap::with_capacity(definitions.len());
    let mut start = 0;
    loop {
        let limit = (start + REGION_BYTES).min(source.len());
        let mut end = boundaries
            .get(boundaries.partition_point(|&boundary| boundary <= start))
            .copied()
            .filter(|&boundary| boundary <= limit)
            .unwrap_or(limit);
        if end == limit
            && limit < source.len()
            && let Some(newline) = source[start..limit].iter().rposition(|&byte| byte == b'\n')
        {
            end = start + newline + 1;
        }
        while upcoming
            .peek()
            .is_some_and(|&index| definitions[index].start <= start)
        {
            let index = upcoming.next().expect("peeked definition");
            let definition = &definitions[index];
            active.push(std::cmp::Reverse((
                definition.end.saturating_sub(definition.start),
                index,
            )));
        }
        while active
            .peek()
            .is_some_and(|&std::cmp::Reverse((_, index))| definitions[index].end <= start)
        {
            active.pop();
        }
        let symbol = active
            .peek()
            .map(|&std::cmp::Reverse((_, index))| &definitions[index]);
        let (name, kind) = symbol
            .map(|definition| (definition.name.as_str(), definition.kind.as_str()))
            .unwrap_or((path, "file"));
        let body = String::from_utf8_lossy(&source[start..end]);
        tx.execute(
            "INSERT INTO regions(file_id,start,end,name,kind,body) VALUES(?1,?2,?3,?4,?5,?6)",
            params![file_id, start as i64, end as i64, name, kind, body.as_ref()],
        )?;
        let region_id = tx.last_insert_rowid();
        let expanded = expand_identifiers(&format!("{name} {body}"));
        tx.execute(
            "INSERT INTO documents(rowid,name,expanded,body,path) VALUES(?1,?2,?3,?4,?5)",
            params![region_id, name, expanded, body.as_ref(), path],
        )?;
        if end == source.len() {
            break;
        }
        start = end;
    }
    Ok(())
}

fn expand_identifiers(source: &str) -> String {
    let mut expanded = String::with_capacity(source.len() + source.len() / 4);
    let mut previous = ' ';
    let mut chars = source.chars().peekable();
    while let Some(current) = chars.next() {
        if current == '_' || !current.is_alphanumeric() {
            expanded.push(' ');
        } else {
            if current.is_uppercase()
                && (previous.is_lowercase()
                    || previous.is_ascii_digit()
                    || (previous.is_uppercase()
                        && chars.peek().is_some_and(|next| next.is_lowercase())))
            {
                expanded.push(' ');
            }
            expanded.extend(current.to_lowercase());
        }
        previous = current;
    }
    expanded
}

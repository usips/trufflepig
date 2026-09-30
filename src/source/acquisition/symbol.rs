//! `show 'sym:NAME [file:P] [lang:L] [kind:K]'`: reads the best-ranked
//! definition as a verified handle read and lists its namesakes. An import that
//! outranks every declaration is followed to the declaration it names.
use super::{AcquiredSource, acquire_entry};
use crate::{
    results::{self, Hit, ResultEntry},
    search::{ImportHop, InvocationDirectory, Query},
    store::Store,
};
use anyhow::{Result, bail};

/// Other same-named definitions listed after a `sym:` read.
const SYMBOL_ALTERNATIVES: usize = 5;

/// The candidates behind a `sym:` read: declaration and import counts,
/// `PATH:START-END KIND` locators for up to `SYMBOL_ALTERNATIVES` others
/// (tied declarations, then the ranked rest), and the import trail when the
/// best row was an import.
#[derive(Debug)]
pub(crate) struct SymbolSelection {
    pub declarations: usize,
    pub imports: usize,
    pub also: Vec<String>,
    pub import: Option<ImportNote>,
}

/// Imports a `sym:` read passed through, whether they reached the shown
/// declaration, and how many declarations tied for it (1 when unique).
#[derive(Debug)]
pub(crate) struct ImportNote {
    pub hops: Vec<ImportHop>,
    pub followed: bool,
    pub candidates: usize,
}

/// The `show` target in command words: a `sym:` target takes every following
/// word as part of its query (`show sym:X file:src/`); others take one word.
pub(crate) fn show_target(words: &[String]) -> Option<String> {
    let first = words.get(1)?;
    Some(if first.starts_with("sym:") {
        words[1..].join(" ")
    } else {
        first.clone()
    })
}

pub(super) fn acquire_symbol(
    store: &Store,
    target: &str,
    origin: &InvocationDirectory,
) -> Result<AcquiredSource> {
    let query = Query::parse(target)?;
    let found = crate::search::definitions(store, &query, origin)?;
    if found.hits.is_empty() {
        bail!(no_definition(store, &query, origin)?);
    }
    let imports = found.hits.iter().filter(|hit| hit.kind == "import").count();
    let declarations = found.hits.len() - imports;
    let trail = if found.hits[0].kind == "import" {
        crate::search::trace_import(store, &found.hits[0], &query, origin)?
    } else {
        None
    };
    // Saved order: the reached declaration and its ties (when followed), then
    // the ranked namesakes. `also:` skips the shown row and a followed import.
    let mut shown: Vec<Hit> = Vec::with_capacity(found.hits.len() + 1);
    let mut import = None;
    if let Some(trail) = trail {
        let followed = trail.declaration.is_some();
        import = Some(ImportNote {
            hops: trail.hops,
            followed,
            candidates: 1 + trail.ties.len(),
        });
        shown.extend(trail.declaration);
        shown.extend(trail.ties);
    }
    let lead = shown.len();
    shown.extend(found.hits);
    let also = shown
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 0 && !(lead > 0 && *index == lead))
        .take(SYMBOL_ALTERNATIVES)
        .map(|(_, hit)| {
            format!(
                "{}:{}-{} {}",
                hit.path, hit.start_line, hit.end_line, hit.kind
            )
        })
        .collect();
    let first = ResultEntry::LiveSource(shown[0].clone());
    let set = results::save_entries(
        store,
        found.generation,
        found.coverage,
        shown.into_iter().map(ResultEntry::LiveSource).collect(),
        found.truncated,
    )?;
    let handle = format!("{set}:1");
    let mut source = acquire_entry(store, &handle, first, None)?;
    source.definitions = Some(SymbolSelection {
        declarations,
        imports,
        also,
        import,
    });
    Ok(source)
}

/// `no_definition` naming the active filters and, when they excluded every
/// namesake, how many exist without them and where the best one is.
fn no_definition(store: &Store, query: &Query, origin: &InvocationDirectory) -> Result<String> {
    let name = &query.text;
    let filters: Vec<String> = query
        .path
        .includes()
        .iter()
        .map(|value| format!("file:{value}"))
        .chain(
            query
                .path
                .excludes()
                .iter()
                .map(|value| format!("-file:{value}")),
        )
        .chain(
            [("lang:", &query.language), ("kind:", &query.kind)]
                .into_iter()
                .filter(|(_, value)| !value.is_empty())
                .map(|(key, value)| format!("{key}{value}")),
        )
        .collect();
    let mut message = format!("no_definition: no indexed definition named `{name}`");
    if !filters.is_empty() {
        message.push_str(&format!(" matching {}", filters.join(" ")));
        let unfiltered = Query {
            text: name.clone(),
            exact: true,
            ..Query::default()
        };
        let others = crate::search::definitions(store, &unfiltered, origin)?.hits;
        if let Some(best) = others.first() {
            message.push_str(&format!(
                "; {} without filters, best {}:{}-{} {}",
                others.len(),
                best.path,
                best.start_line,
                best.end_line,
                best.kind
            ));
        }
    }
    message.push_str(&format!("; try `refs {name}` or `search '{name}'`"));
    Ok(message)
}

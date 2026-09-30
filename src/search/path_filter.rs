//! `file:` filters. Repeated `file:` values OR together and `-file:` values
//! exclude. An include value matches paths it prefixes from the root when some
//! indexed path continues it at a component or stem boundary; otherwise it
//! matches at any path-component start (`script/host` finds
//! `crates/a/src/script/host.rs`). Excludes always match at any component start.
//! [`BoundPathFilter`] is the one matcher SQL lanes and live walks share.
mod filter_diagnosis;
#[cfg(test)]
mod tests;

pub use filter_diagnosis::diagnose_empty_filters;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::fmt::Write;

/// Unbound `file:` and `-file:` values in query order, encoded like indexed paths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathFilter {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl PathFilter {
    /// A filter of one include value, as `map PREFIX` uses.
    pub fn prefix(value: &str) -> Self {
        let mut filter = Self::default();
        filter.include(value);
        filter
    }

    /// Adds a `file:` value; an empty value (the whole root) adds nothing.
    pub fn include(&mut self, value: &str) {
        if let Some(value) = encoded_value(value) {
            self.include.push(value);
        }
    }

    /// Adds a `-file:` value.
    pub fn exclude(&mut self, value: &str) {
        if let Some(value) = encoded_value(value) {
            self.exclude.push(value);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    pub fn includes(&self) -> &[String] {
        &self.include
    }

    pub fn excludes(&self) -> &[String] {
        &self.exclude
    }

    /// Fixes each include value to root-prefix matching when an indexed path
    /// continues it at `/`, `.`, or its end, else to component matching.
    pub fn bind(&self, conn: &Connection) -> Result<BoundPathFilter> {
        let mut include = Vec::with_capacity(self.include.len());
        for value in &self.include {
            let anchored = anchors_at_root(conn, value)?;
            include.push(PathNeedle::new(value, anchored));
        }
        Ok(BoundPathFilter {
            include,
            exclude: self
                .exclude
                .iter()
                .map(|value| PathNeedle::new(value, false))
                .collect(),
        })
    }

    /// The one indexed path this filter matches, if exactly one does; `map`
    /// uses it to list a named file's members.
    pub fn resolve_single_file(&self, conn: &Connection) -> Result<Option<String>> {
        if let [value] = self.include.as_slice()
            && self.exclude.is_empty()
        {
            let exact = conn
                .query_row("SELECT path FROM files WHERE path=?1", [value], |row| {
                    row.get(0)
                })
                .optional()?;
            if exact.is_some() {
                return Ok(exact);
            }
        }
        let bound = self.bind(conn)?;
        if bound.include.is_empty() {
            return Ok(None);
        }
        let mut statement = conn.prepare(&format!(
            "SELECT path FROM files f WHERE 1{} ORDER BY path LIMIT 2",
            bound.sql_clause("f.path")
        ))?;
        let paths = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(match <[String; 1]>::try_from(paths) {
            Ok([path]) if names_file(&path, &self.include) => Some(path),
            _ => None,
        })
    }
}

/// Whether a value ends inside `path`'s basename, at its end or before a `.`
/// suffix: `host.rs` and `script/host` name a file, `src/` does not.
fn names_file(path: &str, values: &[String]) -> bool {
    values.iter().any(|value| {
        let end = if path.starts_with(value.as_str()) {
            value.len()
        } else {
            match path.find(&format!("/{value}")) {
                Some(start) => start + 1 + value.len(),
                None => return false,
            }
        };
        let rest = &path[end..];
        (rest.is_empty() || rest.starts_with('.')) && !rest.contains('/')
    })
}

/// Strips `./` and percent-encodes bytes as [`crate::store::encode_path`] does,
/// keeping `%` so already-encoded values pass through.
fn encoded_value(value: &str) -> Option<String> {
    let mut value = value;
    while let Some(rest) = value.strip_prefix("./") {
        value = rest;
    }
    if value.is_empty() {
        return None;
    }
    let mut encoded = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        if byte == b'%' || ((0x21..=0x7e).contains(&byte) && !matches!(byte, b':' | b'@')) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    Some(encoded)
}

/// Whether an indexed path starts with `value` where a component or stem ends.
fn anchors_at_root(conn: &Connection, value: &str) -> Result<bool> {
    let exists = |low: &str, high: &str| -> Result<bool> {
        Ok(conn
            .query_row(
                "SELECT 1 FROM files WHERE path>=?1 AND path<?2 LIMIT 1",
                params![low, high],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    if value.ends_with('/') {
        return exists(value, &upper_bound(value));
    }
    // `.` (0x2e) and `/` (0x2f) are adjacent, so one range covers `VALUE.` and
    // `VALUE/`; paths are printable ASCII, so `[VALUE, VALUE\x01)` is VALUE alone.
    Ok(exists(&format!("{value}."), &format!("{value}0"))?
        || exists(value, &format!("{value}\u{1}"))?)
}

/// The least string greater than every string starting with `prefix`; filter
/// values are ASCII, so incrementing the last byte suffices.
fn upper_bound(prefix: &str) -> String {
    let mut bytes = prefix.as_bytes().to_vec();
    if let Some(last) = bytes.last_mut() {
        *last += 1;
    }
    String::from_utf8(bytes).expect("encoded filter values are ASCII")
}

fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// A value stored as `/VALUE`, so a component match is a substring match of
/// `/PATH` and a root-prefix match is that substring at offset zero.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PathNeedle {
    slashed: String,
    anchored: bool,
}

impl PathNeedle {
    fn new(value: &str, anchored: bool) -> Self {
        let mut slashed = String::with_capacity(value.len() + 1);
        slashed.push('/');
        slashed.push_str(value);
        Self { slashed, anchored }
    }

    fn value(&self) -> &str {
        &self.slashed[1..]
    }

    fn matches(&self, path: &str) -> bool {
        path.starts_with(self.value()) || (!self.anchored && path.contains(&self.slashed))
    }

    /// A root prefix is an index range; a component match adds `instr`.
    fn sql(&self, column: &str) -> String {
        let range = format!(
            "({column}>={} AND {column}<{})",
            sql_literal(self.value()),
            sql_literal(&upper_bound(self.value()))
        );
        if self.anchored {
            range
        } else {
            format!(
                "({range} OR instr({column},{})>0)",
                sql_literal(&self.slashed)
            )
        }
    }
}

/// A [`PathFilter`] with every include value's matching mode fixed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoundPathFilter {
    include: Vec<PathNeedle>,
    exclude: Vec<PathNeedle>,
}

impl BoundPathFilter {
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Whether a root-relative indexed path passes the filter.
    pub fn matches(&self, path: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|needle| needle.matches(path)))
            && !self.exclude.iter().any(|needle| needle.matches(path))
    }

    /// ` AND PREDICATE` equal to [`Self::matches`] on `column`, or nothing when
    /// the filter is empty. Values are inlined as escaped SQL literals.
    pub fn sql_clause(&self, column: &str) -> String {
        let mut clause = String::new();
        if !self.include.is_empty() {
            let alternatives: Vec<_> = self
                .include
                .iter()
                .map(|needle| needle.sql(column))
                .collect();
            let _ = write!(clause, " AND ({})", alternatives.join(" OR "));
        }
        for needle in &self.exclude {
            let _ = write!(clause, " AND NOT {}", needle.sql(column));
        }
        clause
    }
}

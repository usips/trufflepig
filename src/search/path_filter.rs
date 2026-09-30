//! `file:` filters. Repeated `file:` values OR together and `-file:` values
//! exclude. An include value matches paths it prefixes from the root; when no
//! indexed path has that prefix it matches at any path-component start instead
//! (`script/host` finds `crates/a/src/script/host.rs`). Excludes always match
//! at any component start. [`BoundPathFilter`] is the one matcher SQL lanes and
//! live walks share.
mod filter_diagnosis;
#[cfg(test)]
mod tests;

pub use filter_diagnosis::diagnose_empty_filters;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// Unbound `file:` and `-file:` values in query order.
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
        let value = normalized_value(value);
        if !value.is_empty() {
            self.include.push(value.to_owned());
        }
    }

    /// Adds a `-file:` value.
    pub fn exclude(&mut self, value: &str) {
        let value = normalized_value(value);
        if !value.is_empty() {
            self.exclude.push(value.to_owned());
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

    /// Fixes each include value to root-prefix matching when some indexed path
    /// has that prefix, else to component matching.
    pub fn bind(&self, conn: &Connection) -> Result<BoundPathFilter> {
        let mut include = Vec::with_capacity(self.include.len());
        for value in &self.include {
            let anchored = indexed_prefix_exists(conn, value)?;
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
}

/// Whether any indexed path starts with `value`; uses the `files.path` index.
fn indexed_prefix_exists(conn: &Connection, value: &str) -> Result<bool> {
    let first: Option<String> = conn
        .query_row(
            "SELECT path FROM files WHERE path>=?1 ORDER BY path LIMIT 1",
            [value],
            |row| row.get(0),
        )
        .optional()?;
    Ok(first.is_some_and(|path| path.starts_with(value)))
}

fn normalized_value(value: &str) -> &str {
    let mut value = value;
    while let Some(rest) = value.strip_prefix("./") {
        value = rest;
    }
    value
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

    fn matches(&self, path: &str) -> bool {
        path.starts_with(&self.slashed[1..]) || (!self.anchored && path.contains(&self.slashed))
    }
}

/// A [`PathFilter`] with every include value's matching mode fixed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoundPathFilter {
    include: Vec<PathNeedle>,
    exclude: Vec<PathNeedle>,
}

impl BoundPathFilter {
    /// Whether a root-relative indexed path passes the filter.
    pub fn matches(&self, path: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|needle| needle.matches(path)))
            && !self.exclude.iter().any(|needle| needle.matches(path))
    }

    /// The JSON parameter [`path_filter_sql`] reads.
    pub fn sql_parameter(&self) -> String {
        let include: Vec<_> = self
            .include
            .iter()
            .map(|needle| serde_json::json!([needle.slashed, needle.anchored]))
            .collect();
        let exclude: Vec<_> = self.exclude.iter().map(|needle| &needle.slashed).collect();
        serde_json::json!({"i": include, "x": exclude}).to_string()
    }
}

/// SQL predicate equal to [`BoundPathFilter::matches`] on `column`, reading the
/// filter's [`BoundPathFilter::sql_parameter`] from parameter `?N`.
pub fn path_filter_sql(column: &str, parameter: usize) -> String {
    let filter = format!("?{parameter}");
    let slashed = format!("'/'||{column}");
    format!(
        "((json_array_length({filter},'$.i')=0 OR EXISTS(SELECT 1 FROM json_each({filter},'$.i') n \
         WHERE instr({slashed},n.value->>0)=1 OR (n.value->>1=0 AND instr({slashed},n.value->>0)>0))) \
         AND (json_array_length({filter},'$.x')=0 OR NOT EXISTS(SELECT 1 FROM json_each({filter},'$.x') n \
         WHERE instr({slashed},n.value)>0)))"
    )
}

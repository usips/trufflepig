//! Source-byte extraction with file-local definition indices and explicit provenance.
//! Parsing failure leaves lexical indexing available to the caller.

mod binding_index;
mod bindings;
mod dreammaker;
#[cfg(test)]
mod resolution_tests;
mod syntax;
mod syntax_modules;
mod syntax_names;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Extraction {
    pub language: String,
    pub status: String,
    pub definitions: Vec<Definition>,
    pub occurrences: Vec<Occurrence>,
    pub relationships: Vec<Relationship>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Definition {
    pub name: String,
    pub kind: String,
    pub start: usize,
    pub end: usize,
    pub container: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Occurrence {
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub role: String,
    pub target: Option<usize>,
    pub candidates: Vec<usize>,
    pub provenance: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Relationship {
    pub source: usize,
    pub target: Option<usize>,
    pub kind: String,
    pub evidence_start: usize,
    pub evidence_end: usize,
    pub provenance: String,
}

pub fn language(path: &str) -> &str {
    match path.rsplit('.').next().unwrap_or("") {
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "luau" | "lua" => "luau",
        "dm" | "dme" | "dmf" => "dreammaker",
        _ => "text",
    }
}

pub fn extract(path: &str, source: &[u8]) -> Extraction {
    match language(path) {
        "dreammaker" => dreammaker::extract(source),
        "text" => Extraction {
            language: "text".into(),
            status: "lexical_only".into(),
            ..Extraction::default()
        },
        _ => syntax::extract(path, source),
    }
}

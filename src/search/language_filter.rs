//! `lang:` values: the recorded language names of [`crate::extract::language`]
//! and their aliases. Unknown names fail instead of silently matching nothing.
use anyhow::{Result, bail};

/// Every language name the index records, in help order.
pub const INDEXED_LANGUAGES: [&str; 8] = [
    "rust",
    "typescript",
    "javascript",
    "csharp",
    "php",
    "luau",
    "dreammaker",
    "text",
];

/// The recorded language a `lang:` value names, case-insensitively.
pub fn canonical_language(value: &str) -> Result<&'static str> {
    let lower = value.to_ascii_lowercase();
    if let Some(known) = INDEXED_LANGUAGES.iter().find(|known| **known == lower) {
        return Ok(known);
    }
    Ok(match lower.as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "cs" | "c#" => "csharp",
        "phtml" => "php",
        "lua" => "luau",
        "dm" => "dreammaker",
        "md" | "markdown" | "toml" | "json" | "txt" => "text",
        _ => bail!(
            "unknown_language: {value}; known: {} (other files, including md/markdown/toml/json, are text)",
            INDEXED_LANGUAGES.join(", ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_language_accepts_aliases_and_names_known_languages() {
        assert_eq!(canonical_language("Rust").unwrap(), "rust");
        assert_eq!(canonical_language("c#").unwrap(), "csharp");
        assert_eq!(canonical_language("markdown").unwrap(), "text");
        assert_eq!(canonical_language("md").unwrap(), "text");
        let error = canonical_language("python").unwrap_err().to_string();
        assert!(error.starts_with("unknown_language: python; known: rust, typescript"));
        assert!(error.contains("md/markdown/toml/json, are text"));
    }
}

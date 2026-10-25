use super::MAX_METADATA_SOURCE_BYTES;
use super::ManifestRequirement;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::HashMap;
use std::ops::Range;

const MAX_JSON_TOKENS: usize = 100_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonTokenKind {
    ObjectStart,
    ObjectEnd,
    ArrayStart,
    ArrayEnd,
    Colon,
    Comma,
    String,
    Scalar,
}

struct JsonToken {
    kind: JsonTokenKind,
    span: Range<usize>,
    value: Option<String>,
}

pub(super) fn parse_manifest(source: &[u8]) -> Result<Vec<ManifestRequirement>> {
    ensure!(
        source.len() <= MAX_METADATA_SOURCE_BYTES,
        "manifest exceeds source limit"
    );
    let value: Value = serde_json::from_slice(source).context("invalid add-on JSON")?;
    let root = value
        .as_object()
        .context("add-on manifest root is not an object")?;
    let Some(require) = root.get("require") else {
        return Ok(Vec::new());
    };
    let tokens = tokenize_json(source)?;
    let root_members = object_members(&tokens, 0)?;
    let require_members: Vec<_> = root_members
        .into_iter()
        .filter(|(key, _, _)| tokens[*key].value.as_deref() == Some("require"))
        .collect();
    ensure!(require_members.len() == 1, "duplicate require field");
    let (_, value_start, value_end) = require_members[0];
    if require.as_array().is_some_and(Vec::is_empty) {
        ensure!(
            tokens[value_start].kind == JsonTokenKind::ArrayStart
                && tokens[value_end].kind == JsonTokenKind::ArrayEnd,
            "unsupported empty add-on require shape"
        );
        return Ok(Vec::new());
    }
    ensure!(require.is_object(), "add-on require field is not an object");
    ensure!(
        tokens[value_start].kind == JsonTokenKind::ObjectStart
            && tokens[value_end].kind == JsonTokenKind::ObjectEnd,
        "unsupported add-on require shape"
    );
    let members = object_members(&tokens, value_start)?;
    let mut counts = HashMap::<String, usize>::with_capacity(members.len());
    for (key, _, _) in &members {
        *counts
            .entry(tokens[*key].value.clone().unwrap_or_default())
            .or_default() += 1;
    }
    Ok(members
        .into_iter()
        .filter_map(|(key, _, _)| {
            let name = tokens[key].value.clone()?;
            Some(ManifestRequirement {
                duplicate: counts.get(&name).copied().unwrap_or(0) > 1,
                name,
                span: tokens[key].span.clone(),
            })
        })
        .collect())
}

fn tokenize_json(source: &[u8]) -> Result<Vec<JsonToken>> {
    let mut tokens = Vec::with_capacity(source.len().div_ceil(8).min(MAX_JSON_TOKENS));
    let mut cursor = 0;
    while cursor < source.len() {
        if source[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        let start = cursor;
        let kind = match source[cursor] {
            b'{' => JsonTokenKind::ObjectStart,
            b'}' => JsonTokenKind::ObjectEnd,
            b'[' => JsonTokenKind::ArrayStart,
            b']' => JsonTokenKind::ArrayEnd,
            b':' => JsonTokenKind::Colon,
            b',' => JsonTokenKind::Comma,
            b'"' => {
                cursor += 1;
                while cursor < source.len() {
                    match source[cursor] {
                        b'\\' => cursor = (cursor + 2).min(source.len()),
                        b'"' => {
                            cursor += 1;
                            break;
                        }
                        _ => cursor += 1,
                    }
                }
                ensure!(
                    source.get(cursor.saturating_sub(1)) == Some(&b'"'),
                    "unterminated JSON string"
                );
                let value: String = serde_json::from_slice(&source[start..cursor])?;
                tokens.push(JsonToken {
                    kind: JsonTokenKind::String,
                    span: start..cursor,
                    value: Some(value),
                });
                ensure!(tokens.len() <= MAX_JSON_TOKENS, "JSON token limit exceeded");
                continue;
            }
            _ => {
                while cursor < source.len()
                    && !source[cursor].is_ascii_whitespace()
                    && !matches!(source[cursor], b'{' | b'}' | b'[' | b']' | b':' | b',')
                {
                    cursor += 1;
                }
                ensure!(cursor > start, "invalid JSON token");
                tokens.push(JsonToken {
                    kind: JsonTokenKind::Scalar,
                    span: start..cursor,
                    value: None,
                });
                ensure!(tokens.len() <= MAX_JSON_TOKENS, "JSON token limit exceeded");
                continue;
            }
        };
        cursor += 1;
        tokens.push(JsonToken {
            kind,
            span: start..cursor,
            value: None,
        });
        ensure!(tokens.len() <= MAX_JSON_TOKENS, "JSON token limit exceeded");
    }
    Ok(tokens)
}

fn object_members(tokens: &[JsonToken], object_start: usize) -> Result<Vec<(usize, usize, usize)>> {
    ensure!(
        tokens
            .get(object_start)
            .is_some_and(|token| token.kind == JsonTokenKind::ObjectStart),
        "expected JSON object"
    );
    let mut members = Vec::new();
    let mut cursor = object_start + 1;
    while cursor < tokens.len() && tokens[cursor].kind != JsonTokenKind::ObjectEnd {
        let key = cursor;
        ensure!(
            tokens[key].kind == JsonTokenKind::String,
            "expected JSON object key"
        );
        cursor += 1;
        ensure!(
            tokens
                .get(cursor)
                .is_some_and(|token| token.kind == JsonTokenKind::Colon),
            "expected JSON colon"
        );
        let value_start = cursor + 1;
        let value_end = value_end(tokens, value_start)?;
        members.push((key, value_start, value_end));
        cursor = value_end + 1;
        if tokens
            .get(cursor)
            .is_some_and(|token| token.kind == JsonTokenKind::Comma)
        {
            cursor += 1;
        } else {
            ensure!(
                tokens
                    .get(cursor)
                    .is_some_and(|token| token.kind == JsonTokenKind::ObjectEnd),
                "expected JSON object end"
            );
            break;
        }
    }
    ensure!(
        tokens
            .get(cursor)
            .is_some_and(|token| token.kind == JsonTokenKind::ObjectEnd),
        "unterminated JSON object"
    );
    Ok(members)
}

fn value_end(tokens: &[JsonToken], start: usize) -> Result<usize> {
    let token = tokens.get(start).context("missing JSON value")?;
    match token.kind {
        JsonTokenKind::ObjectStart | JsonTokenKind::ArrayStart => {
            let mut stack = Vec::with_capacity(8);
            stack.push(token.kind);
            for (index, next) in tokens.iter().enumerate().skip(start + 1) {
                match next.kind {
                    JsonTokenKind::ObjectStart | JsonTokenKind::ArrayStart => stack.push(next.kind),
                    JsonTokenKind::ObjectEnd => {
                        ensure!(
                            stack.pop() == Some(JsonTokenKind::ObjectStart),
                            "mismatched JSON object"
                        );
                        if stack.is_empty() {
                            return Ok(index);
                        }
                    }
                    JsonTokenKind::ArrayEnd => {
                        ensure!(
                            stack.pop() == Some(JsonTokenKind::ArrayStart),
                            "mismatched JSON array"
                        );
                        if stack.is_empty() {
                            return Ok(index);
                        }
                    }
                    _ => {}
                }
            }
            bail!("unterminated JSON value")
        }
        JsonTokenKind::ObjectEnd
        | JsonTokenKind::ArrayEnd
        | JsonTokenKind::Colon
        | JsonTokenKind::Comma => {
            bail!("invalid JSON value")
        }
        _ => Ok(start),
    }
}

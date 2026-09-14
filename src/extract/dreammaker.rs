//! Conservative DreamMaker recovery over original bytes; no preprocessor execution.
//! Definitions and evidence are recovered facts, with conditional resolution disabled.

mod dm_candidates;
mod dm_declarations;
mod dm_lex;
mod dm_resolve;
#[cfg(test)]
mod tests;

use super::{Definition, Extraction, Occurrence, Relationship};
use dm_declarations::{add_definition, declaration, recover_parameters};
use dm_lex::{DmToken, tokenize};
use std::collections::HashMap;

const MAX_FACTS: usize = 50_000;
const MAX_CANDIDATES: usize = 64;

struct DmDefinition {
    path: String,
    conditional: bool,
    local_owner: Option<usize>,
    scope_end: usize,
    name_start: usize,
    name_end: usize,
}

struct DmFrame {
    path: String,
    indent: usize,
    brace: Option<usize>,
    definition: Option<usize>,
    procedure: Option<usize>,
}

struct DmEvidence {
    tokens: Vec<DmToken>,
    procedure: Option<usize>,
    conditional: bool,
}

pub(super) fn extract(source: &[u8]) -> Extraction {
    let (lines, complete) = tokenize(source);
    let mut result = Extraction {
        language: "dreammaker".into(),
        status: if complete {
            "recovered"
        } else {
            "recovered_incomplete"
        }
        .into(),
        ..Extraction::default()
    };
    let mut definitions: Vec<DmDefinition> = Vec::new();
    let mut frames: Vec<DmFrame> = Vec::new();
    let mut evidence = Vec::with_capacity(lines.len());
    let mut parents = HashMap::new();
    let mut braces = 0usize;
    let mut conditional_depth = 0usize;
    let mut includes = false;
    let mut bindings: Vec<(usize, usize, usize)> = Vec::new();
    for line in lines {
        let tokens = &line.tokens;
        if tokens.is_empty() {
            continue;
        }
        // A header can add parameters, so reserve its entire token count.
        if result.definitions.len()
            + result.occurrences.len()
            + result.relationships.len()
            + tokens.len()
            + 2
            > MAX_FACTS
        {
            result.status = "recovered_incomplete".into();
            break;
        }
        let closing = tokens.iter().take_while(|t| t.text == "}").count();
        let effective_braces = braces.saturating_sub(closing);
        while bindings
            .last()
            .is_some_and(|(_, indent, depth)| line.indent < *indent || effective_braces < *depth)
        {
            if let Some((index, _, _)) = bindings.pop() {
                definitions[index].scope_end = tokens[0].start;
            }
        }
        if tokens[0].text == "#" {
            let directive = tokens.get(1).map(|t| t.text.as_str()).unwrap_or("");
            match directive {
                "if" | "ifdef" | "ifndef" => conditional_depth += 1,
                "endif" => conditional_depth = conditional_depth.saturating_sub(1),
                "include" => {
                    includes = true;
                    if let Some(path) = tokens.get(2) {
                        result.occurrences.push(Occurrence {
                            name: path.text.trim_matches('"').into(),
                            start: path.start,
                            end: path.end,
                            role: "include".into(),
                            target: None,
                            candidates: Vec::new(),
                            provenance: "dm-recovery:include;preprocessor-not-evaluated".into(),
                        });
                    }
                }
                "define" => {
                    if let Some(name) = tokens.get(2).filter(|t| t.identifier()) {
                        add_definition(
                            &mut result,
                            &mut definitions,
                            name,
                            line.end,
                            "macro",
                            name.text.clone(),
                            None,
                            conditional_depth > 0,
                            None,
                        );
                    }
                }
                _ => {}
            }
            if matches!(directive, "if" | "ifdef" | "ifndef" | "define" | "undef") {
                evidence.push(DmEvidence {
                    tokens: tokens
                        .iter()
                        .skip(2 + usize::from(directive == "define"))
                        .cloned()
                        .collect(),
                    procedure: None,
                    conditional: true,
                });
            }
            continue;
        }
        while frames.last().is_some_and(|frame| match frame.brace {
            Some(depth) => effective_braces < depth,
            None => line.indent <= frame.indent,
        }) {
            close_frame(&mut frames, &mut result, tokens[0].start);
        }
        let procedure = frames.iter().rev().find_map(|f| f.procedure);
        let base = frames.last().map(|f| f.path.as_str()).unwrap_or("");
        let conditional = conditional_depth > 0;
        if procedure.is_none() && tokens[0].text == "parent_type" {
            if let Some(eq) = tokens.iter().position(|t| t.text == "=") {
                let parent = tokens[eq + 1..]
                    .iter()
                    .take_while(|t| t.identifier() || t.text == "/")
                    .map(|t| t.text.as_str())
                    .collect::<String>();
                if parent.starts_with('/') {
                    let parent_end = tokens[eq + 1..]
                        .iter()
                        .take_while(|t| t.identifier() || t.text == "/")
                        .last()
                        .map_or(tokens[eq + 1].end, |token| token.end);
                    result.occurrences.push(Occurrence {
                        name: parent.clone(),
                        start: tokens[eq + 1].start,
                        end: parent_end,
                        role: "parent_type".into(),
                        target: None,
                        candidates: Vec::new(),
                        provenance: "dm-recovery:explicit-parent-type;preprocessor-not-evaluated"
                            .into(),
                    });
                    if let Some(owner) = frames.last().and_then(|frame| frame.definition) {
                        result.relationships.push(Relationship {
                            source: owner,
                            target: None,
                            kind: "inheritance".into(),
                            evidence_start: tokens[eq + 1].start,
                            evidence_end: parent_end,
                            provenance: "dm-recovery:explicit-parent-type".into(),
                        });
                    }
                }
                if parent.starts_with('/') && !conditional {
                    parents.insert(base.to_owned(), parent);
                }
            }
        } else if let Some((path, name_index, path_end, kind)) =
            declaration(tokens, base, procedure.is_some())
        {
            let container = path
                .rsplit_once('/')
                .map(|(parent, _)| parent.to_owned())
                .filter(|s| !s.is_empty());
            let local = (kind == "binding").then_some(procedure).flatten();
            let index = add_definition(
                &mut result,
                &mut definitions,
                &tokens[name_index],
                line.end,
                kind,
                path.clone(),
                container,
                conditional,
                local,
            );
            result.definitions[index].start = tokens[0].start;
            if kind == "binding" {
                bindings.push((index, line.indent, braces));
            }
            if matches!(kind, "proc" | "verb") {
                let open = tokens.iter().filter(|t| t.text == "{").count();
                let close = tokens.iter().filter(|t| t.text == "}").count();
                if open == 0 || open > close {
                    frames.push(DmFrame {
                        path: path.clone(),
                        indent: line.indent,
                        brace: (open > 0).then_some(braces + 1),
                        definition: Some(index),
                        procedure: Some(index),
                    });
                }
                recover_parameters(
                    &mut result,
                    &mut definitions,
                    tokens,
                    path_end,
                    index,
                    conditional,
                );
                let tail = tokens
                    .iter()
                    .position(|t| t.text == ")")
                    .map_or(tokens.len(), |n| n + 1);
                evidence.push(DmEvidence {
                    tokens: tokens[tail..].to_vec(),
                    procedure: Some(index),
                    conditional,
                });
            } else if kind == "type" || kind == "namespace" {
                let open = tokens.iter().filter(|t| t.text == "{").count();
                let close = tokens.iter().filter(|t| t.text == "}").count();
                if open == 0 || open > close {
                    frames.push(DmFrame {
                        path,
                        indent: line.indent,
                        brace: (open > 0).then_some(braces + 1),
                        definition: Some(index),
                        procedure: None,
                    });
                }
            } else {
                evidence.push(DmEvidence {
                    tokens: tokens[path_end..].to_vec(),
                    procedure,
                    conditional,
                });
            }
        } else {
            evidence.push(DmEvidence {
                tokens: line.tokens.clone(),
                procedure,
                conditional,
            });
        }
        for token in tokens {
            if token.text == "{" {
                braces += 1;
            }
            if token.text == "}" {
                braces = braces.saturating_sub(1);
            }
        }
    }
    while !frames.is_empty() {
        close_frame(&mut frames, &mut result, source.len());
    }
    if conditional_depth > 0 {
        result.status = "recovered_incomplete".into();
    }
    dm_resolve::resolve(&mut result, &definitions, evidence, &parents, includes);
    result
}

fn close_frame(frames: &mut Vec<DmFrame>, result: &mut Extraction, end: usize) {
    if let Some(index) = frames.pop().and_then(|frame| frame.definition) {
        result.definitions[index].end = end;
    }
}

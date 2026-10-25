use super::dm_candidates::DmLookup;
use super::{DmDefinition, DmEvidence, Extraction, MAX_FACTS, Occurrence, Relationship};
use std::collections::HashMap;

pub(super) fn resolve(
    result: &mut Extraction,
    definitions: &[DmDefinition],
    evidence: Vec<DmEvidence>,
    parents: &HashMap<String, String>,
    includes: bool,
) {
    let complete = result.status == "recovered";
    let base_count =
        result.definitions.len() + result.occurrences.len() + result.relationships.len();
    let lookup = DmLookup::new(result, definitions);
    let mut fact_limit = false;
    let mut occurrences = Vec::new();
    let mut relationships = Vec::new();
    let mut prior_procedures = HashMap::with_capacity(definitions.len());
    for (index, definition) in definitions.iter().enumerate() {
        if base_count + occurrences.len() + relationships.len() + 2 > MAX_FACTS {
            fact_limit = true;
            break;
        }
        if !matches!(result.definitions[index].kind.as_str(), "proc" | "verb") {
            continue;
        }
        if let Some(previous) = prior_procedures.insert(definition.path.as_str(), index) {
            let target = (complete
                && !includes
                && !definition.conditional
                && !definitions[previous].conditional)
                .then_some(previous);
            occurrences.push(Occurrence {
                name: result.definitions[index].name.clone(),
                start: definition.name_start,
                end: definition.name_end,
                role: "override".into(),
                target,
                candidates: vec![previous],
                provenance: "dm-recovery:same-type-override-order".into(),
            });
            relationships.push(Relationship {
                source: index,
                target,
                kind: "override".into(),
                evidence_start: definition.name_start,
                evidence_end: definition.name_end,
                provenance: "dm-recovery:same-type-override-order".into(),
            });
        }
    }
    'sites: for site in evidence {
        let tokens = &site.tokens;
        for (position, token) in tokens.iter().enumerate() {
            if base_count + occurrences.len() + relationships.len() + 3 > MAX_FACTS {
                fact_limit = true;
                break 'sites;
            }
            let next = tokens.get(position + 1).map(|t| t.text.as_str());
            let previous = position
                .checked_sub(1)
                .and_then(|n| tokens.get(n))
                .map(|t| t.text.as_str());
            if token.text == "."
                && next == Some(".")
                && tokens.get(position + 2).is_some_and(|t| t.text == "(")
            {
                if let Some(owner) = site.procedure {
                    let (candidates, truncated) =
                        lookup.parent_candidates(owner, definitions, result, parents);
                    let target = candidates.last().copied().filter(|&i| {
                        complete
                            && !truncated
                            && !includes
                            && !site.conditional
                            && !definitions[owner].conditional
                            && !definitions[i].conditional
                            && definitions[i].path == definitions[owner].path
                            && i < owner
                            && candidates.iter().all(|&n| !definitions[n].conditional)
                    });
                    let provenance = if target.is_some() {
                        "dm-recovery:same-type-override-order"
                    } else {
                        "dm-recovery:parent;project-order-or-inheritance-unverified"
                    };
                    let provenance = annotate_truncation(provenance, truncated);
                    occurrences.push(Occurrence {
                        name: "..".into(),
                        start: token.start,
                        end: tokens[position + 1].end,
                        role: "parent_call".into(),
                        target,
                        candidates,
                        provenance: provenance.clone(),
                    });
                    relationships.push(Relationship {
                        source: owner,
                        target,
                        kind: "parent_call".into(),
                        evidence_start: token.start,
                        evidence_end: tokens[position + 1].end,
                        provenance,
                    });
                }
                continue;
            }
            if !token.identifier() || keyword(&token.text) {
                continue;
            }
            let (candidates, mut truncated) = lookup.candidates(&token.text);
            let binding =
                if complete && !site.conditional && !matches!(previous, Some("." | ":" | "/")) {
                    let (binding, limited) =
                        lookup.binding(site.procedure, &token.text, token.start, definitions);
                    truncated |= limited;
                    binding
                } else {
                    None
                };
            let call = next == Some("(");
            let signal = matches!(
                token.text.as_str(),
                "SEND_SIGNAL" | "RegisterSignal" | "UnregisterSignal"
            );
            let macro_reference = candidates
                .iter()
                .any(|&i| result.definitions[i].kind == "macro");
            let role = if signal {
                "signal_call"
            } else if call {
                "call"
            } else if macro_reference {
                "macro_reference"
            } else {
                "reference"
            };
            let provenance = if binding.is_some() {
                "dm-recovery:lexical-binding"
            } else if signal {
                "dm-recovery:observed-signal-pattern"
            } else {
                "dm-recovery:name-candidates;preprocessor-not-evaluated"
            };
            let provenance = annotate_truncation(provenance, truncated);
            occurrences.push(Occurrence {
                name: token.text.clone(),
                start: token.start,
                end: token.end,
                role: role.into(),
                target: binding,
                candidates,
                provenance: provenance.clone(),
            });
            if call && let Some(owner) = site.procedure {
                relationships.push(Relationship {
                    source: owner,
                    target: binding,
                    kind: role.into(),
                    evidence_start: token.start,
                    evidence_end: token.end,
                    provenance,
                });
            }
            if signal {
                recover_signal_argument(&mut occurrences, tokens, position);
            }
        }
    }
    result.occurrences.extend(occurrences);
    result.relationships.extend(relationships);
    if fact_limit {
        result.status = "recovered_incomplete".into();
    }
    if result.status != "recovered" {
        for occurrence in &mut result.occurrences {
            occurrence.target = None;
            occurrence.provenance.push_str(";incomplete-extraction");
        }
        for relationship in &mut result.relationships {
            relationship.target = None;
            relationship.provenance.push_str(";incomplete-extraction");
        }
    }
}

fn annotate_truncation(provenance: &str, truncated: bool) -> String {
    if truncated {
        format!("{provenance};candidates-truncated")
    } else {
        provenance.into()
    }
}

fn recover_signal_argument(
    occurrences: &mut Vec<Occurrence>,
    tokens: &[super::DmToken],
    position: usize,
) {
    let mut depth = 0usize;
    let mut argument = 0usize;
    for token in tokens.iter().skip(position + 1).take(super::MAX_CANDIDATES) {
        match token.text.as_str() {
            "(" => depth += 1,
            ")" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            "," if depth == 1 => argument += 1,
            _ if depth == 1 && argument == 1 && (token.quoted || token.identifier()) => {
                occurrences.push(Occurrence {
                    name: token.text.trim_matches('"').into(),
                    start: token.start,
                    end: token.end,
                    role: "signal".into(),
                    target: None,
                    candidates: Vec::new(),
                    provenance: "dm-recovery:observed-signal-argument".into(),
                });
                break;
            }
            _ => {}
        }
    }
}

fn keyword(name: &str) -> bool {
    matches!(
        name,
        "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "switch"
            | "break"
            | "continue"
            | "spawn"
            | "set"
            | "var"
            | "new"
            | "del"
            | "in"
            | "as"
            | "null"
            | "TRUE"
            | "FALSE"
    )
}

use super::{DmDefinition, Extraction, MAX_CANDIDATES};
use std::collections::HashMap;

pub(super) struct DmLookup<'a> {
    names: HashMap<&'a str, Vec<usize>>,
    paths: HashMap<&'a str, Vec<usize>>,
    bindings: HashMap<(usize, &'a str), Vec<usize>>,
}

impl<'a> DmLookup<'a> {
    pub fn new(result: &'a Extraction, definitions: &'a [DmDefinition]) -> Self {
        let mut lookup = Self {
            names: HashMap::with_capacity(definitions.len()),
            paths: HashMap::with_capacity(definitions.len()),
            bindings: HashMap::new(),
        };
        for (index, definition) in definitions.iter().enumerate() {
            let name = result.definitions[index].name.as_str();
            let named = lookup.names.entry(name).or_default();
            // One extra entry records truncation without retaining the whole name bucket.
            if named.len() <= MAX_CANDIDATES {
                named.push(index);
            }
            if matches!(result.definitions[index].kind.as_str(), "proc" | "verb") {
                lookup
                    .paths
                    .entry(&definition.path)
                    .or_default()
                    .push(index);
            }
            if let Some(owner) = definition.local_owner {
                lookup
                    .bindings
                    .entry((owner, name))
                    .or_default()
                    .push(index);
            }
        }
        lookup
    }

    pub fn candidates(&self, name: &str) -> (Vec<usize>, bool) {
        let entries = self.names.get(name).map_or(&[][..], Vec::as_slice);
        (
            entries.iter().take(MAX_CANDIDATES).copied().collect(),
            entries.len() > MAX_CANDIDATES,
        )
    }

    pub fn binding(
        &self,
        owner: Option<usize>,
        name: &str,
        start: usize,
        definitions: &[DmDefinition],
    ) -> (Option<usize>, bool) {
        let Some(entries) = owner.and_then(|owner| self.bindings.get(&(owner, name))) else {
            return (None, false);
        };
        let end = entries.partition_point(|&index| definitions[index].name_start <= start);
        let target = entries[..end]
            .iter()
            .rev()
            .take(MAX_CANDIDATES)
            .copied()
            .find(|&index| start < definitions[index].scope_end);
        // A conditional shadow blocks resolution to an earlier outer binding.
        (
            target.filter(|&index| !definitions[index].conditional),
            end > MAX_CANDIDATES && target.is_none(),
        )
    }

    pub fn parent_candidates(
        &self,
        owner: usize,
        definitions: &[DmDefinition],
        result: &Extraction,
        parents: &HashMap<String, String>,
    ) -> (Vec<usize>, bool) {
        let entries = self
            .paths
            .get(definitions[owner].path.as_str())
            .map_or(&[][..], Vec::as_slice);
        let end = entries.partition_point(|&index| index < owner);
        let mut candidates: Vec<_> = entries[end.saturating_sub(MAX_CANDIDATES)..end].to_vec();
        let mut truncated = end > MAX_CANDIDATES;
        if !candidates.is_empty() {
            return (candidates, truncated);
        }
        let Some(mut ancestor) = result.definitions[owner].container.as_deref() else {
            return (candidates, truncated);
        };
        let mut visited = Vec::with_capacity(8);
        while let Some(parent) = parents.get(ancestor) {
            if visited.contains(&parent.as_str()) {
                break;
            }
            if visited.len() == MAX_CANDIDATES {
                truncated = true;
                break;
            }
            visited.push(parent.as_str());
            let path = format!("{parent}/{}", result.definitions[owner].name);
            if let Some(entries) = self.paths.get(path.as_str()) {
                let remaining = MAX_CANDIDATES - candidates.len();
                candidates.extend(entries.iter().take(remaining).copied());
                truncated |= entries.len() > remaining;
                break;
            }
            ancestor = parent;
        }
        (candidates, truncated)
    }
}

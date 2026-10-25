use std::collections::HashMap;

use tree_sitter::Node;

use super::Extraction;
use super::bindings::{Binding, function_scope};

pub(super) struct BindingIndex {
    bindings: Vec<Binding>,
    names: HashMap<String, Vec<usize>>,
    tokens: HashMap<usize, usize>,
    owners: HashMap<usize, usize>,
}

impl BindingIndex {
    pub fn new(bindings: Vec<Binding>, result: &Extraction) -> Self {
        let mut names = HashMap::with_capacity(bindings.len());
        let mut tokens = HashMap::with_capacity(bindings.len());
        let mut owners = HashMap::new();
        for (index, binding) in bindings.iter().enumerate() {
            let definition = &result.definitions[binding.definition];
            names
                .entry(definition.name.clone())
                .or_insert_with(|| Vec::with_capacity(1))
                .push(index);
            tokens.insert(binding.token.start, index);
            if matches!(definition.kind.as_str(), "function" | "method") {
                owners.insert(definition.start, binding.definition);
            }
        }
        Self {
            bindings,
            names,
            tokens,
            owners,
        }
    }

    pub fn declaration(&self, node: Node<'_>) -> Option<usize> {
        self.tokens
            .get(&node.start_byte())
            .map(|index| &self.bindings[*index])
            .filter(|binding| binding.token == node.byte_range())
            .map(|binding| binding.definition)
    }

    pub fn owner(&self, node: Node<'_>) -> Option<usize> {
        let mut ancestor = node.parent();
        while let Some(parent) = ancestor {
            if let Some(owner) = self.owners.get(&parent.start_byte()) {
                return Some(*owner);
            }
            ancestor = parent.parent();
        }
        None
    }

    pub fn resolve(
        &self,
        name: &str,
        node: Node<'_>,
        namespace: u8,
    ) -> (Option<usize>, Vec<usize>, bool) {
        let position = node.start_byte();
        let function_boundary = function_scope(node).start;
        let Some(indices) = self.names.get(name) else {
            return (None, Vec::new(), false);
        };
        let candidates = indices
            .iter()
            .take(64)
            .map(|index| self.bindings[*index].definition)
            .collect();
        let mut best: Option<&Binding> = None;
        let mut ambiguous = false;
        for binding in indices
            .iter()
            .map(|index| &self.bindings[*index])
            .filter(|binding| {
                binding.resolvable
                    && binding.namespaces & namespace != 0
                    && binding.scope.contains(&position)
                    && binding.visible <= position
                    && binding
                        .function_boundary
                        .is_none_or(|boundary| boundary == function_boundary)
            })
        {
            match best {
                Some(previous)
                    if (previous.scope.len(), std::cmp::Reverse(previous.visible))
                        < (binding.scope.len(), std::cmp::Reverse(binding.visible)) => {}
                Some(previous)
                    if previous.scope == binding.scope && previous.visible == binding.visible =>
                {
                    ambiguous = true
                }
                _ => {
                    best = Some(binding);
                    ambiguous = false;
                }
            }
        }
        let resolved = best
            .filter(|_| !ambiguous)
            .filter(|best| {
                !indices
                    .iter()
                    .map(|index| &self.bindings[*index])
                    .any(|binding| {
                        binding.blocks_before
                            && binding.namespaces & namespace != 0
                            && binding.scope.contains(&position)
                            && binding.visible > position
                            && binding.scope.len() <= best.scope.len()
                    })
            })
            .map(|binding| binding.definition);
        (resolved, candidates, indices.len() > 64)
    }
}

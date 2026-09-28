use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PhpClassKind {
    Class,
    Interface,
    Trait,
    Enum,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PhpClassRole {
    Ordinary,
    Proxy,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PhpVisibility {
    Public,
    Protected,
    Private,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PhpClassFact {
    pub(super) definition_id: i64,
    pub(super) file_id: i64,
    pub(super) file_path: String,
    pub(super) fqcn: String,
    pub(super) canonical_fqcn: Option<String>,
    pub(super) kind: PhpClassKind,
    pub(super) role: PhpClassRole,
    pub(super) complete: bool,
    pub(super) conditional: bool,
    pub(super) has_trait_use: bool,
    pub(super) trait_use_conditional: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PhpMethodFact {
    pub(super) definition_id: i64,
    pub(super) owner_class_id: i64,
    pub(super) file_id: i64,
    pub(super) file_path: String,
    pub(super) name: String,
    pub(super) visibility: PhpVisibility,
    pub(super) abstract_method: bool,
    pub(super) complete: bool,
    pub(super) conditional: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PhpParentKind {
    Extends,
    XfcpProxy,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PhpParentStatus {
    /// One or more candidate edges are available; none is an exact binding.
    Candidates,
    Missing,
    Ambiguous,
    Conditional,
    Incomplete,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PhpParentCandidate {
    pub(super) child_class_id: i64,
    pub(super) declared_name: String,
    pub(super) resolved_name: Option<String>,
    pub(super) canonical_name: Option<String>,
    pub(super) evidence_start: i64,
    pub(super) evidence_end: i64,
    pub(super) kind: PhpParentKind,
    pub(super) candidate_class_ids: Vec<i64>,
    pub(super) status: PhpParentStatus,
    pub(super) conditional: bool,
    pub(super) execute_order: Option<i64>,
    pub(super) tie_group: Option<i64>,
}

#[derive(Default)]
pub(super) struct PhpHierarchyIndex {
    classes: Vec<PhpClassFact>,
    class_ids_by_raw_name: HashMap<String, Vec<usize>>,
    class_ids_by_canonical_name: HashMap<String, Vec<usize>>,
    class_position_by_id: HashMap<i64, usize>,
    methods: Vec<PhpMethodFact>,
    method_positions_by_owner_name: HashMap<(i64, String), Vec<usize>>,
    parents_by_child: HashMap<i64, Vec<PhpParentCandidate>>,
}

impl PhpHierarchyIndex {
    pub(super) fn from_facts(
        classes: Vec<PhpClassFact>,
        methods: Vec<PhpMethodFact>,
        parents: Vec<PhpParentCandidate>,
    ) -> Self {
        let mut index = Self {
            classes,
            methods,
            ..Self::default()
        };

        for (position, class) in index.classes.iter().enumerate() {
            index
                .class_position_by_id
                .insert(class.definition_id, position);
            if class.role == PhpClassRole::Proxy {
                continue;
            }
            if class.role == PhpClassRole::Proxy || is_xfcp_name(&class.fqcn) {
                continue;
            }
            insert_class_position(&mut index.class_ids_by_raw_name, &class.fqcn, position);
            if let Some(canonical_name) = class.canonical_fqcn.as_deref() {
                insert_class_position(
                    &mut index.class_ids_by_canonical_name,
                    canonical_name,
                    position,
                );
            }
        }

        for (position, method) in index.methods.iter().enumerate() {
            let key = (method.owner_class_id, method.name.to_ascii_lowercase());
            index
                .method_positions_by_owner_name
                .entry(key)
                .or_default()
                .push(position);
        }

        for parent in parents {
            index.add_parent_candidate(parent);
        }
        index
    }

    pub(super) fn class_candidates(
        &self,
        raw_or_canonical: &str,
    ) -> impl Iterator<Item = &PhpClassFact> {
        let positions = normalized_class_name(raw_or_canonical).and_then(|key| {
            self.class_ids_by_raw_name
                .get(&key)
                .or_else(|| self.class_ids_by_canonical_name.get(&key))
        });
        positions
            .into_iter()
            .flatten()
            .filter_map(|position| self.classes.get(*position))
    }

    pub(super) fn raw_class_candidates(&self, name: &str) -> impl Iterator<Item = &PhpClassFact> {
        let positions =
            normalized_class_name(name).and_then(|key| self.class_ids_by_raw_name.get(&key));
        positions
            .into_iter()
            .flatten()
            .filter_map(|position| self.classes.get(*position))
    }

    pub(super) fn canonical_class_candidates(
        &self,
        name: &str,
    ) -> impl Iterator<Item = &PhpClassFact> {
        let positions =
            normalized_class_name(name).and_then(|key| self.class_ids_by_canonical_name.get(&key));
        positions
            .into_iter()
            .flatten()
            .filter_map(|position| self.classes.get(*position))
    }

    pub(super) fn class_fact(&self, definition_id: i64) -> Option<&PhpClassFact> {
        self.class_position_by_id
            .get(&definition_id)
            .and_then(|position| self.classes.get(*position))
    }

    pub(super) fn direct_methods(
        &self,
        owner_class_id: i64,
        name: &str,
    ) -> impl Iterator<Item = &PhpMethodFact> {
        let key = (owner_class_id, name.to_ascii_lowercase());
        let positions = self.method_positions_by_owner_name.get(&key);
        positions
            .into_iter()
            .flatten()
            .filter_map(|position| self.methods.get(*position))
    }

    pub(super) fn parent_candidates(&self, child_class_id: i64) -> &[PhpParentCandidate] {
        self.parents_by_child
            .get(&child_class_id)
            .map_or(&[], Vec::as_slice)
    }

    pub(super) fn classes(&self) -> &[PhpClassFact] {
        &self.classes
    }

    pub(super) fn add_parent_candidate(&mut self, candidate: PhpParentCandidate) {
        self.parents_by_child
            .entry(candidate.child_class_id)
            .or_default()
            .push(candidate);
    }

    pub(super) fn replace_proxy_parent_candidates(
        &mut self,
        child_class_id: i64,
        evidence_start: i64,
        evidence_end: i64,
        replacements: Vec<PhpParentCandidate>,
    ) {
        let candidates = self.parents_by_child.entry(child_class_id).or_default();
        candidates.retain(|candidate| {
            candidate.kind != PhpParentKind::XfcpProxy
                || candidate.evidence_start != evidence_start
                || candidate.evidence_end != evidence_end
        });
        candidates.extend(replacements);
    }
}

fn normalized_class_name(name: &str) -> Option<String> {
    let name = name.trim().trim_start_matches('\\');
    if name.is_empty() {
        return None;
    }
    Some(name.to_ascii_lowercase())
}

fn insert_class_position(
    positions_by_name: &mut HashMap<String, Vec<usize>>,
    name: &str,
    position: usize,
) {
    let Some(key) = normalized_class_name(name) else {
        return;
    };
    let positions = positions_by_name.entry(key).or_default();
    if !positions.contains(&position) {
        positions.push(position);
    }
}

fn is_xfcp_name(name: &str) -> bool {
    name.split('\\').any(|part| {
        part.get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("XFCP_"))
    })
}

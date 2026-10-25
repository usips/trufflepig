use super::{ClassExtension, ExtensionFile};
use crate::store::php_resolver::hierarchy_index::{PhpClassKind, PhpClassRole, PhpParentCandidate};
use std::ops::Range;

mod chain_facts;
mod proxy_validation;
mod registration_lookup;

pub(super) use registration_lookup::prepare;

/// Prepared candidate edges and exact source spans for XenForo chain projection.
pub(super) struct PreparedChainProjection {
    pub(super) parent_edges: Vec<PreparedParentEdge>,
    pub(super) placeholder_updates: Vec<PreparedPlaceholderUpdate>,
    pub(super) class_occurrence_updates: Vec<PreparedClassOccurrenceUpdate>,
    pub(super) hierarchy_edges: Vec<PhpParentCandidate>,
    pub(super) issues: Vec<PreparedChainIssue>,
}

pub(super) struct PreparedParentEdge {
    pub(super) child_class_id: i64,
    pub(super) predecessor_class_id: i64,
    pub(super) file_id: i64,
    pub(super) span: Range<usize>,
    pub(super) provenance: String,
}

pub(super) struct PreparedPlaceholderUpdate {
    pub(super) occurrence_id: i64,
    pub(super) file_id: i64,
    pub(super) span: Range<usize>,
    pub(super) target_class_ids: Vec<i64>,
    pub(super) provenance: String,
}

pub(super) struct PreparedClassOccurrenceUpdate {
    pub(super) file_id: i64,
    pub(super) span: Range<usize>,
    pub(super) role: &'static str,
    pub(super) name: String,
    pub(super) class_ids: Vec<i64>,
    pub(super) tag_span: Range<usize>,
    pub(super) provenance: String,
}

pub(super) struct PreparedChainIssue {
    pub(super) child_class_id: Option<i64>,
    pub(super) extension_definition_id: Option<i64>,
    pub(super) file_id: i64,
    pub(super) span: Range<usize>,
    pub(super) provenance: String,
}

struct PendingRow<'a> {
    file: &'a ExtensionFile,
    row_index: usize,
    extension: &'a ClassExtension,
    base: Option<String>,
    implementation: Option<String>,
    expected_proxy: String,
    metadata_issues: Vec<&'static str>,
}

#[derive(Clone)]
struct RegistrationEvidence {
    file_id: i64,
    row_index: usize,
    tag_span: Range<usize>,
    base_name: String,
    implementation_name: String,
    implementation_raw_lookup: String,
    implementation_lookup: String,
    expected_proxy: String,
    priority: i64,
}

#[derive(Clone)]
struct ImplementationLookup {
    classes: Vec<ImplementationClass>,
    truncated: bool,
}

#[derive(Clone)]
struct ImplementationClass {
    definition_id: i64,
    file_id: i64,
    kind: PhpClassKind,
    role: PhpClassRole,
    complete: bool,
    conditional: bool,
    proxy_parent: Option<PhpParentCandidate>,
    proxy_occurrence: Option<(i64, Range<usize>)>,
    proxy_matches: bool,
}

#[derive(Clone, Copy)]
struct ClassTarget {
    definition_id: i64,
    kind: PhpClassKind,
    role: PhpClassRole,
    complete: bool,
    conditional: bool,
}

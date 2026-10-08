use super::path_prior::PathPrior;
use crate::results::Hit;
use std::collections::BTreeMap;

/// Bounds the number of files contributed by one retrieval lane.
pub(crate) const FILE_CANDIDATE_LIMIT: usize = 1_000;
const RRF_K: f64 = 60.0;

/// Fusion-time weights: per-lane provenance weights, the identifier evidence
/// tier and the [path prior](super::path_prior).
#[derive(Clone, Copy, Debug)]
pub(crate) struct FusionPolicy<'p> {
    /// Weighs hits by lane provenance; a re-fusion of already fused lanes leaves it off.
    pub lane_weights: bool,
    /// Ranks files by [`IdentifierEvidence`] before score.
    pub identifier_tier: bool,
    pub prior: &'p PathPrior,
    /// Lanes before this index already carry the prior (a re-fused ranking).
    pub prior_from_lane: usize,
}

/// What a hit proves about an identifier query's token in its file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum IdentifierEvidence {
    #[default]
    Absent,
    /// A use, local binding, or import of the token.
    Used,
    /// A declaration of the token.
    Defined,
    /// A file stem equal to the token (`tick_rate.rs` for `tick_rate`).
    Named,
}

/// The strongest rerank evidence carried by an actual lane hit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FileEvidence {
    #[default]
    Absent,
    Used,
    Defined,
    Named,
    /// The query terms appear together in order in the file.
    Phrase,
}

impl FileEvidence {
    /// Classifies the strongest rerank evidence carried by one actual lane hit.
    pub(crate) fn of(hit: &Hit, policy: &FusionPolicy) -> Self {
        match hit.provenance.as_deref() {
            _ if policy.identifier_tier && policy.prior.names_exactly(&hit.path) => Self::Named,
            Some("phrase") => Self::Phrase,
            _ if !policy.identifier_tier => Self::Absent,
            Some("exact_identifier") if !is_local_kind(&hit.kind) => Self::Defined,
            Some("exact_identifier" | "identifier_occurrence") => Self::Used,
            _ => Self::Absent,
        }
    }

    /// Identifier-only evidence retains its finer fusion rank; phrases do not alter it.
    pub(crate) const fn identifier_evidence(self) -> IdentifierEvidence {
        match self {
            Self::Used => IdentifierEvidence::Used,
            Self::Defined => IdentifierEvidence::Defined,
            Self::Named => IdentifierEvidence::Named,
            Self::Absent | Self::Phrase => IdentifierEvidence::Absent,
        }
    }

    /// Tiers shared by representative selection and reranker ordering.
    pub(crate) const fn tier(self) -> u8 {
        match self {
            Self::Named | Self::Defined | Self::Phrase => 0,
            Self::Used => 1,
            Self::Absent => 2,
        }
    }
}

/// Bindings that only use a name: locals, parameters and imports.
fn is_local_kind(kind: &str) -> bool {
    matches!(kind, "variable" | "parameter" | "import")
}

/// Fuses ranked hit lanes after collapsing each lane to one hit per file.
///
/// Each lane contributes its first hit per path, then is capped at 1,000
/// distinct paths before weighted reciprocal-rank fusion. A file keeps the
/// hit with strongest rerank evidence; lane and rank break evidence ties.
/// Equal file scores sort by encoded source path.
pub(crate) fn fuse_search_file_lanes<I, L>(lanes: I, policy: &FusionPolicy) -> Vec<Hit>
where
    I: IntoIterator<Item = L>,
    L: AsRef<[Hit]>,
{
    let mut files: BTreeMap<String, FileScore> = BTreeMap::new();
    for (lane_index, lane) in lanes.into_iter().enumerate() {
        let lane = lane.as_ref();
        let mut representatives: BTreeMap<&str, (usize, &Hit)> = BTreeMap::new();
        for (rank, hit) in lane.iter().enumerate() {
            representatives.entry(&hit.path).or_insert((rank, hit));
        }

        let mut representatives: Vec<_> = representatives.into_values().collect();
        representatives.sort_by(|(left_rank, left), (right_rank, right)| {
            left_rank
                .cmp(right_rank)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.start.cmp(&right.start))
                .then_with(|| left.end.cmp(&right.end))
        });
        representatives.truncate(FILE_CANDIDATE_LIMIT);

        for (file_rank, (_, hit)) in representatives.into_iter().enumerate() {
            let evidence = FileEvidence::of(hit, policy);
            let score = files.entry(hit.path.clone()).or_insert_with(|| FileScore {
                score: 0.0,
                identifier_evidence: IdentifierEvidence::Absent,
                representative_evidence: evidence,
                representative: hit.clone(),
                representative_lane: lane_index,
                representative_rank: file_rank,
            });
            let weight = if policy.lane_weights {
                lane_weight(hit, policy)
            } else {
                1.0
            };
            let prior = if lane_index >= policy.prior_from_lane {
                policy.prior.weight(&hit.path)
            } else {
                1.0
            };
            score.score += weight * prior / (RRF_K + file_rank as f64 + 1.0);
            score.identifier_evidence = score
                .identifier_evidence
                .max(evidence.identifier_evidence());
            let better_evidence = evidence.tier() < score.representative_evidence.tier();
            let tied_evidence = evidence.tier() == score.representative_evidence.tier();
            let earlier_hit =
                (lane_index, file_rank) < (score.representative_lane, score.representative_rank);
            if better_evidence || (tied_evidence && earlier_hit) {
                score.representative_evidence = evidence;
                score.representative = hit.clone();
                score.representative_lane = lane_index;
                score.representative_rank = file_rank;
            }
        }
    }

    let mut files: Vec<_> = files.into_values().collect();
    files.sort_by(|left, right| {
        right
            .identifier_evidence
            .cmp(&left.identifier_evidence)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.representative.path.cmp(&right.representative.path))
            .then_with(|| left.representative.start.cmp(&right.representative.start))
            .then_with(|| left.representative.end.cmp(&right.representative.end))
            .then_with(|| left.representative.kind.cmp(&right.representative.kind))
    });
    files.into_iter().map(|file| file.representative).collect()
}

/// Lane weights: an identifier query's exact declarations ×3, phrase matches
/// and exact filenames ×2, partial filenames ×0.5, every other lane ×1.
fn lane_weight(hit: &Hit, policy: &FusionPolicy) -> f64 {
    match hit.provenance.as_deref() {
        Some("exact_identifier") if policy.identifier_tier && !is_local_kind(&hit.kind) => 3.0,
        Some("phrase" | "filename_exact") => 2.0,
        Some("filename_partial") => 0.5,
        _ => 1.0,
    }
}

struct FileScore {
    score: f64,
    identifier_evidence: IdentifierEvidence,
    representative_evidence: FileEvidence,
    representative: Hit,
    representative_lane: usize,
    representative_rank: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Query;

    const NEUTRAL: &PathPrior = &PathPrior::neutral();
    const UNWEIGHTED: FusionPolicy = FusionPolicy {
        lane_weights: false,
        identifier_tier: false,
        prior: NEUTRAL,
        prior_from_lane: 0,
    };
    const WEIGHTED: FusionPolicy = FusionPolicy {
        lane_weights: true,
        identifier_tier: false,
        prior: NEUTRAL,
        prior_from_lane: 0,
    };

    fn hit(path: &str, start: usize, provenance: &str) -> Hit {
        Hit {
            handle: String::new(),
            path: path.into(),
            revision: None,
            start,
            end: start + 1,
            start_line: 1,
            end_line: 1,
            name: path.into(),
            kind: "region".into(),
            container: None,
            provenance: Some(provenance.into()),
            resolution: None,
            candidates: Vec::new(),
            target: None,
            repeats: None,
            snippet: None,
            differs: false,
        }
    }

    #[test]
    fn exact_basename_survives_a_strong_partial_name_distractor() {
        let mut lexical = Vec::with_capacity(82);
        lexical.push(hit("assets/programmable_control.ron", 0, "lexical"));
        for index in 0..80 {
            lexical.push(hit(&format!("other/{index}.rs"), 0, "lexical"));
        }
        lexical.push(hit("content/control.luau", 0, "lexical"));
        let filenames = vec![
            hit("content/control.luau", 0, "filename_exact"),
            hit("assets/programmable_control.ron", 0, "filename_tokens"),
        ];
        let fused = fuse_search_file_lanes([lexical, filenames], &WEIGHTED);
        assert_eq!(fused[0].path, "content/control.luau");
        assert_eq!(fused[0].provenance.as_deref(), Some("lexical"));
    }

    #[test]
    fn duplicate_regions_cannot_crowd_another_file() {
        let mut duplicate = (0..FILE_CANDIDATE_LIMIT + 1)
            .map(|start| hit("a.rs", start, "lexical"))
            .collect::<Vec<_>>();
        duplicate.push(hit("b.rs", 7, "lexical"));
        let fused = fuse_search_file_lanes([duplicate.as_slice()], &UNWEIGHTED);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].path, "a.rs");
        assert_eq!(fused[1].path, "b.rs");
    }

    #[test]
    fn path_tie_is_stable_and_first_lane_is_representative() {
        let first = [hit("a.rs", 4, "exact")];
        let second = [hit("a.rs", 2, "lexical"), hit("b.rs", 0, "lexical")];
        let fused = fuse_search_file_lanes([first.as_slice(), second.as_slice()], &UNWEIGHTED);
        assert_eq!(
            fused
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            ["a.rs", "b.rs"]
        );
        assert_eq!(fused[0].provenance.as_deref(), Some("exact"));
    }

    #[test]
    fn path_prior_demotes_test_and_doc_files_without_dropping_them() {
        let lexical = [
            hit("src/sim/tests.rs", 0, "lexical"),
            hit("AGENTS.md", 0, "lexical"),
            hit("src/sim/item_slots.rs", 0, "lexical"),
        ];
        let prior = PathPrior::for_query(&Query::parse("item slot").unwrap());
        let policy = FusionPolicy {
            prior: &prior,
            ..WEIGHTED
        };
        let fused = fuse_search_file_lanes([lexical.as_slice()], &policy);
        let paths: Vec<_> = fused.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(
            paths,
            ["src/sim/item_slots.rs", "src/sim/tests.rs", "AGENTS.md"]
        );
        // Re-fusion demotes only the new semantic lane: the fused ranks already carry it.
        let semantic = [hit("src/sim/tests.rs", 0, "semantic")];
        let refused = fuse_search_file_lanes(
            [fused.as_slice(), semantic.as_slice()],
            &FusionPolicy {
                lane_weights: false,
                prior_from_lane: 1,
                ..policy
            },
        );
        // tests.rs: 1/62 + 0.7/61 beats item_slots.rs: 1/61.
        assert_eq!(refused[0].path, "src/sim/tests.rs");
    }

    #[test]
    fn a_file_named_by_an_identifier_query_is_identifier_evidence() {
        let prior = PathPrior::for_query(&Query::parse("tick_rate").unwrap());
        let policy = FusionPolicy {
            identifier_tier: true,
            prior: &prior,
            ..WEIGHTED
        };
        let occurrences: Vec<_> = (0..200)
            .map(|index| {
                hit(
                    &format!("src/user_{index:03}.rs"),
                    0,
                    "identifier_occurrence",
                )
            })
            .collect();
        let lexical = [hit("src/other.rs", 0, "lexical")];
        let filenames = [hit("crates/core/src/tick_rate.rs", 0, "filename_exact")];
        let fused = fuse_search_file_lanes(
            [
                occurrences.as_slice(),
                lexical.as_slice(),
                filenames.as_slice(),
            ],
            &policy,
        );
        assert_eq!(fused[0].path, "crates/core/src/tick_rate.rs");
        assert_eq!(fused.last().unwrap().path, "src/other.rs");
    }

    #[test]
    fn identifier_evidence_outranks_files_lacking_the_token() {
        let exact = [hit("src/defs.rs", 0, "exact_identifier")];
        let occurrences = [hit("src/uses.rs", 0, "identifier_occurrence")];
        let lexical = [
            hit("src/other.rs", 0, "lexical"),
            hit("src/defs.rs", 0, "lexical"),
        ];
        let filenames = [hit("src/other.rs", 0, "filename_exact")];
        let semantic = [hit("src/other.rs", 0, "semantic")];
        let policy = FusionPolicy {
            identifier_tier: true,
            ..WEIGHTED
        };
        let fused = fuse_search_file_lanes(
            [
                exact.as_slice(),
                occurrences.as_slice(),
                lexical.as_slice(),
                filenames.as_slice(),
            ],
            &policy,
        );
        let paths: Vec<_> = fused.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(paths, ["src/defs.rs", "src/uses.rs", "src/other.rs"]);
        // Re-fusing with semantic hits keeps the tier through representative provenance.
        let refused = fuse_search_file_lanes(
            [fused, semantic.to_vec()],
            &FusionPolicy {
                lane_weights: false,
                ..policy
            },
        );
        assert_eq!(refused[2].path, "src/other.rs");
        // Without the identifier tier, the same lanes let the filename match lead.
        let untiered = fuse_search_file_lanes(
            [exact.as_slice(), lexical.as_slice(), filenames.as_slice()],
            &WEIGHTED,
        );
        assert_eq!(untiered[0].path, "src/other.rs");
    }

    #[test]
    fn phrase_lane_outweighs_an_or_lane_rank() {
        let phrase = [hit("b.rs", 0, "phrase")];
        let lexical = [hit("a.rs", 0, "lexical"), hit("b.rs", 0, "lexical")];
        let fused = fuse_search_file_lanes([phrase.as_slice(), lexical.as_slice()], &WEIGHTED);
        assert_eq!(fused[0].path, "b.rs");
        assert_eq!(fused[0].provenance.as_deref(), Some("phrase"));
    }
}

use crate::results::Hit;
use std::collections::BTreeMap;

/// Bounds the number of files contributed by one retrieval lane.
pub(crate) const FILE_CANDIDATE_LIMIT: usize = 1_000;
const RRF_K: f64 = 60.0;

/// Fuses ranked hit lanes after collapsing each lane to one hit per file.
///
/// A lane's first hit for a path is its strongest representative. The lane is
/// then capped at 1,000 distinct paths before reciprocal-rank fusion. Results
/// use the first lane's representative on a path tie and sort equal scores by
/// their encoded source path.
pub(crate) fn fuse_file_lanes<I, L>(lanes: I) -> Vec<Hit>
where
    I: IntoIterator<Item = L>,
    L: AsRef<[Hit]>,
{
    fuse(lanes, false)
}

pub(crate) fn fuse_search_file_lanes<I, L>(lanes: I) -> Vec<Hit>
where
    I: IntoIterator<Item = L>,
    L: AsRef<[Hit]>,
{
    fuse(lanes, true)
}

fn fuse<I, L>(lanes: I, filename_weights: bool) -> Vec<Hit>
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
            let score = files.entry(hit.path.clone()).or_insert_with(|| FileScore {
                score: 0.0,
                representative: hit.clone(),
                representative_lane: lane_index,
                representative_rank: file_rank,
            });
            let weight = if filename_weights {
                match hit.provenance.as_deref() {
                    Some("filename_exact") => 2.0,
                    Some("filename_partial") => 0.5,
                    _ => 1.0,
                }
            } else {
                1.0
            };
            score.score += weight / (RRF_K + file_rank as f64 + 1.0);
            if (lane_index, file_rank) < (score.representative_lane, score.representative_rank) {
                score.representative = hit.clone();
                score.representative_lane = lane_index;
                score.representative_rank = file_rank;
            }
        }
    }

    let mut files: Vec<_> = files.into_values().collect();
    files.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.representative.path.cmp(&right.representative.path))
            .then_with(|| left.representative.start.cmp(&right.representative.start))
            .then_with(|| left.representative.end.cmp(&right.representative.end))
            .then_with(|| left.representative.kind.cmp(&right.representative.kind))
    });
    files.into_iter().map(|file| file.representative).collect()
}

struct FileScore {
    score: f64,
    representative: Hit,
    representative_lane: usize,
    representative_rank: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

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
            snippet: None,
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
        let fused = fuse_search_file_lanes([lexical, filenames]);
        assert_eq!(fused[0].path, "content/control.luau");
        assert_eq!(fused[0].provenance.as_deref(), Some("lexical"));
    }

    #[test]
    fn duplicate_regions_cannot_crowd_another_file() {
        let mut duplicate = (0..FILE_CANDIDATE_LIMIT + 1)
            .map(|start| hit("a.rs", start, "lexical"))
            .collect::<Vec<_>>();
        duplicate.push(hit("b.rs", 7, "lexical"));
        let fused = fuse_file_lanes([duplicate.as_slice()]);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].path, "a.rs");
        assert_eq!(fused[1].path, "b.rs");
    }

    #[test]
    fn path_tie_is_stable_and_first_lane_is_representative() {
        let first = [hit("a.rs", 4, "exact")];
        let second = [hit("a.rs", 2, "lexical"), hit("b.rs", 0, "lexical")];
        let fused = fuse_file_lanes([first.as_slice(), second.as_slice()]);
        assert_eq!(
            fused
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            ["a.rs", "b.rs"]
        );
        assert_eq!(fused[0].provenance.as_deref(), Some("exact"));
    }
}

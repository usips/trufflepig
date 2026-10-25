use super::AdmissionTier;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tp_core::request_pool::RequestLane;

type RootWorkKey = (PathBuf, RequestLane);
type RootWorkCounts = Arc<Mutex<HashMap<RootWorkKey, usize>>>;

#[derive(Clone, Default)]
pub(super) struct RootWorkLimits {
    counts: RootWorkCounts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RootLimitRefusal {
    pub outstanding: usize,
    pub limit: usize,
}

/// Keeps each protected root reservation until its queued or active job drops.
pub(super) struct RootWorkPermit {
    counts: Option<RootWorkCounts>,
    keys: Vec<RootWorkKey>,
}

impl RootWorkLimits {
    pub(super) fn try_acquire(
        &self,
        tier: AdmissionTier,
        lane: RequestLane,
        roots: &[PathBuf],
    ) -> Result<RootWorkPermit, RootLimitRefusal> {
        let Some(limit) = outstanding_limit(tier, lane) else {
            return Ok(RootWorkPermit {
                counts: None,
                keys: Vec::new(),
            });
        };
        let mut roots = roots.to_vec();
        roots.sort_unstable();
        roots.dedup();
        if roots.is_empty() {
            return Err(RootLimitRefusal {
                outstanding: 0,
                limit,
            });
        }

        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for root in &roots {
            let outstanding = counts
                .get(&(root.clone(), lane))
                .copied()
                .unwrap_or_default();
            if outstanding >= limit {
                return Err(RootLimitRefusal { outstanding, limit });
            }
        }
        let keys: Vec<_> = roots.into_iter().map(|root| (root, lane)).collect();
        for key in &keys {
            *counts.entry(key.clone()).or_default() += 1;
        }
        drop(counts);

        Ok(RootWorkPermit {
            counts: Some(Arc::clone(&self.counts)),
            keys,
        })
    }

    #[cfg(test)]
    fn outstanding(&self, root: &std::path::Path, lane: RequestLane) -> usize {
        self.counts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&(root.to_path_buf(), lane))
            .copied()
            .unwrap_or_default()
    }
}

impl Drop for RootWorkPermit {
    fn drop(&mut self) {
        let Some(counts) = &self.counts else {
            return;
        };
        let mut counts = counts.lock().unwrap_or_else(|poison| poison.into_inner());
        for key in &self.keys {
            let remove = counts.get_mut(key).is_some_and(|count| {
                *count = count.saturating_sub(1);
                *count == 0
            });
            if remove {
                counts.remove(key);
            }
        }
    }
}

fn outstanding_limit(tier: AdmissionTier, lane: RequestLane) -> Option<usize> {
    if matches!(tier, AdmissionTier::Root) {
        return None;
    }
    match lane {
        RequestLane::Maintenance => Some(2),
        RequestLane::Regex => Some(3),
        RequestLane::Semantic => Some(2),
        RequestLane::Query | RequestLane::Board => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn root(name: &str) -> PathBuf {
        PathBuf::from(format!("/repos/{name}"))
    }

    #[test]
    fn overlapping_workspace_reservations_refuse_atomically() {
        let limits = RootWorkLimits::default();
        let b = root("b");
        let _held: Vec<_> = (0..3)
            .map(|_| {
                limits
                    .try_acquire(
                        AdmissionTier::Router,
                        RequestLane::Regex,
                        std::slice::from_ref(&b),
                    )
                    .unwrap()
            })
            .collect();

        let workspace = [root("a"), b.clone(), root("c"), root("a")];
        let Err(refusal) =
            limits.try_acquire(AdmissionTier::Router, RequestLane::Regex, &workspace)
        else {
            panic!("overlapping reservation bypassed a full member limit");
        };
        assert_eq!(
            refusal,
            RootLimitRefusal {
                outstanding: 3,
                limit: 3
            }
        );
        assert_eq!(limits.outstanding(&root("a"), RequestLane::Regex), 0);
        assert_eq!(limits.outstanding(&root("c"), RequestLane::Regex), 0);

        let _disjoint = limits
            .try_acquire(
                AdmissionTier::Router,
                RequestLane::Regex,
                std::slice::from_ref(&root("disjoint")),
            )
            .unwrap();
    }

    #[test]
    fn dropped_and_unwound_work_releases_root_reservations() {
        let limits = RootWorkLimits::default();
        let a = root("a");
        let permit = limits
            .try_acquire(
                AdmissionTier::Coordinator,
                RequestLane::Maintenance,
                &[a.clone()],
            )
            .unwrap();
        assert_eq!(limits.outstanding(&a, RequestLane::Maintenance), 1);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _permit = permit;
            panic!("simulate handler panic");
        }));
        assert_eq!(limits.outstanding(&a, RequestLane::Maintenance), 0);
        assert!(
            limits
                .try_acquire(AdmissionTier::Coordinator, RequestLane::Maintenance, &[a])
                .is_ok()
        );
    }

    #[test]
    fn outstanding_limits_match_the_runtime_contract() {
        let cases: [(AdmissionTier, RequestLane, Option<usize>); 15] = [
            (AdmissionTier::Router, RequestLane::Maintenance, Some(2)),
            (AdmissionTier::Router, RequestLane::Regex, Some(3)),
            (AdmissionTier::Router, RequestLane::Semantic, Some(2)),
            (AdmissionTier::Router, RequestLane::Query, None),
            (AdmissionTier::Router, RequestLane::Board, None),
            (
                AdmissionTier::Coordinator,
                RequestLane::Maintenance,
                Some(2),
            ),
            (AdmissionTier::Coordinator, RequestLane::Regex, Some(3)),
            (AdmissionTier::Coordinator, RequestLane::Semantic, Some(2)),
            (AdmissionTier::Coordinator, RequestLane::Query, None),
            (AdmissionTier::Coordinator, RequestLane::Board, None),
            (AdmissionTier::Root, RequestLane::Maintenance, None),
            (AdmissionTier::Root, RequestLane::Regex, None),
            (AdmissionTier::Root, RequestLane::Semantic, None),
            (AdmissionTier::Root, RequestLane::Query, None),
            (AdmissionTier::Root, RequestLane::Board, None),
        ];
        for (tier, lane, expected) in cases {
            assert_eq!(
                outstanding_limit(tier, lane),
                expected,
                "outstanding_limit({tier:?}, {lane:?})"
            );
        }
    }

    #[test]
    fn root_lane_widths_remain_the_root_tier_limit() {
        let limits = RootWorkLimits::default();
        let a = root("a");
        let held: Vec<_> = (0..4)
            .map(|_| {
                limits
                    .try_acquire(
                        AdmissionTier::Root,
                        RequestLane::Regex,
                        std::slice::from_ref(&a),
                    )
                    .unwrap()
            })
            .collect();
        assert_eq!(held.len(), 4);
    }
}

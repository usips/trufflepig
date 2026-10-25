use super::AdmissionTier;
use super::root_work_limits::{RootLimitRefusal, RootWorkLimits, RootWorkPermit};
use super::semantic_fallback::SemanticAdmissionFallback;
use crate::cli::{ClassifiedDaemonRequest, classify_daemon_request};
use anyhow::{Result, anyhow, ensure};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tp_core::request_pool::{PoolSize, RequestClass, RequestLane, RequestPool};

const FRAME_INGRESS_WORKERS: usize = 2;
const ROUTER_BOARD_LANE: PoolSize = PoolSize {
    workers: 8,
    queue: 32,
};
// Parked inbox waiters hold board workers; at most half the lane may wait so
// writes, reads, and feedback always find a free worker.
const _: () =
    assert!(2 * tp_board::board_backend::BOARD_INBOX_WAITER_LIMIT <= ROUTER_BOARD_LANE.workers);
/// Bounds socket frame reads separately from request execution and routes work
/// to tier-sized query, maintenance, regex, semantic, board, and control queues.
pub(in crate::daemon) struct RequestAdmission {
    tier: AdmissionTier,
    accepted_budget: Duration,
    ingress: Arc<RequestPool>,
    query: Arc<RequestPool>,
    maintenance: Arc<RequestPool>,
    regex: Arc<RequestPool>,
    semantic: Arc<RequestPool>,
    board: Arc<RequestPool>,
    control: Arc<RequestPool>,
    root_work_limits: RootWorkLimits,
}

pub(super) struct AdmittedRequest {
    pub class: RequestClass,
    pub args: Vec<String>,
    pub permit: RootWorkPermit,
    pub fallback: Option<SemanticAdmissionFallback>,
}

pub(super) enum AdmissionDecision {
    Admitted(AdmittedRequest),
    RootLimit(RootLimitRefusal),
}

impl RequestAdmission {
    pub(super) fn new(
        name: &str,
        tier: AdmissionTier,
        query_size: PoolSize,
    ) -> std::io::Result<Self> {
        Ok(Self {
            tier,
            accepted_budget: tier.accepted_budget(),
            ingress: Arc::new(RequestPool::new(
                &format!("{name}-ingress"),
                PoolSize {
                    workers: FRAME_INGRESS_WORKERS,
                    queue: tier.pending_frame_capacity(),
                },
            )?),
            query: Arc::new(RequestPool::new(&format!("{name}-query"), query_size)?),
            maintenance: Arc::new(RequestPool::new(
                &format!("{name}-maintenance"),
                lane_size(tier, RequestLane::Maintenance),
            )?),
            regex: Arc::new(RequestPool::new(
                &format!("{name}-regex"),
                lane_size(tier, RequestLane::Regex),
            )?),
            semantic: Arc::new(RequestPool::new(
                &format!("{name}-semantic"),
                lane_size(tier, RequestLane::Semantic),
            )?),
            board: Arc::new(RequestPool::new(
                &format!("{name}-board"),
                lane_size(tier, RequestLane::Board),
            )?),
            control: Arc::new(RequestPool::new(
                &format!("{name}-control"),
                lane_size(tier, RequestLane::Control),
            )?),
            root_work_limits: RootWorkLimits::default(),
        })
    }

    pub(super) fn accepted_budget(&self) -> Duration {
        self.accepted_budget
    }

    pub(super) fn pending_frame_capacity(&self) -> usize {
        self.tier.pending_frame_capacity()
    }

    pub(super) fn ingress(&self) -> &Arc<RequestPool> {
        &self.ingress
    }

    pub(super) fn execution(&self, class: RequestClass) -> (&str, &Arc<RequestPool>) {
        match class.lane {
            RequestLane::Query => (self.query.name(), &self.query),
            RequestLane::Maintenance => (self.maintenance.name(), &self.maintenance),
            RequestLane::Regex => (self.regex.name(), &self.regex),
            RequestLane::Semantic => (self.semantic.name(), &self.semantic),
            RequestLane::Board => (self.board.name(), &self.board),
            RequestLane::Control => (self.control.name(), &self.control),
        }
    }

    pub(super) fn reserve_roots(
        &self,
        class: RequestClass,
        roots: &[PathBuf],
    ) -> Result<RootWorkPermit, RootLimitRefusal> {
        self.root_work_limits
            .try_acquire(self.tier, class.lane, roots)
    }

    pub(super) fn admit(
        &self,
        classified: ClassifiedDaemonRequest,
        args: Vec<String>,
    ) -> Result<AdmissionDecision> {
        let class = classified.class;
        match self.reserve_roots(class, &classified.roots) {
            Ok(permit) => Ok(AdmissionDecision::Admitted(AdmittedRequest {
                class,
                args,
                permit,
                fallback: None,
            })),
            Err(limit)
                if class.lane == RequestLane::Semantic
                    && class.verb == "search"
                    && limit.limit > 0
                    && limit.outstanding >= limit.limit =>
            {
                let (fallback, fallback_args) =
                    SemanticAdmissionFallback::from_request(&args, limit.outstanding, limit.limit)?;
                let lexical = classify_daemon_request(&fallback_args)?;
                ensure!(
                    lexical.class.lane == RequestLane::Query
                        && lexical.class.verb == "search"
                        && lexical.roots.is_empty(),
                    "semantic_fallback_classification_invalid"
                );
                let permit =
                    self.reserve_roots(lexical.class, &lexical.roots)
                        .map_err(|refusal| {
                            anyhow!(coded!(
                                SemanticFallbackRootReservationFailed,
                                "outstanding={} limit={}",
                                refusal.outstanding,
                                refusal.limit
                            ))
                        })?;
                Ok(AdmissionDecision::Admitted(AdmittedRequest {
                    class: lexical.class,
                    args: fallback_args,
                    permit,
                    fallback: Some(fallback),
                }))
            }
            Err(limit) => Ok(AdmissionDecision::RootLimit(limit)),
        }
    }

    /// Drains every lane against one deadline, so stop never multiplies its
    /// maximum wait by the number of pools.
    pub(super) fn drain(&self, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        for pool in [
            &self.ingress,
            &self.query,
            &self.maintenance,
            &self.regex,
            &self.semantic,
            &self.board,
            &self.control,
        ] {
            if !pool.drain(end.saturating_duration_since(Instant::now())) {
                return false;
            }
        }
        true
    }

    pub(super) fn shutdown(&self) {
        self.ingress.shutdown();
        self.query.shutdown();
        self.maintenance.shutdown();
        self.regex.shutdown();
        self.semantic.shutdown();
        self.board.shutdown();
        self.control.shutdown();
    }
}

fn lane_size(tier: AdmissionTier, lane: RequestLane) -> PoolSize {
    match (tier, lane) {
        (AdmissionTier::Router, RequestLane::Maintenance) => PoolSize {
            workers: 16,
            queue: 32,
        },
        (AdmissionTier::Router, RequestLane::Regex) => PoolSize {
            workers: 16,
            queue: 32,
        },
        (AdmissionTier::Router, RequestLane::Semantic) => PoolSize {
            workers: 8,
            queue: 16,
        },
        (AdmissionTier::Coordinator, RequestLane::Maintenance) => PoolSize {
            workers: 4,
            queue: 8,
        },
        (AdmissionTier::Coordinator, RequestLane::Regex) => PoolSize {
            workers: 4,
            queue: 8,
        },
        (AdmissionTier::Coordinator, RequestLane::Semantic) => PoolSize {
            workers: 2,
            queue: 8,
        },
        (AdmissionTier::Root, RequestLane::Maintenance) => PoolSize {
            workers: 1,
            queue: 8,
        },
        (AdmissionTier::Root, RequestLane::Regex) => PoolSize {
            workers: 2,
            queue: 8,
        },
        (AdmissionTier::Root, RequestLane::Semantic) => PoolSize {
            workers: 1,
            queue: 8,
        },
        // Only the router answers board and status commands; other tiers keep
        // minimal lanes.
        (AdmissionTier::Router, RequestLane::Board) => ROUTER_BOARD_LANE,
        (AdmissionTier::Router, RequestLane::Control) => PoolSize {
            workers: 2,
            queue: 16,
        },
        (_, RequestLane::Board | RequestLane::Control) => PoolSize {
            workers: 1,
            queue: 8,
        },
        (_, RequestLane::Query) => unreachable!("query size comes from the serve profile"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_widths_match_the_runtime_contract() {
        let cases = [
            (AdmissionTier::Router, RequestLane::Maintenance, (16, 32)),
            (AdmissionTier::Router, RequestLane::Regex, (16, 32)),
            (AdmissionTier::Router, RequestLane::Semantic, (8, 16)),
            (AdmissionTier::Router, RequestLane::Board, (8, 32)),
            (AdmissionTier::Router, RequestLane::Control, (2, 16)),
            (AdmissionTier::Coordinator, RequestLane::Maintenance, (4, 8)),
            (AdmissionTier::Coordinator, RequestLane::Regex, (4, 8)),
            (AdmissionTier::Coordinator, RequestLane::Semantic, (2, 8)),
            (AdmissionTier::Coordinator, RequestLane::Board, (1, 8)),
            (AdmissionTier::Coordinator, RequestLane::Control, (1, 8)),
            (AdmissionTier::Root, RequestLane::Maintenance, (1, 8)),
            (AdmissionTier::Root, RequestLane::Regex, (2, 8)),
            (AdmissionTier::Root, RequestLane::Semantic, (1, 8)),
            (AdmissionTier::Root, RequestLane::Board, (1, 8)),
            (AdmissionTier::Root, RequestLane::Control, (1, 8)),
        ];
        for (tier, lane, expected) in cases {
            let size = lane_size(tier, lane);
            assert_eq!(
                (size.workers, size.queue),
                expected,
                "lane_size({tier:?}, {lane:?})"
            );
        }
    }

    #[test]
    fn query_lane_size_comes_from_the_serve_profile() {
        for tier in [
            AdmissionTier::Router,
            AdmissionTier::Coordinator,
            AdmissionTier::Root,
        ] {
            let size = std::panic::catch_unwind(move || lane_size(tier, RequestLane::Query));
            assert!(
                size.is_err(),
                "lane_size({tier:?}, Query) must stay unreachable"
            );
        }
    }
}

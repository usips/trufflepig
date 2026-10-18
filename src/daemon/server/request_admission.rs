use super::super::pool::{PoolSize, RequestClass, RequestLane, RequestPool};
use super::AdmissionTier;
use super::root_work_limits::{RootLimitRefusal, RootWorkLimits, RootWorkPermit};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FRAME_INGRESS_WORKERS: usize = 2;
/// Bounds socket frame reads separately from request execution and routes work
/// to tier-sized query, maintenance, regex, semantic, and board queues.
pub(in crate::daemon) struct RequestAdmission {
    tier: AdmissionTier,
    accepted_budget: Duration,
    ingress: Arc<RequestPool>,
    query: Arc<RequestPool>,
    maintenance: Arc<RequestPool>,
    regex: Arc<RequestPool>,
    semantic: Arc<RequestPool>,
    board: Arc<RequestPool>,
    root_work_limits: RootWorkLimits,
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
        // Only the router answers board commands; other tiers keep a minimal lane.
        (AdmissionTier::Router, RequestLane::Board) => PoolSize {
            workers: 4,
            queue: 32,
        },
        (_, RequestLane::Board) => PoolSize {
            workers: 1,
            queue: 8,
        },
        (_, RequestLane::Query) => unreachable!("query size comes from the serve profile"),
    }
}

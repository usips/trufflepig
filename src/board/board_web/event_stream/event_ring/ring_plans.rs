//! Plan filter leases: relevance reads stop when the stream drops.
use super::EventRing;
use crate::board::board_ids::PlanId;

/// Unregisters the plan filter on drop so relevance reads stop with the stream.
pub(in crate::board::board_web::event_stream) struct PlanLease {
    pub(super) ring: EventRing,
    pub(super) plan: PlanId,
}

impl Drop for PlanLease {
    fn drop(&mut self) {
        self.ring.unsubscribe_plan(self.plan);
    }
}

impl EventRing {
    fn unsubscribe_plan(&self, plan: PlanId) {
        let mut state = self.lock();
        if let Some(index) = state.plans.iter().position(|track| track.plan == plan) {
            let track = &mut state.plans[index];
            track.subscribers = track.subscribers.saturating_sub(1);
            if track.subscribers == 0 {
                state.plans.remove(index);
            }
        }
    }
}

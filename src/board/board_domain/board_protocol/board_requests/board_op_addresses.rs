//! Read classification and plan attribution for every board operation.
use super::BoardOp;
use crate::board::board_ids::PlanId;

impl BoardOp {
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::Inbox { after: Some(_), .. }
                | Self::Show { .. }
                | Self::Search { .. }
                | Self::Review { .. }
                | Self::FeedbackList { .. }
                | Self::Repositories { .. }
                | Self::Overview { .. }
                | Self::Attention { .. }
                | Self::Feed { .. }
                | Self::History { .. }
                | Self::Entries { .. }
                | Self::Tasks { .. }
                | Self::Claims { .. }
        )
    }

    pub fn plan_id(&self) -> Option<PlanId> {
        match self {
            Self::Post { target, .. } | Self::Show { target } => target.plan_id(),
            Self::TaskCreate { plan, .. } | Self::CarveClaim { plan, .. } => Some(*plan),
            Self::TaskMove { task, .. } | Self::ClaimTask { task, .. } => Some(task.plan),
            Self::Propose { base, .. } | Self::Edit { base, .. } | Self::Review { base, .. } => {
                Some(base.plan)
            }
            Self::Feedback { plan, .. }
            | Self::Search { plan, .. }
            | Self::Repositories { plan }
            | Self::Feed { plan, .. }
            | Self::Entries { plan, .. }
            | Self::Claims { plan, .. } => *plan,
            Self::History { plan, .. } | Self::Tasks { plan, .. } => Some(*plan),
            Self::RegisterRepo { registration } => registration.plan_id,
            _ => None,
        }
    }
}

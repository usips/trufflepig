//! Read classification and plan attribution for every board operation.
use super::{BoardOp, ReadScope};
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
                | Self::Projects
                | Self::Overview { .. }
                | Self::Attention { .. }
                | Self::Feed { .. }
                | Self::History { .. }
                | Self::Entries { .. }
                | Self::Tasks { .. }
                | Self::Claims { .. }
        )
    }

    /// Concrete scopes are accepted only by these collection reads.
    pub fn read_scope_mut(&mut self) -> Option<&mut ReadScope> {
        match self {
            Self::Inbox { scope, .. }
            | Self::Overview { scope, .. }
            | Self::Attention { scope, .. }
            | Self::Claims { scope, .. }
            | Self::Feed { scope, .. } => Some(scope),
            _ => None,
        }
    }

    /// Workspace writes register the caller's repository; commit link edits
    /// use the plan's existing repository identities.
    pub fn registers_workspace(&self) -> bool {
        !self.is_read_only()
            && !matches!(
                self,
                Self::Inbox { .. } | Self::LinkCommit { .. } | Self::UnlinkCommit { .. }
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
            Self::LinkCommit { task, .. } | Self::UnlinkCommit { task, .. } => Some(task.plan),
            _ => None,
        }
    }
}

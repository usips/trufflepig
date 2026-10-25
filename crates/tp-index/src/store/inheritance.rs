pub(super) mod ordered_chains;

use anyhow::{Result, ensure};

const MAX_DERIVED_ITEMS: usize = 200_000;

/// Bounds inferred relationship edges and occurrence candidate IDs per staged index.
#[derive(Clone, Debug, Default)]
pub(super) struct DerivedBudget {
    used: usize,
}

impl DerivedBudget {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn charge(&mut self, count: usize) -> Result<()> {
        self.used = self.used.saturating_add(count);
        ensure!(
            self.used <= MAX_DERIVED_ITEMS,
            "derived inheritance fact limit exceeded"
        );
        Ok(())
    }
}

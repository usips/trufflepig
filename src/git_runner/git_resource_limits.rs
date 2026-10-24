//! Typed history resource-limit refusals. Their text renders unchanged, and
//! callers recognize them by type instead of by wording.
use crate::coded_error::{CodedError, ErrorCode};
use std::{error::Error, fmt};

/// A Git input, output, or object that exceeds a history resource limit.
#[derive(Debug)]
pub(crate) struct ResourceLimitExceeded(pub &'static str);

impl fmt::Display for ResourceLimitExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for ResourceLimitExceeded {}

/// Whether `error`'s outermost layer is a resource-limit refusal.
pub(crate) fn is_resource_limited(error: &anyhow::Error) -> bool {
    error.chain().next().is_some_and(|outer| {
        outer.is::<ResourceLimitExceeded>()
            || outer
                .downcast_ref::<CodedError>()
                .is_some_and(|coded| coded.code() == ErrorCode::HistoryResourceLimited)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn only_an_outermost_typed_limit_counts() {
        let limit = anyhow!(ResourceLimitExceeded("Git output exceeds resource limit"));
        assert_eq!(limit.to_string(), "Git output exceeds resource limit");
        assert!(is_resource_limited(&limit));
        let staged = anyhow!(coded!(
            HistoryResourceLimited,
            "serialized staging capacity reached"
        ));
        assert!(is_resource_limited(&staged));
        assert!(!is_resource_limited(
            &limit.context("historical blob unavailable")
        ));
        assert!(!is_resource_limited(&anyhow!("mentions a resource limit")));
    }
}

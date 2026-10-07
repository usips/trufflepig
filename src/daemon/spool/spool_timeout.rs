//! Provenance for a locally expired file-spool transport deadline.

use crate::daemon::deadline::TIMED_OUT;
use std::{error::Error, fmt};

#[derive(Debug)]
pub(super) struct LocalSpoolTimeout;

impl fmt::Display for LocalSpoolTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{TIMED_OUT}: spooled request deadline expired")
    }
}

impl Error for LocalSpoolTimeout {}

pub(crate) fn is_local_spool_timeout(error: &anyhow::Error) -> bool {
    error.is::<LocalSpoolTimeout>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_spool_timeout_preserves_wire_text_and_provenance() {
        let directory = crate::board::board_test_support::scratch("st-");
        let context = crate::diagnostics::RequestContext::new(None, None);
        let error = super::super::request(
            directory.path(),
            &["status".into()],
            &context,
            crate::daemon::deadline::QueryDeadline::after(std::time::Duration::ZERO),
        )
        .unwrap_err();
        let text = error.to_string();
        assert_eq!(text, "timed_out: spooled request deadline expired");
        assert!(is_local_spool_timeout(
            &error.context("local spool attempt")
        ));
        assert!(!is_local_spool_timeout(&anyhow::anyhow!(text)));
    }
}

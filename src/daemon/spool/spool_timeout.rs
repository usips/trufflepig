//! Provenance for a locally expired file-spool transport deadline.

use crate::coded_error::{CodeDetail, CodedError, ErrorCode};

/// The `timed_out` answer for a spool wait whose deadline expired locally.
pub(super) fn local_spool_timeout() -> CodedError {
    coded!(TimedOut, "spooled request deadline expired").with_detail(CodeDetail::LocalSpoolTimeout)
}

pub(crate) fn is_local_spool_timeout(error: &anyhow::Error) -> bool {
    CodeDetail::LocalSpoolTimeout.is_anywhere_in(error)
}

/// Whether `error` is a `timed_out` answer or a locally expired spool wait.
pub(crate) fn is_spool_or_coded_timeout(error: &anyhow::Error) -> bool {
    ErrorCode::TimedOut.is_code_of(error) || is_local_spool_timeout(error)
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

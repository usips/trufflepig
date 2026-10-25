//! Provenance for a locally expired file-spool transport deadline.

use crate::coded_error::{CodeDetail, CodedError, ErrorCode};

/// The `timed_out` answer for a spool wait whose deadline expired locally.
pub(super) fn local_spool_timeout() -> CodedError {
    coded!(TimedOut, "spooled request deadline expired").with_detail(CodeDetail::LocalSpoolTimeout)
}

pub fn is_local_spool_timeout(error: &anyhow::Error) -> bool {
    CodeDetail::LocalSpoolTimeout.is_anywhere_in(error)
}

/// Whether `error` is a `timed_out` answer or a locally expired spool wait.
pub fn is_spool_or_coded_timeout(error: &anyhow::Error) -> bool {
    ErrorCode::TimedOut.is_code_of(error) || is_local_spool_timeout(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_spool_timeout_preserves_wire_text_and_provenance() {
        let directory = crate::test_fixtures::scratch("st-");
        let context = crate::request_context::RequestContext::new(None, None);
        let error = super::super::request(
            directory.path(),
            &["status".into()],
            &context,
            crate::query_deadline::QueryDeadline::after(std::time::Duration::ZERO),
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

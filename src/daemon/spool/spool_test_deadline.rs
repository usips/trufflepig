//! One-shot deadline expiry after a real request reaches the spool.

use super::QueryDeadline;
use std::{cell::Cell, path::Path, time::Duration};

#[derive(Clone, Copy, PartialEq)]
enum PublicationDeadline {
    Disabled,
    Armed,
    Expired,
}

thread_local! {
    static DEADLINE: Cell<PublicationDeadline> = const { Cell::new(PublicationDeadline::Disabled) };
}

pub(crate) fn expire_after_next_publication() {
    DEADLINE.set(PublicationDeadline::Armed);
}

pub(crate) fn publication_expired() -> bool {
    DEADLINE.get() == PublicationDeadline::Expired
}

pub(super) fn after_publication(path: &Path, deadline: QueryDeadline) -> QueryDeadline {
    if DEADLINE.get() == PublicationDeadline::Armed {
        assert!(
            path.is_file(),
            "deadline must expire after request publication"
        );
        DEADLINE.set(PublicationDeadline::Expired);
        QueryDeadline::after(Duration::ZERO)
    } else {
        deadline
    }
}

use super::board_wait::{BoardInboxWaiterPermit, MAX_WAITERS, waiter_transient};
use super::board_writer::lock_before;
use super::*;
use crate::board::board_protocol::{BoardOp, BoardResult, InboxWait};
use crate::diagnostics::RequestContext;
use std::sync::atomic::Ordering;

mod board_configuration_tests;
mod board_registration_tests;
mod board_wait_tests;
mod board_writer_tests;
mod project_scope_host_tests;

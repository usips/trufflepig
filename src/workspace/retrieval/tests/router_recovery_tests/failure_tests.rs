use super::super::super::{MemberCommand, WorkspaceQuery, search_with_policy_using};
use super::{PreparedWorkspace, direct_source_fallback, indexed_fixture, replaced_member};
use std::time::Duration;
use tp_core::{coded_error::ErrorCode, query_deadline::QueryDeadline};
use tp_semantic::SemanticSession;

#[test]
fn incomplete_retry_without_an_answer_reports_unproven_absence() {
    let fixture = indexed_fixture();
    let prepared = PreparedWorkspace::new(&fixture, &["search", "sym:SharedThing", "in:pack"]);
    let request = prepared.request();
    let mut session = SemanticSession::default();
    let error = search_with_policy_using(
        &request,
        &mut session,
        |_request, _command: &MemberCommand<'_>, member, _first, _map_miss| {
            assert_eq!(member.name(), "pack");
            Ok(replaced_member())
        },
        |_request, _member| {
            Ok(
                crate::workspace::stateless_queries::RouterReplacementAnswer {
                    output: String::new(),
                    complete: false,
                    incomplete_reason: Some("current-source scan was incomplete".to_owned()),
                },
            )
        },
    )
    .unwrap_err();

    assert!(
        ErrorCode::WorkspaceUnavailable.is_code_of(&error),
        "{error:#}"
    );
    assert!(
        format!("{error:#}").contains("absence=not_proven"),
        "{error:#}"
    );
}

#[test]
fn expired_parent_deadline_is_not_reset_for_recovery() {
    let fixture = indexed_fixture();
    let prepared = PreparedWorkspace::new(&fixture, &["search", "sym:SharedThing", "in:pack"]);
    let mut request: WorkspaceQuery<'_> = prepared.request();
    request.deadline = QueryDeadline::after(Duration::ZERO);
    let mut session = SemanticSession::default();
    let error = search_with_policy_using(
        &request,
        &mut session,
        |_request, _command: &MemberCommand<'_>, member, _first, _map_miss| {
            assert_eq!(member.name(), "pack");
            Ok(replaced_member())
        },
        |request, member| {
            assert!(request.deadline.time_expired());
            direct_source_fallback(request, member)
        },
    )
    .unwrap_err();

    assert!(ErrorCode::TimedOut.is_code_of(&error), "{error:#}");
    assert!(
        format!("{error:#}").contains("absence=not_proven"),
        "{error:#}"
    );
}

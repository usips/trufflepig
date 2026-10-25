//! Browser operation requests: allowlisted ops under captured local identity.
use super::super::WebStore;
use crate::{
    board_backend::BoardBackend,
    board_config::BoardConfig,
    board_protocol::{BOARD_API, BoardError, BoardErrorCode, BoardOp, BoardReply, BoardRequest},
    board_vocabulary::EntryKind,
};
use serde::Deserialize;
use std::{net::IpAddr, time::Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebRequest {
    pub api: u32,
    pub op: BoardOp,
    #[serde(default)]
    pub project: Option<String>,
}

impl WebRequest {
    pub(crate) fn into_request(
        self,
        config: &BoardConfig,
        peer_ip: IpAddr,
    ) -> Result<BoardRequest, BoardError> {
        if self.api != BOARD_API {
            return Err(BoardError::new(
                BoardErrorCode::BoardApiMismatch,
                format!("expected {BOARD_API}, received {}", self.api),
            ));
        }
        let name = serde_json::to_value(&self.op).map_err(|error| invalid(error.to_string()))?;
        let allowed = matches!(
            name["op"].as_str(),
            Some(
                "overview"
                    | "repositories"
                    | "projects"
                    | "show"
                    | "tasks"
                    | "done_tasks"
                    | "claims"
                    | "feed"
                    | "attention"
                    | "history"
                    | "entries"
                    | "search"
                    | "feedback_list"
                    | "new"
                    | "post"
                    | "task_create"
                    | "task_move"
                    | "task_transfer"
                    | "plan_close"
                    | "plan_reopen"
                    | "accept"
                    | "reject"
                    | "edit"
                    | "feedback_close"
                    | "feedback_triage"
                    | "grant"
                    | "revoke"
            )
        );
        if !allowed {
            return Err(invalid("op not available over web"));
        }
        if let BoardOp::Post { kind, .. } = &self.op {
            if !matches!(
                kind,
                EntryKind::Note | EntryKind::Answer | EntryKind::Decision | EntryKind::Question
            ) {
                return Err(invalid(
                    "the web board accepts note, answer, decision, and question posts",
                ));
            }
        }
        let actor = config
            .actor(Some("human"), Some("web"))
            .map_err(BoardError::from)?;
        let mut request = BoardRequest::new(actor, self.op);
        request.origin = Some(request.actor.origin());
        request.observe_peer_ip(peer_ip);
        request.validate().map_err(BoardError::from)?;
        Ok(request)
    }
}

pub(crate) fn execute(
    store: &WebStore,
    mut request: WebRequest,
    peer_ip: IpAddr,
    expires: Instant,
) -> Result<BoardReply, BoardError> {
    let config = store.config(expires)?;
    let project = request.project.take();
    let mut request = request.into_request(&config, peer_ip)?;
    if let Some(selector) = project {
        crate::board_projects::validate_scope_selection(&mut request.op)?;
        let projects = store.projects(&request.actor.host, expires)?;
        crate::board_projects::select_scope(&mut request.op, &selector, &projects)?;
    }
    if matches!(request.op, BoardOp::Projects) {
        let projects = store.projects(&request.actor.host, expires)?;
        let deadline = tp_core::query_deadline::QueryDeadline::after(
            expires.saturating_duration_since(Instant::now()),
        );
        return crate::board_projects::project_reply(&config, projects, &deadline);
    }
    let reply = if request.op.is_read_only() {
        store
            .readers
            .with_reader(&config, expires, |reader| reader.handle(&request))?
    } else {
        store.with_writer(&config, expires, |writer| writer.handle(&request))?
    };
    Ok(reply)
}

fn invalid(message: impl Into<String>) -> BoardError {
    BoardError::new(BoardErrorCode::InvalidOptions, message)
}

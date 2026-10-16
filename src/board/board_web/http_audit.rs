//! Bounded, structured audit records for Board HTTP interactions.
use super::http_wire::{HttpMethod, HttpRequest};
use std::net::IpAddr;

#[derive(Clone, Copy, Debug)]
enum Method {
    Get,
    Post,
    Unknown,
}

impl Method {
    fn label(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Route {
    Unparsed,
    Root,
    Asset,
    Challenge,
    Events,
    Board,
    Ingest,
    RenderPlan,
    RenderDiff,
    RenderProposal,
    Unknown,
}

impl Route {
    fn classify(path: &str) -> Self {
        match path {
            "/" => Self::Root,
            "/api/v1/challenge" => Self::Challenge,
            "/api/v1/events" => Self::Events,
            "/api/v1/board" => Self::Board,
            "/api/v1/ingest" => Self::Ingest,
            path if path.starts_with("/api/v1/render/plan/") => Self::RenderPlan,
            path if path.starts_with("/api/v1/render/diff/") => Self::RenderDiff,
            path if path.starts_with("/api/v1/render/proposal/") => Self::RenderProposal,
            path if super::web_routes::public_asset(path).is_some() => Self::Asset,
            _ => Self::Unknown,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Unparsed => "unparsed",
            Self::Root => "root",
            Self::Asset => "asset",
            Self::Challenge => "challenge",
            Self::Events => "events",
            Self::Board => "board",
            Self::Ingest => "ingest",
            Self::RenderPlan => "render_plan",
            Self::RenderDiff => "render_diff",
            Self::RenderProposal => "render_proposal",
            Self::Unknown => "unknown",
        }
    }
}

/// One request's audit metadata; route values never contain client input.
#[derive(Debug)]
pub(super) struct HttpAudit {
    peer_ip: Option<IpAddr>,
    method: Method,
    route: Route,
    finished: bool,
}

impl HttpAudit {
    pub(super) fn new(peer_ip: IpAddr) -> Self {
        Self::with_optional_peer(Some(peer_ip))
    }

    fn with_optional_peer(peer_ip: Option<IpAddr>) -> Self {
        Self {
            peer_ip,
            method: Method::Unknown,
            route: Route::Unparsed,
            finished: false,
        }
    }

    #[cfg(test)]
    pub(super) fn from_request(peer_ip: IpAddr, request: &HttpRequest) -> Self {
        let mut audit = Self::new(peer_ip);
        audit.request(request);
        audit
    }

    pub(super) fn request(&mut self, request: &HttpRequest) {
        self.method = match request.method {
            HttpMethod::Get => Method::Get,
            HttpMethod::Post => Method::Post,
        };
        self.route = Route::classify(request.path());
    }

    pub(super) fn finish(&mut self, status: u16) {
        if !self.finished {
            self.emit(status);
            self.finished = true;
        }
    }

    fn emit(&self, status: u16) {
        eprintln!("{}", self.record(status));
    }

    fn record(&self, status: u16) -> serde_json::Value {
        serde_json::json!({
            "event": "board_http",
            "peer_ip": self.peer_ip.map_or_else(|| "unknown".to_owned(), |ip| ip.to_string()),
            "method": self.method.label(),
            "route": self.route.label(),
            "status": status,
        })
    }

    pub(super) fn defer(&mut self) -> Self {
        self.finished = true;
        Self {
            peer_ip: self.peer_ip,
            method: self.method,
            route: self.route,
            finished: false,
        }
    }

    pub(super) fn suspended(&self) -> Self {
        Self {
            peer_ip: self.peer_ip,
            method: self.method,
            route: self.route,
            finished: true,
        }
    }

    pub(super) fn activate(&mut self) {
        self.finished = false;
    }

    pub(super) fn suspend(&mut self) {
        self.finished = true;
    }
}

impl Drop for HttpAudit {
    fn drop(&mut self) {
        if !self.finished {
            self.emit(500);
        }
    }
}

pub(super) fn record_unparsed(peer_ip: Option<IpAddr>, status: u16) {
    let mut audit = HttpAudit::with_optional_peer(peer_ip);
    audit.finish(status);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn audit_record_uses_bounded_route_labels_and_excludes_request_data() {
        let request = HttpRequest {
            method: HttpMethod::Post,
            target: "/api/v1/render/plan/P7?token=secret#fragment".into(),
            headers: [
                ("x-board-token".into(), "credential".into()),
                ("x-forwarded-for".into(), "203.0.113.10".into()),
            ]
            .into(),
            body: b"private body marker".to_vec(),
        };
        let mut audit = HttpAudit::from_request(Ipv4Addr::LOCALHOST.into(), &request);
        let record = audit.record(403);
        audit.finished = true;
        assert_eq!(record["event"], "board_http");
        assert_eq!(record["peer_ip"], "127.0.0.1");
        assert_eq!(record["method"], "POST");
        assert_eq!(record["route"], "render_plan");
        assert_eq!(record["status"], 403);
        let serialized = record.to_string();
        for secret in [
            "P7",
            "secret",
            "fragment",
            "credential",
            "203.0.113.10",
            "private body marker",
        ] {
            assert!(!serialized.contains(secret));
        }
    }

    #[test]
    fn malformed_and_unknown_routes_remain_bounded() {
        let mut malformed = HttpAudit::with_optional_peer(None);
        let malformed_record = malformed.record(431);
        malformed.finished = true;
        let malformed = malformed_record;
        assert_eq!(malformed["peer_ip"], "unknown");
        assert_eq!(malformed["method"], "unknown");
        assert_eq!(malformed["route"], "unparsed");
        assert_eq!(malformed["status"], 431);

        let request = HttpRequest {
            method: HttpMethod::Get,
            target: "/totally/unknown/path?private=1".into(),
            headers: Default::default(),
            body: vec![],
        };
        let mut audit = HttpAudit::from_request(IpAddr::V4(Ipv4Addr::LOCALHOST), &request);
        let unknown = audit.record(404);
        audit.finished = true;
        assert_eq!(unknown["route"], "unknown");
        assert_eq!(unknown["status"], 404);
        assert!(!unknown.to_string().contains("private"));
    }
}

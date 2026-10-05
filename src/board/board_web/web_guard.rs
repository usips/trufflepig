//! Loopback authority, browser-origin, and rotating local-token checks.
//! Bootstrap URLs carry the secret only in the fragment; private requests use a header.

mod challenge;
#[cfg(test)]
mod tests;
mod token_file;

use super::http_wire::{HttpError, HttpMethod, HttpRequest};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
};

pub(crate) use challenge::ChallengeNonce;
pub(crate) use token_file::BoardWebToken;

/// How a route authenticates: shell assets are public, the ownership challenge
/// proves token possession without receiving it, everything else needs the token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteAccess {
    Public,
    Challenge,
    Private,
}

pub(crate) struct WebGuard {
    address: SocketAddr,
    authority: String,
    allowed_authorities: [String; 3],
    origin: String,
    token: BoardWebToken,
}

impl WebGuard {
    /// Adopt an already-bound listener (the serve path reuses the persisted
    /// port first); rotation still precedes any publish.
    pub(crate) fn with_listener(listener: TcpListener) -> io::Result<(TcpListener, Self)> {
        let token = BoardWebToken::rotate().map_err(io::Error::other)?;
        let guard = Self::with_token(listener.local_addr()?, token)?;
        Ok((listener, guard))
    }

    /// Browser origin for a bound loopback listener address.
    pub(crate) fn origin_for(address: SocketAddr) -> String {
        let port = if address.port() == 80 {
            String::new()
        } else {
            format!(":{}", address.port())
        };
        let authority = if address.is_ipv4() {
            format!("127.0.0.1{port}")
        } else {
            format!("[::1]{port}")
        };
        format!("http://{authority}")
    }

    /// The address is the listener's actual local address, including its assigned port.
    pub(crate) fn with_token(address: SocketAddr, token: BoardWebToken) -> io::Result<Self> {
        require_loopback(address)?;
        if address.port() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bound port required",
            ));
        }
        let port = if address.port() == 80 {
            String::new()
        } else {
            format!(":{}", address.port())
        };
        let allowed_authorities = [
            format!("127.0.0.1{port}"),
            format!("localhost{port}"),
            format!("[::1]{port}"),
        ];
        let authority = allowed_authorities[if address.is_ipv4() { 0 } else { 2 }].clone();
        let origin = Self::origin_for(address);
        Ok(Self {
            address,
            authority,
            allowed_authorities,
            origin,
            token,
        })
    }

    pub(crate) fn authority(&self) -> &str {
        &self.authority
    }

    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }

    pub(crate) fn bootstrap_url(&self) -> String {
        format!("{}/#token={}", self.origin, self.token.expose())
    }

    /// Hex HMAC proof over the client nonce and this listener's address.
    pub(crate) fn challenge_proof(&self, nonce: &ChallengeNonce) -> String {
        challenge::proof_hex(&self.token, nonce, &self.address)
    }

    /// Constant-time check of a replied proof against the local token.
    pub(crate) fn challenge_matches(&self, nonce: &ChallengeNonce, candidate_hex: &str) -> bool {
        challenge::proof_matches(&self.token, nonce, &self.address, candidate_hex)
    }

    /// Call for every route. Only the GET shell, static assets, and the
    /// ownership challenge answer without the token.
    pub(crate) fn authorize(
        &self,
        request: &HttpRequest,
        access: RouteAccess,
        json_body: bool,
    ) -> Result<(), HttpError> {
        let host = request.header("host").unwrap_or_default();
        if !self
            .allowed_authorities
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
        {
            return Err(HttpError::new(421, "Host does not match bound authority"));
        }
        let origin = request.header("origin");
        let matches_origin = |origin: &str| {
            origin
                .strip_prefix("http://")
                .is_some_and(|authority| authority.eq_ignore_ascii_case(host))
        };
        if (request.method == HttpMethod::Post && !origin.is_some_and(matches_origin))
            || origin.is_some_and(|origin| !matches_origin(origin))
        {
            return Err(HttpError::new(403, "Origin forbidden"));
        }
        match access {
            RouteAccess::Public if request.method != HttpMethod::Get => {
                return Err(HttpError::new(405, "public route requires GET").with_allow("GET"));
            }
            RouteAccess::Challenge if request.method != HttpMethod::Post => {
                return Err(HttpError::new(405, "challenge route requires POST").with_allow("POST"));
            }
            RouteAccess::Private
                if !request
                    .header("x-board-token")
                    .is_some_and(|token| self.token.matches(token)) =>
            {
                return Err(HttpError::new(403, "board token required"));
            }
            _ => {}
        }
        if json_body
            && request.method == HttpMethod::Post
            && !request
                .header("content-type")
                .is_some_and(|value| value.eq_ignore_ascii_case("application/json"))
        {
            return Err(HttpError::new(403, "application/json required"));
        }
        Ok(())
    }
}

pub(super) fn require_loopback(address: SocketAddr) -> io::Result<()> {
    let ip_allowed = matches!(address.ip(), IpAddr::V4(ip) if ip == Ipv4Addr::LOCALHOST)
        || matches!(address.ip(), IpAddr::V6(ip) if ip == Ipv6Addr::LOCALHOST);
    let scoped = matches!(address, SocketAddr::V6(address) if address.scope_id() != 0 || address.flowinfo() != 0);
    if !ip_allowed || scoped {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "loopback bind required",
        ));
    }
    Ok(())
}

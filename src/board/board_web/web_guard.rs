//! Loopback authority, browser-origin, and stable local-token checks.
//! Bootstrap URLs carry the secret only in the fragment; private requests use a header.

#[cfg(test)]
mod tests;
mod token_file;

use super::http_wire::{HttpError, HttpMethod, HttpRequest};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
};

pub(crate) use token_file::BoardWebToken;

pub(crate) struct WebGuard {
    authority: String,
    allowed_authorities: [String; 3],
    origin: String,
    token: BoardWebToken,
}

impl WebGuard {
    pub(crate) fn bind(address: SocketAddr) -> io::Result<(TcpListener, Self)> {
        require_loopback(address)?;
        let listener = TcpListener::bind(address)?;
        let token = BoardWebToken::load().map_err(io::Error::other)?;
        let guard = Self::with_token(listener.local_addr()?, token)?;
        Ok((listener, guard))
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
        let origin = format!("http://{authority}");
        Ok(Self {
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

    /// Call for every route. Only the GET shell and static assets are public.
    pub(crate) fn authorize(
        &self,
        request: &HttpRequest,
        private: bool,
        json_body: bool,
    ) -> Result<(), HttpError> {
        let host = request.header("host").unwrap_or_default();
        if !self
            .allowed_authorities
            .iter()
            .any(|allowed| allowed == host)
        {
            return Err(HttpError::new(421, "Host does not match bound authority"));
        }
        let origin = request.header("origin");
        let matches_origin = |origin: &str| origin.strip_prefix("http://") == Some(host);
        if (request.method == HttpMethod::Post && !origin.is_some_and(matches_origin))
            || origin.is_some_and(|origin| !matches_origin(origin))
        {
            return Err(HttpError::new(403, "Origin forbidden"));
        }
        if !private && request.method != HttpMethod::Get {
            return Err(HttpError::new(405, "public route requires GET"));
        }
        if private
            && !request
                .header("x-board-token")
                .is_some_and(|token| self.token.matches(token))
        {
            return Err(HttpError::new(403, "board token required"));
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

fn require_loopback(address: SocketAddr) -> io::Result<()> {
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

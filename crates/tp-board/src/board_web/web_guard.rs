//! Local/LAN authority, browser-origin, and rotating Board-web token checks.
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
    allow_lan_hosts: bool,
    origin: String,
    token: BoardWebToken,
}

impl WebGuard {
    /// Adopt an already-bound listener (the serve path reuses the persisted
    /// port first); rotation still precedes any publish.
    pub(crate) fn with_listener(listener: TcpListener) -> io::Result<(TcpListener, Self)> {
        let address = listener.local_addr()?;
        require_supported_bind_address(address)?;
        let token = BoardWebToken::rotate().map_err(io::Error::other)?;
        let guard = Self::with_token(address, token)?;
        Ok((listener, guard))
    }

    /// Browser origin for a bound local or private-LAN listener address.
    pub(crate) fn origin_for(address: SocketAddr) -> String {
        format!("http://{}", Self::authority_for(address))
    }

    fn authority_for(address: SocketAddr) -> String {
        let port = if address.port() == 80 {
            String::new()
        } else {
            format!(":{}", address.port())
        };
        match address.ip() {
            IpAddr::V4(ip) if is_lan_ipv4(ip) => format!("{ip}{port}"),
            IpAddr::V4(_) => format!("127.0.0.1{port}"),
            IpAddr::V6(_) => format!("[::1]{port}"),
        }
    }

    /// The address is the listener's actual local address, including its assigned port.
    pub(crate) fn with_token(address: SocketAddr, token: BoardWebToken) -> io::Result<Self> {
        require_supported_bind_address(address)?;
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
        let authority = Self::authority_for(address);
        let origin = format!("http://{authority}");
        let allow_lan_hosts =
            matches!(address.ip(), IpAddr::V4(ip) if ip.is_unspecified() || is_lan_ipv4(ip));
        Ok(Self {
            address,
            authority,
            allowed_authorities,
            allow_lan_hosts,
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
        if !self.host_is_allowed(host) {
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

    fn host_is_allowed(&self, host: &str) -> bool {
        self.allowed_authorities
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
            || self.lan_host_is_allowed(host)
    }

    fn lan_host_is_allowed(&self, host: &str) -> bool {
        if !self.allow_lan_hosts {
            return false;
        }
        let (literal, has_valid_port) = if self.address.port() == 80 {
            match host.rsplit_once(':') {
                Some(_) => return false,
                None => (host, true),
            }
        } else {
            let expected_port = self.address.port().to_string();
            match host.rsplit_once(':') {
                Some((literal, port)) => (literal, port == expected_port.as_str()),
                None => return false,
            }
        };
        if !has_valid_port {
            return false;
        }
        let Ok(ip) = literal.parse::<Ipv4Addr>() else {
            return false;
        };
        if ip.to_string() != literal || !is_lan_ipv4(ip) {
            return false;
        }
        match self.address.ip() {
            IpAddr::V4(bound) if bound.is_unspecified() => true,
            IpAddr::V4(bound) => ip == bound,
            IpAddr::V6(_) => false,
        }
    }
}

fn is_lan_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 192 && octets[1] == 168
}

pub(crate) fn require_supported_bind_address(address: SocketAddr) -> io::Result<()> {
    let ip_allowed = match address.ip() {
        IpAddr::V4(ip) => ip == Ipv4Addr::LOCALHOST || ip.is_unspecified() || is_lan_ipv4(ip),
        IpAddr::V6(ip) => ip == Ipv6Addr::LOCALHOST,
    };
    let scoped = matches!(address, SocketAddr::V6(address) if address.scope_id() != 0 || address.flowinfo() != 0);
    if !ip_allowed || scoped {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported Board web bind address",
        ));
    }
    Ok(())
}

pub(super) fn normalize_peer_ip(peer: IpAddr) -> IpAddr {
    match peer {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(peer),
        IpAddr::V4(_) => peer,
    }
}

pub(super) fn allowed_peer_ip(peer: IpAddr) -> bool {
    match normalize_peer_ip(peer) {
        IpAddr::V4(ip) => ip.is_loopback() || is_lan_ipv4(ip),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

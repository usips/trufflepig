mod token_security;

use super::*;
use std::{collections::BTreeMap, io::Write, net::TcpStream, time::Instant};

fn fixture() -> (tempfile::TempDir, WebGuard) {
    let directory = tempfile::tempdir().unwrap();
    let token = BoardWebToken::rotate_at(&directory.path().join("board-web.token")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let guard = WebGuard::with_token(listener.local_addr().unwrap(), token).unwrap();
    (directory, guard)
}

fn request(guard: &WebGuard, method: HttpMethod, authenticated: bool) -> HttpRequest {
    let mut headers = BTreeMap::new();
    headers.insert("host".to_owned(), guard.authority().to_owned());
    if authenticated {
        headers.insert("x-board-token".to_owned(), guard.token.expose().to_owned());
    }
    if method == HttpMethod::Post {
        headers.insert("origin".to_owned(), guard.origin().to_owned());
        headers.insert("content-type".to_owned(), "application/json".to_owned());
    }
    HttpRequest {
        method,
        target: "/api/v1/board".to_owned(),
        headers,
        body: Vec::new(),
    }
}

#[test]
fn host_and_origin_comparisons_ignore_ascii_case() {
    let (_directory, guard) = fixture();
    let port = guard.authority().rsplit_once(':').unwrap().1;
    let mut request = request(&guard, HttpMethod::Get, true);
    for host in [format!("LOCALHOST:{port}"), format!("LocalHost:{port}")] {
        request.headers.insert("host".to_owned(), host.clone());
        assert!(
            guard.authorize(&request, RouteAccess::Private, false).is_ok(),
            "{host}"
        );
        request
            .headers
            .insert("origin".to_owned(), format!("http://localhost:{port}"));
        assert!(
            guard.authorize(&request, RouteAccess::Private, false).is_ok(),
            "{host} with lowercase origin"
        );
    }
    request.method = HttpMethod::Post;
    request
        .headers
        .insert("host".to_owned(), format!("LOCALHOST:{port}"));
    request
        .headers
        .insert("origin".to_owned(), format!("http://localhost:{port}"));
    request
        .headers
        .insert("content-type".to_owned(), "application/json".to_owned());
    assert!(guard.authorize(&request, RouteAccess::Private, true).is_ok());
    request
        .headers
        .insert("host".to_owned(), "EXAMPLE.COM".to_owned());
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Private, true)
            .unwrap_err()
            .status,
        421
    );
}

#[test]
fn method_refusals_carry_their_allow_list() {
    let (_directory, guard) = fixture();
    let mut request = request(&guard, HttpMethod::Post, false);
    request.target = "/".to_owned();
    let error = guard
        .authorize(&request, RouteAccess::Public, true)
        .unwrap_err();
    assert_eq!(error.status, 405);
    assert_eq!(error.allow, Some("GET"));
    request.method = HttpMethod::Get;
    request.target = "/api/v1/challenge".to_owned();
    let error = guard
        .authorize(&request, RouteAccess::Challenge, false)
        .unwrap_err();
    assert_eq!(error.status, 405);
    assert_eq!(error.allow, Some("POST"));
}

#[test]
fn guards_actual_ephemeral_port_and_literal_loopback_authority() {
    let (_directory, guard) = fixture();
    let mut request = request(&guard, HttpMethod::Get, true);
    assert!(guard.authorize(&request, RouteAccess::Private, false).is_ok());
    for host in [
        "127.0.0.1:0",
        "localhost:1234",
        "example.com:1234",
        "127.0.0.1",
    ] {
        request.headers.insert("host".to_owned(), host.to_owned());
        assert_eq!(
            guard.authorize(&request, RouteAccess::Private, false).unwrap_err().status,
            421
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    let ipv6 = WebGuard::with_token("[::1]:32123".parse().unwrap(), token).unwrap();
    assert_eq!(ipv6.authority(), "[::1]:32123");
    assert_eq!(ipv6.origin(), "http://[::1]:32123");
}

#[test]
fn refuses_non_loopback_before_loading_any_live_token() {
    for address in ["0.0.0.0:0", "[::]:0", "192.0.2.1:0", "127.0.0.2:0"] {
        assert!(WebGuard::bind(address.parse().unwrap()).is_err());
    }
    let scoped = std::net::SocketAddrV6::new(std::net::Ipv6Addr::LOCALHOST, 0, 0, 1);
    assert!(WebGuard::bind(SocketAddr::V6(scoped)).is_err());
    let directory = tempfile::tempdir().unwrap();
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    assert!(WebGuard::with_token("127.0.0.1:0".parse().unwrap(), token).is_err());
}

#[test]
fn accepts_all_same_port_aliases_and_requires_matching_request_origin() {
    let (_directory, guard) = fixture();
    let port = guard.authority().rsplit_once(':').unwrap().1;
    let aliases = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    for host in &aliases {
        let mut request = request(&guard, HttpMethod::Get, true);
        request.headers.insert("host".to_owned(), host.clone());
        assert!(guard.authorize(&request, RouteAccess::Private, false).is_ok());
        request
            .headers
            .insert("origin".to_owned(), format!("http://{host}"));
        assert!(guard.authorize(&request, RouteAccess::Private, false).is_ok());
        request.method = HttpMethod::Post;
        request
            .headers
            .insert("content-type".to_owned(), "application/json".to_owned());
        assert!(guard.authorize(&request, RouteAccess::Private, true).is_ok());
        for other in aliases.iter().filter(|other| *other != host) {
            request
                .headers
                .insert("origin".to_owned(), format!("http://{other}"));
            assert_eq!(
                guard.authorize(&request, RouteAccess::Private, true).unwrap_err().status,
                403
            );
            request.method = HttpMethod::Get;
            assert_eq!(
                guard.authorize(&request, RouteAccess::Private, false).unwrap_err().status,
                403
            );
            request.method = HttpMethod::Post;
        }
    }
}

#[test]
fn default_http_port_uses_browser_normalized_authorities() {
    let directory = tempfile::tempdir().unwrap();
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    let guard = WebGuard::with_token("127.0.0.1:80".parse().unwrap(), token).unwrap();
    assert_eq!(guard.origin(), "http://127.0.0.1");
    for host in ["127.0.0.1", "localhost", "[::1]"] {
        let mut request = request(&guard, HttpMethod::Post, true);
        request.headers.insert("host".to_owned(), host.to_owned());
        request
            .headers
            .insert("origin".to_owned(), format!("http://{host}"));
        assert!(guard.authorize(&request, RouteAccess::Private, true).is_ok());
    }
}

#[test]
fn every_private_read_requires_token_while_public_gets_do_not() {
    let (_directory, guard) = fixture();
    let mut request = request(&guard, HttpMethod::Get, false);
    for target in [
        "/api/v1/board",
        "/api/v1/render",
        "/api/v1/diff",
        "/api/v1/events",
    ] {
        request.target = target.to_owned();
        assert_eq!(
            guard.authorize(&request, RouteAccess::Private, false).unwrap_err().status,
            403
        );
        request
            .headers
            .insert("x-board-token".to_owned(), "bad-token".to_owned());
        assert_eq!(
            guard.authorize(&request, RouteAccess::Private, false).unwrap_err().status,
            403
        );
        request.headers.remove("x-board-token");
    }
    for target in ["/", "/board.js", "/board.css"] {
        request.target = target.to_owned();
        assert!(guard.authorize(&request, RouteAccess::Public, false).is_ok());
    }
    assert!(!guard.authority().contains(guard.token.expose()));
    assert!(!guard.origin().contains(guard.token.expose()));
    assert_eq!(
        guard.bootstrap_url(),
        format!("{}/#token={}", guard.origin(), guard.token.expose())
    );
}

#[test]
fn post_requires_exact_origin_and_unparameterized_json_media_type() {
    let (_directory, guard) = fixture();
    let mut request = request(&guard, HttpMethod::Post, true);
    assert!(guard.authorize(&request, RouteAccess::Private, true).is_ok());
    for origin in [
        None,
        Some("null"),
        Some("http://evil.example"),
        Some("http://127.0.0.1:0"),
    ] {
        match origin {
            Some(origin) => {
                request
                    .headers
                    .insert("origin".to_owned(), origin.to_owned());
            }
            None => {
                request.headers.remove("origin");
            }
        }
        assert_eq!(
            guard.authorize(&request, RouteAccess::Private, true).unwrap_err().status,
            403
        );
    }
    request
        .headers
        .insert("origin".to_owned(), format!("{}/", guard.origin()));
    assert_eq!(
        guard.authorize(&request, RouteAccess::Private, true).unwrap_err().status,
        403
    );
    request
        .headers
        .insert("origin".to_owned(), guard.origin().to_owned());
    for content_type in [
        None,
        Some("text/plain"),
        Some("application/json; charset=utf-8"),
    ] {
        match content_type {
            Some(content_type) => {
                request
                    .headers
                    .insert("content-type".to_owned(), content_type.to_owned());
            }
            None => {
                request.headers.remove("content-type");
            }
        }
        assert_eq!(
            guard.authorize(&request, RouteAccess::Private, true).unwrap_err().status,
            403
        );
    }
    request
        .headers
        .insert("content-type".to_owned(), "Application/JSON".to_owned());
    assert!(guard.authorize(&request, RouteAccess::Private, true).is_ok());
    request.headers.remove("x-board-token");
    assert_eq!(
        guard.authorize(&request, RouteAccess::Private, true).unwrap_err().status,
        403
    );
}

#[test]
fn provided_foreign_get_origin_is_forbidden_even_with_correct_token() {
    let (_directory, guard) = fixture();
    let mut request = request(&guard, HttpMethod::Get, true);
    assert!(guard.authorize(&request, RouteAccess::Private, false).is_ok());
    request
        .headers
        .insert("origin".to_owned(), guard.origin().to_owned());
    assert!(guard.authorize(&request, RouteAccess::Private, false).is_ok());
    request
        .headers
        .insert("origin".to_owned(), "http://evil.example".to_owned());
    assert_eq!(
        guard.authorize(&request, RouteAccess::Private, false).unwrap_err().status,
        403
    );
    assert_eq!(
        guard.authorize(&request, RouteAccess::Public, false).unwrap_err().status,
        403
    );
}

#[test]
fn socket_parsed_post_reaches_same_auth_checks() {
    let directory = tempfile::tempdir().unwrap();
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let guard = WebGuard::with_token(listener.local_addr().unwrap(), token).unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client.write_all(format!(
        "POST /api/v1/board HTTP/1.1\r\nHost: {}\r\nOrigin: http://evil.example\r\nX-Board-Token: {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}",
        guard.authority(), guard.token.expose(),
    ).as_bytes()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let request = super::super::http_wire::read_request(&mut server, Instant::now()).unwrap();
    assert_eq!(
        guard.authorize(&request, RouteAccess::Private, true).unwrap_err().status,
        403
    );
}

#[test]
fn challenge_access_needs_no_token_but_keeps_transport_checks() {
    let (_directory, guard) = fixture();
    let mut request = request(&guard, HttpMethod::Post, false);
    request.target = "/api/v1/challenge".to_owned();
    assert!(guard.authorize(&request, RouteAccess::Challenge, true).is_ok());
    request.method = HttpMethod::Get;
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Challenge, false)
            .unwrap_err()
            .status,
        405
    );
    request.method = HttpMethod::Post;
    request
        .headers
        .insert("origin".to_owned(), "http://evil.example".to_owned());
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Challenge, true)
            .unwrap_err()
            .status,
        403
    );
    request
        .headers
        .insert("origin".to_owned(), guard.origin().to_owned());
    request
        .headers
        .insert("host".to_owned(), "example.com".to_owned());
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Challenge, true)
            .unwrap_err()
            .status,
        421
    );
}

#[test]
fn challenge_proof_verifies_only_with_the_local_token_and_address() {
    let (directory, guard) = fixture();
    let nonce = ChallengeNonce::generate().unwrap();
    let proof = guard.challenge_proof(&nonce);
    assert_eq!(proof.len(), 64);
    assert!(guard.challenge_matches(&nonce, &proof));
    let other = BoardWebToken::rotate_at(&directory.path().join("other.token")).unwrap();
    let address: SocketAddr = guard.authority().parse().unwrap();
    let foreign = WebGuard::with_token(address, other).unwrap();
    assert!(!foreign.challenge_matches(&nonce, &proof));
    let replayed = ChallengeNonce::generate().unwrap();
    assert!(!guard.challenge_matches(&replayed, &proof));
}

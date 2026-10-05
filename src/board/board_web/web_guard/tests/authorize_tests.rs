use super::*;

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
            guard
                .authorize(&request, RouteAccess::Private, false)
                .unwrap_err()
                .status,
            403
        );
        request
            .headers
            .insert("x-board-token".to_owned(), "bad-token".to_owned());
        assert_eq!(
            guard
                .authorize(&request, RouteAccess::Private, false)
                .unwrap_err()
                .status,
            403
        );
        request.headers.remove("x-board-token");
    }
    for target in ["/", "/board.js", "/board.css"] {
        request.target = target.to_owned();
        assert!(
            guard
                .authorize(&request, RouteAccess::Public, false)
                .is_ok()
        );
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
    assert!(
        guard
            .authorize(&request, RouteAccess::Private, true)
            .is_ok()
    );
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
            guard
                .authorize(&request, RouteAccess::Private, true)
                .unwrap_err()
                .status,
            403
        );
    }
    request
        .headers
        .insert("origin".to_owned(), format!("{}/", guard.origin()));
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Private, true)
            .unwrap_err()
            .status,
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
            guard
                .authorize(&request, RouteAccess::Private, true)
                .unwrap_err()
                .status,
            403
        );
    }
    request
        .headers
        .insert("content-type".to_owned(), "Application/JSON".to_owned());
    assert!(
        guard
            .authorize(&request, RouteAccess::Private, true)
            .is_ok()
    );
    request.headers.remove("x-board-token");
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Private, true)
            .unwrap_err()
            .status,
        403
    );
}

#[test]
fn provided_foreign_get_origin_is_forbidden_even_with_correct_token() {
    let (_directory, guard) = fixture();
    let mut request = request(&guard, HttpMethod::Get, true);
    assert!(
        guard
            .authorize(&request, RouteAccess::Private, false)
            .is_ok()
    );
    request
        .headers
        .insert("origin".to_owned(), guard.origin().to_owned());
    assert!(
        guard
            .authorize(&request, RouteAccess::Private, false)
            .is_ok()
    );
    request
        .headers
        .insert("origin".to_owned(), "http://evil.example".to_owned());
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Private, false)
            .unwrap_err()
            .status,
        403
    );
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Public, false)
            .unwrap_err()
            .status,
        403
    );
}

#[test]
fn socket_parsed_post_reaches_same_auth_checks() {
    let directory = crate::board::board_test_support::scratch("web-guard-socket-");
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let guard = WebGuard::with_token(listener.local_addr().unwrap(), token).unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .write_all(
            format!(
                concat!(
                    "POST /api/v1/board HTTP/1.1\r\n",
                    "Host: {}\r\n",
                    "Origin: http://evil.example\r\n",
                    "X-Board-Token: {}\r\n",
                    "Content-Type: application/json\r\n",
                    "Content-Length: 2\r\n\r\n{{}}"
                ),
                guard.authority(),
                guard.token.expose(),
            )
            .as_bytes(),
        )
        .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let request =
        super::super::super::http_wire::read_request(&mut server, Instant::now()).unwrap();
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Private, true)
            .unwrap_err()
            .status,
        403
    );
}

#[test]
fn challenge_access_needs_no_token_but_keeps_transport_checks() {
    let (_directory, guard) = fixture();
    let mut request = request(&guard, HttpMethod::Post, false);
    request.target = "/api/v1/challenge".to_owned();
    assert!(
        guard
            .authorize(&request, RouteAccess::Challenge, true)
            .is_ok()
    );
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

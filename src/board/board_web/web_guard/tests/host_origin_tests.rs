use super::*;

#[test]
fn host_and_origin_comparisons_ignore_ascii_case() {
    let (_directory, guard) = fixture();
    let port = guard.authority().rsplit_once(':').unwrap().1;
    let mut request = request(&guard, HttpMethod::Get, true);
    for host in [format!("LOCALHOST:{port}"), format!("LocalHost:{port}")] {
        request.headers.insert("host".to_owned(), host.clone());
        assert!(
            guard
                .authorize(&request, RouteAccess::Private, false)
                .is_ok(),
            "{host}"
        );
        request
            .headers
            .insert("origin".to_owned(), format!("http://localhost:{port}"));
        assert!(
            guard
                .authorize(&request, RouteAccess::Private, false)
                .is_ok(),
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
    assert!(
        guard
            .authorize(&request, RouteAccess::Private, true)
            .is_ok()
    );
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
fn guards_actual_ephemeral_port_and_literal_loopback_authority() {
    let (_directory, guard) = fixture();
    let mut request = request(&guard, HttpMethod::Get, true);
    assert!(
        guard
            .authorize(&request, RouteAccess::Private, false)
            .is_ok()
    );
    for host in [
        "127.0.0.1:0",
        "localhost:1234",
        "example.com:1234",
        "127.0.0.1",
    ] {
        request.headers.insert("host".to_owned(), host.to_owned());
        assert_eq!(
            guard
                .authorize(&request, RouteAccess::Private, false)
                .unwrap_err()
                .status,
            421
        );
    }
    let directory = crate::board::board_test_support::scratch("web-guard-ipv6-");
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    let ipv6 = WebGuard::with_token("[::1]:32123".parse().unwrap(), token).unwrap();
    assert_eq!(ipv6.authority(), "[::1]:32123");
    assert_eq!(ipv6.origin(), "http://[::1]:32123");
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
        assert!(
            guard
                .authorize(&request, RouteAccess::Private, false)
                .is_ok()
        );
        request
            .headers
            .insert("origin".to_owned(), format!("http://{host}"));
        assert!(
            guard
                .authorize(&request, RouteAccess::Private, false)
                .is_ok()
        );
        request.method = HttpMethod::Post;
        request
            .headers
            .insert("content-type".to_owned(), "application/json".to_owned());
        assert!(
            guard
                .authorize(&request, RouteAccess::Private, true)
                .is_ok()
        );
        for other in aliases.iter().filter(|other| *other != host) {
            request
                .headers
                .insert("origin".to_owned(), format!("http://{other}"));
            assert_eq!(
                guard
                    .authorize(&request, RouteAccess::Private, true)
                    .unwrap_err()
                    .status,
                403
            );
            request.method = HttpMethod::Get;
            assert_eq!(
                guard
                    .authorize(&request, RouteAccess::Private, false)
                    .unwrap_err()
                    .status,
                403
            );
            request.method = HttpMethod::Post;
        }
    }
}

#[test]
fn default_http_port_uses_browser_normalized_authorities() {
    let directory = crate::board::board_test_support::scratch("web-guard-port80-");
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    let guard = WebGuard::with_token("127.0.0.1:80".parse().unwrap(), token).unwrap();
    assert_eq!(guard.origin(), "http://127.0.0.1");
    for host in ["127.0.0.1", "localhost", "[::1]"] {
        let mut request = request(&guard, HttpMethod::Post, true);
        request.headers.insert("host".to_owned(), host.to_owned());
        request
            .headers
            .insert("origin".to_owned(), format!("http://{host}"));
        assert!(
            guard
                .authorize(&request, RouteAccess::Private, true)
                .is_ok()
        );
    }
}

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
    let directory = tp_core::test_fixtures::scratch("web-guard-ipv6-");
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
    let directory = tp_core::test_fixtures::scratch("web-guard-port80-");
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

#[test]
fn lan_host_requires_an_enabled_listener_canonical_literal_and_exact_port() {
    let (_directory, loopback) = fixture();
    let lan_ip: std::net::Ipv4Addr = "192.168.7.19".parse().unwrap();
    let address = SocketAddr::new(lan_ip.into(), 32123);
    let directory = tp_core::test_fixtures::scratch("web-guard-lan-host-");
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    let specific = WebGuard::with_token(address, token).unwrap();

    let mut loopback_request = request(&loopback, HttpMethod::Get, true);
    loopback_request
        .headers
        .insert("host".to_owned(), "192.168.7.19:32123".to_owned());
    assert_eq!(
        loopback
            .authorize(&loopback_request, RouteAccess::Private, false)
            .unwrap_err()
            .status,
        421,
        "loopback-only listeners do not authorize a LAN Host"
    );

    let mut request = request(&specific, HttpMethod::Post, true);
    request
        .headers
        .insert("host".to_owned(), "192.168.7.19:32123".to_owned());
    request
        .headers
        .insert("origin".to_owned(), "http://192.168.7.19:32123".to_owned());
    assert!(
        specific
            .authorize(&request, RouteAccess::Private, true)
            .is_ok()
    );
    request
        .headers
        .insert("origin".to_owned(), "http://192.168.7.20:32123".to_owned());
    assert_eq!(
        specific
            .authorize(&request, RouteAccess::Private, true)
            .unwrap_err()
            .status,
        403,
        "POST Origin must exactly match Host"
    );

    for host in [
        "192.168.7.20:32123",
        "192.168.7.19:32124",
        "192.168.7.19",
        "192.168.7.19.attacker:32123",
        "192.168.7.19:32123/path",
        "192.168.007.019:32123",
        "192.167.7.19:32123",
        "192.169.7.19:32123",
        "192.168.example:32123",
        "127.0.0.1:0",
    ] {
        request.headers.insert("host".to_owned(), host.to_owned());
        request.headers.remove("origin");
        assert_eq!(
            specific
                .authorize(&request, RouteAccess::Private, true)
                .unwrap_err()
                .status,
            421,
            "{host}"
        );
    }
}

#[test]
fn wildcard_lan_hosts_are_restricted_to_canonical_192_168_literals() {
    let directory = tp_core::test_fixtures::scratch("web-guard-wildcard-host-");
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    let wildcard = WebGuard::with_token("0.0.0.0:32123".parse().unwrap(), token).unwrap();
    assert_eq!(wildcard.authority(), "127.0.0.1:32123");
    assert_eq!(wildcard.origin(), "http://127.0.0.1:32123");

    for host in ["192.168.0.1:32123", "192.168.255.254:32123"] {
        let mut request = request(&wildcard, HttpMethod::Get, true);
        request.headers.insert("host".to_owned(), host.to_owned());
        assert!(
            wildcard
                .authorize(&request, RouteAccess::Private, false)
                .is_ok(),
            "{host}"
        );
    }
    for host in [
        "192.167.255.255:32123",
        "192.169.0.0:32123",
        "10.0.0.1:32123",
        "172.16.0.1:32123",
        "192.168.1.1:32124",
        "192.168.1.1.attacker:32123",
        "192.168.1.1:32123/path",
    ] {
        let mut request = request(&wildcard, HttpMethod::Get, true);
        request.headers.insert("host".to_owned(), host.to_owned());
        assert_eq!(
            wildcard
                .authorize(&request, RouteAccess::Private, false)
                .unwrap_err()
                .status,
            421,
            "{host}"
        );
    }
}

#[test]
fn lan_authority_uses_the_canonical_port_eighty_host_form() {
    let directory = tp_core::test_fixtures::scratch("web-guard-lan-port80-");
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    let guard = WebGuard::with_token("192.168.3.4:80".parse().unwrap(), token).unwrap();
    assert_eq!(guard.origin(), "http://192.168.3.4");

    let mut request = request(&guard, HttpMethod::Get, true);
    request
        .headers
        .insert("host".to_owned(), "192.168.3.4".to_owned());
    assert!(
        guard
            .authorize(&request, RouteAccess::Private, false)
            .is_ok()
    );
    request
        .headers
        .insert("host".to_owned(), "192.168.3.4:80".to_owned());
    assert_eq!(
        guard
            .authorize(&request, RouteAccess::Private, false)
            .unwrap_err()
            .status,
        421
    );
}

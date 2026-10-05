use super::*;

#[test]
fn refuses_non_loopback_before_loading_any_live_token() {
    let directory = crate::board::board_test_support::scratch("web-loopback-");
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    for address in ["0.0.0.0:0", "[::]:0", "192.0.2.1:0", "127.0.0.2:0"] {
        assert!(
            super::super::super::web_endpoint::bind_listener(&runtime, address.parse().unwrap())
                .is_err()
        );
    }
    let scoped = std::net::SocketAddrV6::new(std::net::Ipv6Addr::LOCALHOST, 0, 0, 1);
    assert!(
        super::super::super::web_endpoint::bind_listener(&runtime, SocketAddr::V6(scoped)).is_err()
    );
    assert_eq!(
        std::fs::read_dir(&runtime).unwrap().count(),
        0,
        "a refused bind creates no runtime files"
    );
    let directory = crate::board::board_test_support::scratch("web-guard-port-zero-");
    let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
    assert!(WebGuard::with_token("127.0.0.1:0".parse().unwrap(), token).is_err());
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
